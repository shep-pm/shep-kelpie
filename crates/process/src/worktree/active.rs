//! Whether a worker is still at work in its worktree
//!
//! A turn that hits its ceiling may be stopped mid-task. The worktree says so:
//! a commit on the branch or a file written a moment ago. Kelpie writes
//! nothing into a worktree itself (a call's settings, instructions and
//! templates live in the worker and build folders), so what is there is the
//! worker's.

use std::path::Path;
use std::time::SystemTime;

use super::{WorktreeError, git, trusted, uncommitted};

/// Whether `worktree` holds a commit on its branch that `origin/{base}` lacks,
/// made at or after `since`, or an uncommitted file written at or after `since`
///
/// A file deleted since the last commit has no time to read and does not count.
/// A link is read as itself, not as what it points to.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn touched_since(
    repo: &Path,
    worktree: &Path,
    base: &str,
    since: SystemTime,
) -> Result<bool, WorktreeError> {
    // Read from the checkout: a clone's own `origin/main` is the worker's.
    let main = git(repo, ["rev-parse", "--verify", &format!("origin/{base}")])?;
    let range = format!("{main}..HEAD");
    let committed = trusted(repo, worktree)?(&["log", "-1", "--format=%ct", &range])?;
    let at_or_after = |time: SystemTime| time >= since;
    if let Ok(seconds) = committed.trim().parse::<u64>()
        && at_or_after(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
    {
        return Ok(true);
    }
    Ok(uncommitted(repo, worktree)?.iter().any(|file| {
        std::fs::symlink_metadata(worktree.join(file))
            .and_then(|meta| meta.modified())
            .is_ok_and(at_or_after)
    }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    use super::*;
    use shep_kelpie_test_support::{git, linked_worktree, write_in};

    const MINUTES: Duration = Duration::from_secs(60);

    // A repo whose `origin/main` is its first commit, and a worktree on its
    // own branch from there, in a home that outlives the test
    struct Rig {
        _home: tempfile::TempDir,
        repo: PathBuf,
        worktree: PathBuf,
    }

    impl Rig {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let (repo, worktree) = linked_worktree(&home.path().canonicalize().unwrap());
            git(&repo, &["update-ref", "refs/remotes/origin/main", "main"]);
            Self {
                _home: home,
                repo,
                worktree,
            }
        }

        // Commits `file` in the worktree as of `when`
        fn commit_at(&self, file: &str, when: SystemTime) {
            write_in(&self.worktree, file, "work\n");
            git(&self.worktree, &["add", file]);
            let seconds = when
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let status = Command::new("git")
                .args([
                    "-c",
                    "user.name=rig",
                    "-c",
                    "user.email=rig@example.invalid",
                ])
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "-m",
                    file,
                ])
                .current_dir(&self.worktree)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_COMMITTER_DATE", format!("@{seconds} +0000"))
                .status()
                .unwrap();
            assert!(status.success());
        }

        // Writes `file` in the worktree, last modified at `when`
        fn write_at(&self, file: &str, when: SystemTime) {
            write_in(&self.worktree, file, "work\n");
            let file = std::fs::File::options()
                .write(true)
                .open(self.worktree.join(file))
                .unwrap();
            file.set_modified(when).unwrap();
        }

        fn touched_since(&self, since: SystemTime) -> bool {
            touched_since(&self.repo, &self.worktree, "main", since).unwrap()
        }
    }

    #[test]
    fn a_commit_on_the_branch_inside_the_window_is_touched() {
        let rig = Rig::new();
        let now = SystemTime::now();
        rig.commit_at("done.rs", now - MINUTES);

        assert!(rig.touched_since(now - 10 * MINUTES));
    }

    #[test]
    fn a_file_written_inside_the_window_is_touched() {
        let rig = Rig::new();
        let now = SystemTime::now();
        rig.write_at("half.rs", now - MINUTES);

        assert!(rig.touched_since(now - 10 * MINUTES));
    }

    #[test]
    fn an_old_commit_and_an_old_file_are_not_touched() {
        let rig = Rig::new();
        let now = SystemTime::now();
        rig.commit_at("done.rs", now - 30 * MINUTES);
        rig.write_at("half.rs", now - 20 * MINUTES);

        assert!(!rig.touched_since(now - 10 * MINUTES));
    }

    #[test]
    fn a_commit_already_on_the_base_is_not_the_branchs() {
        let rig = Rig::new();
        let now = SystemTime::now();
        rig.commit_at("landed.rs", now - MINUTES);
        git(&rig.repo, &["update-ref", "refs/remotes/origin/main", "wt"]);

        assert!(!rig.touched_since(now - 10 * MINUTES));
    }
}
