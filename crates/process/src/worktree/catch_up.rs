//! Catching a work item's branch up with `origin/main`

use std::path::Path;

use super::{Remote, WorktreeError, clone, git, trusted};

/// What a rebase onto `origin/main` came to
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebase {
    /// Rebased or merged, and pushed: the branch's new head
    Pushed(String),
    /// The branch conflicts with `main`, and the merge of it is left in
    /// progress in the worktree for the worker to resolve: `main`'s other
    /// files are written and staged
    Conflicts {
        /// The `origin/main` commit it conflicts with
        main: String,
        /// The files that conflict
        files: Vec<String>,
    },
    /// Left as it was, for this reason, which only the maintainer can settle
    Refused(String),
}

/// Catches the worktree's branch at `head` up with `origin/main` and pushes it to `remote`
///
/// A branch with a merge commit, or whose commits are not kelpie's to
/// `rewrite`, is merged with `origin/main` and pushed without force. Any other
/// is rebased and pushed with a lease on `head`; a rebase that conflicts is
/// merged instead, and a merge that conflicts is left for the worker. A failed
/// push puts the branch back at `head`. Run [`super::base_of`] first.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn rebase(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    head: &str,
    rewrite: bool,
    remote: &Remote,
    base: &str,
) -> Result<Rebase, WorktreeError> {
    let in_worktree = trusted(repo, worktree)?;
    let full_ref = format!("refs/heads/{branch}");
    let on_branch = in_worktree(&["symbolic-ref", "--quiet", "HEAD"])
        .ok()
        .as_deref()
        == Some(&full_ref);
    if !on_branch || in_worktree(&["rev-parse", "HEAD"])? != head {
        let short = head.get(..7).unwrap_or(head);
        return Ok(Rebase::Refused(format!(
            "its worktree is not at the pull request's head {short}"
        )));
    }
    if in_worktree(&["rev-parse", "--quiet", "--verify", "MERGE_HEAD"]).is_ok() {
        return Ok(Rebase::Refused(
            "its worktree is in the middle of a merge".into(),
        ));
    }
    if !in_worktree(&["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Ok(Rebase::Refused(
            "its worktree has changes that are not committed".into(),
        ));
    }
    // The rebased commits and the merge commit take the head's committer, the
    // worker's one identity, so neither needs an identity of its own.
    let name = format!(
        "user.name={}",
        in_worktree(&["log", "-1", "--format=%cn", head])?
    );
    let email = format!(
        "user.email={}",
        in_worktree(&["log", "-1", "--format=%ce", head])?
    );
    let caught_up = |verb: &[&str]| {
        let mut args = vec!["-c", &name, "-c", &email];
        args.extend_from_slice(verb);
        in_worktree(&args)
    };
    let conflicted =
        || in_worktree(&["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
    let tip = format!("origin/{base}");
    clone::mirror(repo, worktree, &[&format!("refs/remotes/{tip}")])?;
    // A rebase replays the branch's own commits and drops its merge commits,
    // and with them the worker's hand resolution of an earlier conflict. A
    // branch holding one is caught up by merging instead, and pushed plain.
    let ahead = format!("{tip}..HEAD");
    let rebasing =
        rewrite && in_worktree(&["rev-list", "--merges", "--max-count=1", &ahead])?.is_empty();
    let mut rebased = false;
    if rebasing && let Err(e) = caught_up(&["rebase", "--quiet", &tip]) {
        let conflicts = conflicted();
        let aborted = in_worktree(&["rebase", "--abort"]);
        if conflicts.is_empty() {
            return Err(e);
        }
        aborted?;
    } else {
        rebased = rebasing;
    }
    if !rebased {
        if let Err(e) = caught_up(&["merge", "--quiet", "--no-commit", "--no-ff", &tip]) {
            let conflicts = conflicted();
            if conflicts.is_empty() {
                let _ = in_worktree(&["merge", "--abort"]);
                return Err(e);
            }
            return Ok(Rebase::Conflicts {
                main: git(repo, ["rev-parse", &tip])?,
                files: conflicts.lines().map(str::to_owned).collect(),
            });
        }
        if let Err(e) = caught_up(&["commit", "--quiet", "--no-edit"]) {
            // Best effort: a merge left in progress is refused on the next look.
            let _ = in_worktree(&["merge", "--abort"]);
            return Err(e);
        }
    }
    let caught = in_worktree(&["rev-parse", "HEAD"])?;
    let there = remote.branch_ref(branch);
    let lease = format!("--force-with-lease={there}:{head}");
    let target = format!("HEAD:{there}");
    let pushed = if rebased {
        in_worktree(&["push", "--quiet", &lease, "--", remote.target(), &target])
    } else {
        in_worktree(&["push", "--quiet", "--", remote.target(), &target])
    };
    if let Err(e) = pushed {
        // Best effort: a branch left off the head is refused on the next look.
        let _ = in_worktree(&["reset", "--quiet", "--hard", head]);
        return Err(e);
    }
    Ok(Rebase::Pushed(caught))
}
