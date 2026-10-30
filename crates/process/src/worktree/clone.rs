//! A work item's clone: a git dir of its own beside the worktree, borrowing the checkout's objects
//!
//! The clone's git dir is the sibling `<worktree>.git`. Its refs, index and
//! config are its own, and its `objects/info/alternates` names the checkout's
//! objects, so nothing is fetched and a commit the worker makes lands in the
//! clone alone. Kelpie's git reads a clone through [`super::trusted`], which
//! reads the clone's `config`: the worker must not be able to write it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{
    WorktreeError, canonical, common_dir, git, git_dirs, has_ref, in_repo, loose_commit,
    loose_commit_in, output, own_git_dir, trusted,
};

/// The clone's git dir for `worktree`: the sibling `<worktree>.git`
pub fn git_dir_of(worktree: &Path) -> PathBuf {
    let mut name = worktree.as_os_str().to_owned();
    name.push(".git");
    PathBuf::from(name)
}

/// The clone's git dir twice (own and common) when `worktree` is a clone of
/// the repo whose common dir is `common`, and none when it is not a clone
///
/// # Errors
///
/// [`WorktreeError::Foreign`] when the clone borrows objects from anywhere but the repo.
pub(super) fn dirs(
    common: &Path,
    worktree: &Path,
) -> Result<Option<(PathBuf, PathBuf)>, WorktreeError> {
    let own = git_dir_of(worktree);
    if !own.join("HEAD").is_file() {
        return Ok(None);
    }
    let borrowed = std::fs::read_to_string(own.join("objects/info/alternates"))
        .map_err(|_| WorktreeError::Foreign(worktree.to_owned()))?;
    let want = canonical(&common.join("objects"));
    let objects = own.join("objects");
    let mut lines = borrowed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .peekable();
    if lines.peek().is_none() || lines.any(|line| canonical(&objects.join(line)) != want) {
        return Err(WorktreeError::Foreign(worktree.to_owned()));
    }
    let own = canonical(&own);
    Ok(Some((own.clone(), own)))
}

/// The object folder of `worktree`'s clone, when `worktree` is a clone of `repo`
///
/// A reader of the checkout names it as an alternate to see the commits the
/// worker made, which sit in the clone alone.
pub fn objects_of(repo: &Path, worktree: &Path) -> Option<PathBuf> {
    let common = common_dir(repo).ok()?;
    let (own, _) = dirs(&common, worktree).ok()??;
    Some(own.join("objects"))
}

/// The commit `refs/heads/<branch>` is at for the work item whose worktree is `worktree`
///
/// Read from the clone, since a clone holds its branch and the checkout does
/// not: its loose ref where it has one, else through its trusted git. A
/// worktree that is no clone, or is gone, is read in the checkout, which a
/// linked worktree shares its branch with.
pub fn branch_head(repo: &Path, worktree: &Path, branch: &str) -> Option<String> {
    let full_ref = format!("refs/heads/{branch}");
    let common =
        git_dirs(repo, worktree).map_or_else(|_| common_dir(repo), |(_, common)| Ok(common));
    if let Some(commit) = common
        .ok()
        .and_then(|common| loose_commit(&common, &full_ref))
    {
        return Some(commit);
    }
    let peeled = format!("{full_ref}^{{commit}}");
    let args = ["rev-parse", "--verify", "--quiet", &peeled];
    match trusted(repo, worktree) {
        Ok(in_worktree) => in_worktree(&args).ok(),
        Err(_) => git(repo, args).ok(),
    }
}

/// The commit `refs/heads/<branch>` is at in the git dir `worktree` left behind, when it left one
///
/// A removal cut short, or a worktree folder deleted by hand, leaves the
/// clone's git dir and the worker's commits in it.
///
/// # Errors
///
/// [`WorktreeError::Foreign`] when the git dir borrows objects from anywhere but the repo.
pub(super) fn leftover(
    repo: &Path,
    worktree: &Path,
    branch: &str,
) -> Result<Option<String>, WorktreeError> {
    let Some((own, _)) = dirs(&common_dir(repo)?, worktree)? else {
        return Ok(None);
    };
    let full_ref = format!("refs/heads/{branch}");
    Ok(in_clone(&own, &["rev-parse", "--verify", "--quiet", &full_ref]).ok())
}

