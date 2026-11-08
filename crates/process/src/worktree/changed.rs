//! The files a branch changes from `main`, and what one commit changed since another

use std::path::Path;

use super::{WorktreeError, git, trusted};

/// The files `head` changes from `origin/main`, as the pull request shows them
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn changed_files(repo: &Path, head: &str, base: &str) -> Result<Vec<String>, WorktreeError> {
    let range = format!("origin/{base}...{head}");
    let names = git(repo, ["diff", "--name-only", "--no-renames", "-z", &range])?;
    Ok(names_of(&names))
}

/// What the commit `to` holds since the commit `from`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// How many commits `to` has that `from` does not
    pub commits: u32,
    /// The files those commits changed
    pub files: Vec<String>,
}

/// What `to` changed since `from`, read in `worktree`, whose clone holds the
/// commits its worker made
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed, which is what a
/// `from` the clone no longer holds gives.
pub fn moved_since(
    repo: &Path,
    worktree: &Path,
    from: &str,
    to: &str,
) -> Result<Moved, WorktreeError> {
    let git = trusted(repo, worktree)?;
    let range = format!("{from}..{to}");
    let args = ["rev-list", "--count", range.as_str()];
    let count = git(&args)?;
    let commits = match count.parse::<u32>() {
        Ok(n) => n,
        Err(_) => {
            return Err(WorktreeError::Git {
                args: args.join(" "),
                stderr: count,
            });
        }
    };
    let names = git(&["diff", "--name-only", "--no-renames", "-z", range.as_str()])?;
    Ok(Moved {
        commits,
        files: names_of(&names),
    })
}

// git's `-z` list of file names, in the order it gave them.
fn names_of(list: &str) -> Vec<String> {
    (list.split('\0'))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use shep_kelpie_test_support::{git as test_git, write_in};

    use super::*;
    use crate::worktree::{clone, head};

    // A checkout with one commit on `main`, and a clone of it at
    // `<dir>/worktree` on `kelpie/7`, holding one commit for each of `files`
    // on that branch. The tempdir, the checkout and the worktree.
    fn a_worktree(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        test_git(&repo, &["init", "--quiet", "-b", "main"]);
        write_in(&repo, "base.rs", "fn base() {}\n");
        test_git(&repo, &["add", "base.rs"]);
        test_git(&repo, &["commit", "--quiet", "-m", "first"]);
        let worktree = dir.path().join("worktree");
        clone::make(&repo, &worktree, "kelpie/7", "main").unwrap();
        for (file, text) in files {
            write_in(&worktree, file, text);
            test_git(&worktree, &["add", file]);
            test_git(&worktree, &["commit", "--quiet", "-m", file]);
        }
        (dir, repo, worktree)
    }

    #[test]
    fn moved_since_counts_the_commits_and_names_the_files() {
        let (_dir, repo, worktree) = a_worktree(&[("a.rs", "1\n"), ("b.rs", "2\n")]);
        let from = test_git(&repo, &["rev-parse", "main"]);
        let to = head(&repo, &worktree).unwrap();

        let moved = moved_since(&repo, &worktree, &from, &to).unwrap();

        assert_eq!(
            moved,
            Moved {
                commits: 2,
                files: vec!["a.rs".to_owned(), "b.rs".to_owned()],
            }
        );
    }

    #[test]
    fn a_from_the_clone_does_not_hold_is_an_error() {
        let (_dir, repo, worktree) = a_worktree(&[("a.rs", "1\n")]);
        let to = head(&repo, &worktree).unwrap();

        let moved = moved_since(
            &repo,
            &worktree,
            "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f",
            &to,
        );

        assert!(matches!(moved, Err(WorktreeError::Git { .. })), "{moved:?}");
    }
}
