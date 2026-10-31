//! A ref read from its loose file, which spares starting git where one is kept
//!
//! Git reads a ref's loose file before `packed-refs`, so where the file is,
//! it is the ref. Starting git costs tens of milliseconds on Windows, and the
//! board reads every work item's branch on each write.

use std::path::{Component, Path};

use super::{WorktreeError, common_dir, git};

/// The commit the loose ref `full_ref` in the git dir `git_dir` names, none where git must answer
///
/// A ref that is packed, symbolic, in a reftable, behind a link or absent
/// has no such file. A branch or remote-tracking ref names a commit, so the
/// id is not peeled.
pub fn loose_commit(git_dir: &Path, full_ref: &str) -> Option<String> {
    let path = Path::new(full_ref);
    let plain =
        path.starts_with("refs") && path.components().all(|c| matches!(c, Component::Normal(_)));
    if !plain {
        return None;
    }
    full_id(&file_text(&git_dir.join(path))?)
}

/// The commit a worktree's `HEAD` in its git dir `own` names, through its
/// branch in the common dir `common`, none where git must answer
pub(super) fn loose_head(own: &Path, common: &Path) -> Option<String> {
    let text = file_text(&own.join("HEAD"))?;
    match text.strip_prefix("ref: ") {
        Some(branch) => loose_commit(common, branch.strip_suffix('\n')?),
        None => full_id(&text),
    }
}

// What the regular file at `path` holds, none for a link or a file that is not there.
fn file_text(path: &Path) -> Option<String> {
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

// The object id a ref file holds, on a line of its own.
fn full_id(text: &str) -> Option<String> {
    let id = text.strip_suffix('\n')?;
    let full =
        matches!(id.len(), 40 | 64) && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    full.then(|| id.to_owned())
}

/// [`loose_commit`] in `repo`'s common dir, where its branches and
/// remote-tracking refs are kept
pub fn loose_commit_in(repo: &Path, full_ref: &str) -> Option<String> {
    loose_commit(&common_dir(repo).ok()?, full_ref)
}

/// The commit `full_ref` names in `repo`, from its loose file where it has one, else as git reads it
///
/// # Errors
///
/// [`WorktreeError`] when git cannot answer, as it cannot for an absent ref.
pub fn commit_of(repo: &Path, full_ref: &str) -> Result<String, WorktreeError> {
    match loose_commit_in(repo, full_ref) {
        Some(commit) => Ok(commit),
        None => git(repo, ["rev-parse", "--verify", "--quiet", full_ref]),
    }
}

#[cfg(test)]
mod tests {
    use shep_kelpie_test_support::git;

    use super::*;

    fn repo() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet", "-b", "main"]);
        git(
            dir.path(),
            &["commit", "--quiet", "--allow-empty", "-m", "first"],
        );
        let head = git(dir.path(), &["rev-parse", "HEAD"]);
        (dir, head)
    }

    #[test]
    fn a_loose_branch_reads_as_git_reads_it() {
        let (dir, head) = repo();
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", &head],
        );

        assert_eq!(
            loose_commit_in(dir.path(), "refs/heads/main"),
            Some(head.clone())
        );
        assert_eq!(
            loose_commit_in(dir.path(), "refs/remotes/origin/main"),
            Some(head)
        );
    }

    #[test]
    fn a_packed_symbolic_or_absent_ref_is_left_to_git() {
        let (dir, _) = repo();
        git(
            dir.path(),
            &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
        );
        git(dir.path(), &["pack-refs", "--all"]);

        assert_eq!(loose_commit_in(dir.path(), "refs/heads/main"), None);
        assert_eq!(loose_commit_in(dir.path(), "refs/heads/alias"), None);
        assert_eq!(loose_commit_in(dir.path(), "refs/heads/gone"), None);
    }

    #[test]
    fn a_ref_reads_loose_and_asks_git_for_a_packed_or_absent_one() {
        let (dir, head) = repo();
        git(dir.path(), &["branch", "packed"]);
        git(dir.path(), &["pack-refs", "--all"]);
        git(dir.path(), &["branch", "loose"]);

        assert_eq!(commit_of(dir.path(), "refs/heads/loose"), Ok(head.clone()));
        assert_eq!(commit_of(dir.path(), "refs/heads/packed"), Ok(head));
        let absent = commit_of(dir.path(), "refs/heads/gone").unwrap_err();
        assert!(
            matches!(&absent, WorktreeError::Git { args, .. } if args.ends_with("refs/heads/gone")),
            "{absent:?}"
        );
    }

    #[test]
    fn a_head_on_a_branch_or_detached_reads_as_git_reads_it() {
        let (dir, head) = repo();
        let git_dir = dir.path().join(".git");

        assert_eq!(loose_head(&git_dir, &git_dir), Some(head.clone()));
        git(dir.path(), &["checkout", "--quiet", "--detach"]);
        assert_eq!(loose_head(&git_dir, &git_dir), Some(head));
    }

    #[test]
    fn a_ref_reaching_out_of_the_git_dir_is_left_to_git() {
        let (dir, head) = repo();
        std::fs::write(dir.path().join(".git/outside"), format!("{head}\n")).unwrap();

        assert_eq!(loose_commit_in(dir.path(), "refs/../outside"), None);
        assert_eq!(loose_commit_in(dir.path(), "HEAD"), None);
    }
}