/// Refuses to start over a git dir `worktree` left behind that holds work
///
/// [`make`] resets the branch to `start_ref`, so a leftover branch anywhere
/// else holds commits it would throw away.
///
/// # Errors
///
/// [`WorktreeError::BranchTaken`] when the leftover branch is not at `start_ref`.
pub(super) fn keep_work_at(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    start_ref: &str,
) -> Result<(), WorktreeError> {
    let Some(left) = leftover(repo, worktree, branch)? else {
        return Ok(());
    };
    let start = git(
        repo,
        ["rev-parse", "--verify", &format!("{start_ref}^{{commit}}")],
    )?;
    if start == left {
        Ok(())
    } else {
        Err(WorktreeError::BranchTaken(branch.to_owned()))
    }
}

/// Finishes the checkout of a clone that [`make`] was cut short in
///
/// A clone has an index once its files are checked out. One without, in a
/// folder holding only the link, gets its files now. A folder holding more is
/// not kelpie's to overwrite.
///
/// # Errors
///
/// [`WorktreeError::Foreign`] for a folder holding files and no index, and
/// [`WorktreeError`] naming the git command that failed.
pub(super) fn complete(repo: &Path, worktree: &Path) -> Result<(), WorktreeError> {
    if git_dir_of(worktree).join("index").is_file() {
        return Ok(());
    }
    let only_the_link = std::fs::read_dir(worktree)
        .map_err(|_| WorktreeError::Foreign(worktree.to_owned()))?
        .all(|entry| entry.is_ok_and(|entry| entry.file_name() == ".git"));
    if !only_the_link {
        return Err(WorktreeError::Foreign(worktree.to_owned()));
    }
    trusted(repo, worktree)?(&["reset", "--quiet", "--hard", "HEAD"]).map(drop)
}

/// Makes `worktree` a clone of `repo` on `branch`, started at the commit `at`
///
/// `worktree` holds nothing yet. The clone borrows the checkout's objects and
/// copies its `remote.origin.url`, `remote.origin.pushurl` and
/// `core.hooksPath`, and holds no ref but `branch`.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command or folder that failed.
pub fn make(repo: &Path, worktree: &Path, branch: &str, at: &str) -> Result<(), WorktreeError> {
    let git_dir = git_dir_of(worktree);
    let commit = git(repo, ["rev-parse", "--verify", &format!("{at}^{{commit}}")])?;
    skeleton(repo, &git_dir)?;
    let full_ref = format!("refs/heads/{branch}");
    in_clone(&git_dir, &["symbolic-ref", "HEAD", &full_ref])?;
    in_clone(&git_dir, &["update-ref", &full_ref, &commit])?;
    link(worktree, &git_dir)?;
    trusted(repo, worktree)?(&["reset", "--quiet", "--hard", "HEAD"]).map(drop)
}

// A git dir that is bare to git's init and not after it, with the checkout's
// objects borrowed and the settings a push needs copied.
fn skeleton(repo: &Path, git_dir: &Path) -> Result<(), WorktreeError> {
    let parent = git_dir.parent().unwrap_or(git_dir);
    create(parent)?;
    let args: [&OsStr; 5] = [
        "init".as_ref(),
        "--quiet".as_ref(),
        "--bare".as_ref(),
        "--".as_ref(),
        git_dir.as_os_str(),
    ];
    git(parent, args)?;
    in_clone(git_dir, &["config", "core.bare", "false"])?;
    let objects = common_dir(repo)?.join("objects");
    write(
        &git_dir.join("objects/info/alternates"),
        &format!("{}\n", objects.display()),
    )?;
    create(&git_dir.join("modules"))?;
    for key in COPIED {
        if let Ok(value) = git(repo, ["config", "--local", "--get", key]) {
            in_clone(git_dir, &["config", key, &value])?;
        }
    }
    Ok(())
}

// The checkout's settings a clone keeps.
const COPIED: [&str; 3] = [
    "remote.origin.url",
    "remote.origin.pushurl",
    "core.hooksPath",
];

// The worktree's `.git` file, which names the clone's git dir.
fn link(worktree: &Path, git_dir: &Path) -> Result<(), WorktreeError> {
    create(worktree)?;
    write(
        &worktree.join(".git"),
        &format!("gitdir: {}\n", git_dir.display()),
    )
}

fn create(path: &Path) -> Result<(), WorktreeError> {
    std::fs::create_dir_all(path).map_err(|e| WorktreeError::Folder {
        path: path.to_owned(),
        kind: e.kind(),
    })
}

fn write(path: &Path, text: &str) -> Result<(), WorktreeError> {
    std::fs::write(path, text).map_err(|e| WorktreeError::Folder {
        path: path.to_owned(),
        kind: e.kind(),
    })
}

