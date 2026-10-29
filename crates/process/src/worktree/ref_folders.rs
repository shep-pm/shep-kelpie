//! The folders a branch's refs and reflogs sit in
//!
//! Git removes a folder under `refs/heads/` when the last loose ref in it is
//! deleted, as it does for `kelpie/` once a merged work item's branch is
//! gone and the others are packed. The worker's fence lets it write
//! `refs/heads/kelpie/<n>` but not make the folder that holds it, so its next
//! commit would fail. Kelpie writes as the maintainer and makes the folders
//! before it hands a worktree over, and again after it deletes a branch.

use std::path::Path;

use super::{WorktreeError, create};

/// Makes the folders `branch`'s refs and reflogs go in, under `common`
///
/// Only folders are made, never the refs.
pub(super) fn ensure(common: &Path, branch: &str) -> Result<(), WorktreeError> {
    for root in [
        "refs/heads",
        "refs/remotes/origin",
        "logs/refs/heads",
        "logs/refs/remotes/origin",
    ] {
        if let Some(parent) = common.join(root).join(branch).parent() {
            create(parent)?;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use crate::worktree::{Remote, Start, clone, prepare, remove};
    use shep_kelpie_test_support::git;

    // A clone of an origin holding `main`, with `kelpie/` folders to be
    // emptied the way the cleanup of a merged branch empties them.
    struct Clone {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl Clone {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            git(
                &root,
                &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
            );
            git(&root, &["clone", "--quiet", "origin.git", "repo"]);
            let clone = Self { _dir: dir, root };
            git(
                &clone.repo(),
                &["commit", "--quiet", "--allow-empty", "-m", "init"],
            );
            git(&clone.repo(), &["push", "--quiet", "origin", "main"]);
            clone
        }

        fn repo(&self) -> PathBuf {
            self.root.join("repo")
        }

        fn worktree(&self) -> PathBuf {
            self.root.join("wt")
        }

        fn prepare(&self) {
            prepare(
                &self.repo(),
                &self.worktree(),
                "kelpie/7",
                Start::Main,
                "main",
                &Remote::Origin,
                &self.root.join("build"),
            )
            .unwrap();
        }

        fn git_dir(&self) -> PathBuf {
            self.repo().join(".git")
        }

        // What git does when the last loose ref under `kelpie/` is deleted.
        fn lose_branch_folders(&self) {
            git(&self.repo(), &["pack-refs", "--all"]);
            for root in ["refs/heads", "logs/refs/heads"] {
                let _ = fs::remove_dir_all(self.git_dir().join(root).join("kelpie"));
            }
        }

        // A commit in the worktree as the worker's fence allows it: the
        // `refs/heads` and `logs/refs/heads` folders take no new folder.
        fn worker_commits(&self) -> bool {
            let fenced = ["refs/heads", "logs/refs/heads"].map(|r| self.git_dir().join(r));
            for dir in &fenced {
                set_mode(dir, 0o555);
            }
            let committed = Command::new("git")
                .args([
                    "-c",
                    "user.name=rig",
                    "-c",
                    "user.email=rig@example.invalid",
                ])
                .args(["-c", "commit.gpgsign=false"])
                .args(["commit", "--quiet", "--allow-empty", "-m", "work"])
                .current_dir(self.worktree())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .unwrap()
                .success();
            for dir in &fenced {
                set_mode(dir, 0o755);
            }
            committed
        }

        // The branch lives in the worktree's clone.
        fn branch_head(&self) -> String {
            git(
                &clone::git_dir_of(&self.worktree()),
                &["rev-parse", "refs/heads/kelpie/7"],
            )
        }
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_commit_in_an_existing_worktree_updates_its_branch_after_the_folder_was_cleaned_up() {
        let clone = Clone::new();
        clone.prepare();
        clone.lose_branch_folders();

        clone.prepare();

        assert!(clone.git_dir().join("refs/heads/kelpie").is_dir());
        let before = clone.branch_head();
        assert!(clone.worker_commits());
        assert_ne!(clone.branch_head(), before);
    }

    #[test]
    fn a_commit_in_a_worktree_prepare_creates_updates_its_branch_after_the_folder_was_cleaned_up() {
        let clone = Clone::new();
        git(&clone.repo(), &["branch", "kelpie/6"]);
        clone.lose_branch_folders();

        clone.prepare();

        assert!(clone.git_dir().join("refs/heads/kelpie").is_dir());
        let before = clone.branch_head();
        assert!(clone.worker_commits());
        assert_ne!(clone.branch_head(), before);
    }

    #[test]
    fn removing_a_work_item_leaves_the_folders_other_workers_commit_through() {
        let clone = Clone::new();
        clone.prepare();

        remove(
            &clone.repo(),
            &clone.worktree(),
            "kelpie/7",
            &clone.root.join("build"),
            None,
        )
        .unwrap();

        for root in ["refs/heads", "logs/refs/heads"] {
            assert!(clone.git_dir().join(root).join("kelpie").is_dir());
        }
    }

    #[test]
    fn a_removal_that_cannot_reach_origin_still_leaves_the_folders() {
        let clone = Clone::new();
        clone.prepare();
        git(
            &clone.repo(),
            &["remote", "set-url", "origin", "/nonexistent/origin.git"],
        );

        let removed = remove(
            &clone.repo(),
            &clone.worktree(),
            "kelpie/7",
            &clone.root.join("build"),
            Some(&Remote::Origin),
        );

        assert!(removed.is_err());
        for root in ["refs/heads", "logs/refs/heads"] {
            assert!(clone.git_dir().join(root).join("kelpie").is_dir());
        }
    }
}
