//! A worker's commits, pushed to a contributor's branch
//!
//! The worker only commits: its credential is scoped to the repo and cannot
//! reach a fork, and the maintainer's must never be in its reach. So kelpie
//! pushes from outside the sandbox, by URL, with whatever login git there
//! holds for the fork, as [`remove`](super::remove) deletes a fork's branch.

use std::path::Path;

use super::{Remote, WorktreeError, head, trusted};

/// Pushes the commit the worktree has checked out to `remote`'s branch
///
/// The push is by commit, fast-forward only, and never forced, so a branch
/// that holds anything the commit lacks is refused. A branch that is gone is
/// not created again: the caller fetches the branch first, and a fetch of a
/// deleted one fails. Git is the worktree's trusted one, which reads no hook
/// and no config but the worktree's git dir and the machine's.
///
/// # Errors
///
/// [`WorktreeError`] with what git printed when it cannot read the worktree's
/// head, or `remote` refuses the push. `remote` being `Origin` is refused too,
/// since a repo's own branch is the worker's to push.
pub fn push_commits(repo: &Path, worktree: &Path, remote: &Remote) -> Result<(), WorktreeError> {
    let Remote::Fork { url, branch } = remote else {
        return Err(WorktreeError::Git {
            args: "push".into(),
            stderr: "a branch on origin is pushed by its worker".into(),
        });
    };
    let sha = head(repo, worktree)?;
    let spec = format!("{sha}:refs/heads/{branch}");
    // A clone's commits are not in the checkout.
    trusted(repo, worktree)?(&["push", "--quiet", "--no-verify", "--", url, &spec]).map(drop)
}