// `git` on a clone's git dir before its worktree is linked.
pub(super) fn in_clone(git_dir: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let mut command = in_repo(git_dir.parent().unwrap_or(git_dir));
    command.arg("--git-dir").arg(git_dir);
    output(command, args)
}

/// Sets each ref in `refs` in the clone to the commit the checkout holds it at
///
/// A worktree that is not a clone shares the checkout's refs, so it is left
/// as it is.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn mirror(repo: &Path, worktree: &Path, refs: &[&str]) -> Result<(), WorktreeError> {
    if !git_dir_of(worktree).join("HEAD").is_file() {
        return Ok(());
    }
    let in_worktree = trusted(repo, worktree)?;
    for name in refs {
        let commit = match loose_commit_in(repo, name) {
            Some(commit) => commit,
            None => git(
                repo,
                ["rev-parse", "--verify", &format!("{name}^{{commit}}")],
            )?,
        };
        in_worktree(&["update-ref", "--no-deref", name, &commit])?;
    }
    Ok(())
}

/// Turns the linked worktree `worktree` of `repo` into a clone
///
/// The clone takes the files of the worktree's folder under the common dir
/// but `gitdir` and `commondir`: its HEAD, index and a merge in progress. It
/// takes the branch's commit too, and the folder is removed. The checkout
/// keeps `refs/heads/<branch>`, so its gc keeps what the clone borrows.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command or folder that failed.
pub fn convert(repo: &Path, worktree: &Path, branch: &str) -> Result<(), WorktreeError> {
    let common = common_dir(repo)?;
    let Some(own) = own_git_dir(&common, worktree) else {
        return if git_dir_of(worktree).join("HEAD").is_file() {
            Ok(())
        } else {
            Err(WorktreeError::Foreign(worktree.to_owned()))
        };
    };
    let git_dir = git_dir_of(worktree);
    skeleton(repo, &git_dir)?;
    copy_into(&own, &git_dir)?;
    let full_ref = format!("refs/heads/{branch}");
    if has_ref(repo, &full_ref)? {
        let commit = git(repo, ["rev-parse", "--verify", &full_ref])?;
        in_clone(&git_dir, &["update-ref", &full_ref, &commit])?;
    }
    link(worktree, &git_dir)?;
    std::fs::remove_dir_all(&own).map_err(|e| WorktreeError::Remove {
        path: own,
        kind: e.kind(),
    })
}

// Copies what `from` holds into `to` but the two files that tie a linked
// worktree to its repo. A link is left behind: the worker wrote `from`, and
// copying through one would hand it a file it cannot read.
fn copy_into(from: &Path, to: &Path) -> Result<(), WorktreeError> {
    let unreadable = |path: &Path, e: std::io::Error| WorktreeError::Unreadable {
        path: path.to_owned(),
        kind: e.kind(),
    };
    for entry in std::fs::read_dir(from).map_err(|e| unreadable(from, e))? {
        let entry = entry.map_err(|e| unreadable(from, e))?;
        let name = entry.file_name();
        if name == "gitdir" || name == "commondir" {
            continue;
        }
        let target = to.join(&name);
        let kind = entry.file_type().map_err(|e| unreadable(from, e))?;
        if kind.is_dir() {
            create(&target)?;
            copy_into(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target).map_err(|e| unreadable(&entry.path(), e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use super::*;
    use crate::worktree::{Rebase, Remote, holds_work, rebase, touched_since};
    use shep_kelpie_test_support::{git, main_commits, write_in};

    // A repo checkout with `a.txt` committed on `main`, and a path for a
    // worktree that does not exist yet
    struct Rig {
        _home: tempfile::TempDir,
        repo: PathBuf,
        worktree: PathBuf,
    }

    impl Rig {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let root = home.path().canonicalize().unwrap();
            let repo = root.join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "--quiet", "-b", "main"]);
            write_in(&repo, "a.txt", "a\n");
            git(&repo, &["add", "a.txt"]);
            git(&repo, &["commit", "--quiet", "-m", "first"]);
            git(&repo, &["branch", "other"]);
            Self {
                worktree: root.join("wt"),
                repo,
                _home: home,
            }
        }

        fn make(&self) {
            make(&self.repo, &self.worktree, "kelpie/7", "main").unwrap();
        }

        fn in_clone(&self, args: &[&str]) -> String {
            trusted(&self.repo, &self.worktree).unwrap()(args).unwrap()
        }

        // A commit in the worktree, as the worker makes one
        fn commit(&self, file: &str) -> String {
            write_in(&self.worktree, file, "work\n");
            self.in_clone(&["add", file]);
            self.in_clone(&[
                "-c",
                "user.name=rig",
                "-c",
                "user.email=rig@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                file,
            ]);
            self.in_clone(&["rev-parse", "HEAD"])
        }

        // An `origin` the checkout fetches from and pushes to, holding `main`
        fn with_origin(&self) {
            let origin = self.repo.parent().unwrap().join("origin.git");
            git(
                &self.repo,
                &[
                    "init",
                    "--quiet",
                    "--bare",
                    "-b",
                    "main",
                    origin.to_str().unwrap(),
                ],
            );
            git(
                &self.repo,
                &["remote", "add", "origin", origin.to_str().unwrap()],
            );
            git(&self.repo, &["push", "--quiet", "origin", "main"]);
            git(&self.repo, &["fetch", "--quiet", "origin"]);
        }
    }

    #[test]
    fn a_clone_reads_the_checkouts_objects_and_holds_no_other_branch() {
        let rig = Rig::new();
        let first = git(&rig.repo, &["rev-parse", "main"]);
        rig.make();
        write_in(&rig.repo, "b.txt", "b\n");
        git(&rig.repo, &["add", "b.txt"]);
        git(&rig.repo, &["commit", "--quiet", "-m", "later"]);
        let later = git(&rig.repo, &["rev-parse", "main"]);

        assert_eq!(
            std::fs::read_to_string(rig.worktree.join("a.txt")).unwrap(),
            "a\n"
        );
        assert_eq!(rig.in_clone(&["cat-file", "-t", &later]), "commit");
        assert_eq!(
            rig.in_clone(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            format!("refs/heads/kelpie/7 {first}")
        );
        assert_eq!(
            rig.in_clone(&["symbolic-ref", "HEAD"]),
            "refs/heads/kelpie/7"
        );
    }

    #[test]
    fn a_commit_in_a_clone_moves_its_own_branch_and_no_ref_of_the_checkout() {
        let rig = Rig::new();
        rig.make();
        let refs = || git(&rig.repo, &["for-each-ref"]);
        let before = refs();

        let work = rig.commit("b.txt");

        assert_eq!(rig.in_clone(&["rev-parse", "refs/heads/kelpie/7"]), work);
        assert_eq!(refs(), before);
        assert!(crate::worktree::git(&rig.repo, ["cat-file", "-e", &work]).is_err());
    }

    #[test]
    fn a_clones_origin_is_the_checkouts_origin_url() {
        let rig = Rig::new();
        git(
            &rig.repo,
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/pull.git",
            ],
        );
        git(
            &rig.repo,
            &[
                "config",
                "remote.origin.pushurl",
                "https://example.invalid/push.git",
            ],
        );
        git(&rig.repo, &["config", "core.hooksPath", ".githooks"]);
        git(&rig.repo, &["config", "user.name", "someone else"]);
        rig.make();

        let config = |key: &str| rig.in_clone(&["config", "--get", key]);
        assert_eq!(
            config("remote.origin.url"),
            "https://example.invalid/pull.git"
        );
        assert_eq!(
            config("remote.origin.pushurl"),
            "https://example.invalid/push.git"
        );
        assert_eq!(config("core.hooksPath"), "/dev/null");
        assert_eq!(
            git(
                &git_dir_of(&rig.worktree),
                &["config", "--get", "core.hooksPath"]
            ),
            ".githooks"
        );
        assert_eq!(config("core.bare"), "false");
        let own = std::fs::read_to_string(git_dir_of(&rig.worktree).join("config")).unwrap();
        assert!(!own.contains("someone else"), "{own}");
    }

    #[test]
    fn a_converted_worktree_keeps_its_branch_its_uncommitted_files_and_a_merge_in_progress() {
        let rig = Rig::new();
        let at = rig.worktree.to_str().unwrap();
        git(
            &rig.repo,
            &["worktree", "add", "--quiet", "-b", "kelpie/7", at],
        );
        write_in(&rig.worktree, "a.txt", "mine\n");
        git(&rig.worktree, &["commit", "--quiet", "-am", "mine"]);
        let mine = git(&rig.worktree, &["rev-parse", "HEAD"]);
        write_in(&rig.repo, "a.txt", "theirs\n");
        git(&rig.repo, &["commit", "--quiet", "-am", "theirs"]);
        let merge = [
            "-c",
            "user.name=rig",
            "-c",
            "user.email=rig@example.invalid",
            "merge",
            "--no-commit",
            "main",
        ];
        assert!(crate::worktree::git(&rig.worktree, merge).is_err());
        write_in(&rig.worktree, "new.txt", "new\n");

        convert(&rig.repo, &rig.worktree, "kelpie/7").unwrap();

        assert!(!git(&rig.repo, &["worktree", "list", "--porcelain"]).contains(at));
        assert!(!rig.repo.join(".git/worktrees/wt").exists());
        assert_eq!(
            rig.in_clone(&["symbolic-ref", "HEAD"]),
            "refs/heads/kelpie/7"
        );
        assert_eq!(rig.in_clone(&["rev-parse", "HEAD"]), mine);
        assert_eq!(git(&rig.repo, &["rev-parse", "refs/heads/kelpie/7"]), mine);
        assert_eq!(
            rig.in_clone(&["rev-parse", "MERGE_HEAD"]),
            git(&rig.repo, &["rev-parse", "main"])
        );
        let mut files = crate::worktree::uncommitted(&rig.repo, &rig.worktree).unwrap();
        files.sort();
        assert_eq!(files, ["a.txt", "new.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_linked_worktrees_git_dir_is_not_copied_into_the_clone() {
        let rig = Rig::new();
        let at = rig.worktree.to_str().unwrap();
        git(
            &rig.repo,
            &["worktree", "add", "--quiet", "-b", "kelpie/7", at],
        );
        let secret = rig.repo.parent().unwrap().join("secret");
        std::fs::write(&secret, "secret\n").unwrap();
        let own = rig.repo.join(".git/worktrees/wt");
        std::os::unix::fs::symlink(&secret, own.join("loot")).unwrap();

        convert(&rig.repo, &rig.worktree, "kelpie/7").unwrap();

        assert!(
            git_dir_of(&rig.worktree)
                .join("loot")
                .symlink_metadata()
                .is_err()
        );
    }

    #[test]
    fn holds_work_sees_a_commit_only_the_clone_holds() {
        let rig = Rig::new();
        rig.with_origin();
        rig.make();
        assert!(!holds_work(&rig.repo, &rig.worktree, "kelpie/7", "main").unwrap());

        rig.commit("b.txt");

        assert!(holds_work(&rig.repo, &rig.worktree, "kelpie/7", "main").unwrap());
    }

    #[test]
    fn a_mirrored_ref_the_worker_made_a_link_to_its_branch_leaves_the_branch_alone() {
        let rig = Rig::new();
        rig.make();
        git(
            &rig.repo,
            &["update-ref", "refs/remotes/origin/main", "main"],
        );
        let work = rig.commit("b.txt");
        rig.in_clone(&[
            "symbolic-ref",
            "refs/remotes/origin/main",
            "refs/heads/kelpie/7",
        ]);

        mirror(&rig.repo, &rig.worktree, &["refs/remotes/origin/main"]).unwrap();

        assert_eq!(rig.in_clone(&["rev-parse", "refs/heads/kelpie/7"]), work);
        assert_eq!(
            rig.in_clone(&["rev-parse", "refs/remotes/origin/main"]),
            git(&rig.repo, &["rev-parse", "origin/main"])
        );
    }

    #[test]
    fn kelpie_reads_main_from_the_checkout_whatever_the_clone_says() {
        let rig = Rig::new();
        rig.with_origin();
        rig.make();
        let work = rig.commit("b.txt");
        rig.in_clone(&["push", "--quiet", "origin", "HEAD:refs/heads/kelpie/7"]);
        rig.in_clone(&["update-ref", "refs/remotes/origin/main", &work]);

        assert!(touched_since(&rig.repo, &rig.worktree, "main", SystemTime::UNIX_EPOCH).unwrap());

        main_commits(&rig.repo, "main.txt", "main\n");
        let main = git(&rig.repo, &["rev-parse", "origin/main"]);
        let rebased = rebase(
            &rig.repo,
            &rig.worktree,
            "kelpie/7",
            &work,
            true,
            &Remote::Origin,
            "main",
        )
        .unwrap();

        let Rebase::Pushed(caught) = rebased else {
            panic!("expected a push, got {rebased:?}");
        };
        assert_eq!(rig.in_clone(&["rev-parse", &format!("{caught}^")]), main);
    }
}
