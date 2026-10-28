//! The commits of other pull requests a head carries

use std::path::Path;
use std::process::Stdio;

use super::{WorktreeError, git, in_repo};

/// The `tips` whose history shares a commit with `head` that `origin/<base>` lacks, fetched first
///
/// A tip that has moved on since the head merged it still shares the commits it had. A tip git
/// does not hold after a fetch of it is not carried.
///
/// # Errors
///
/// [`WorktreeError`] naming the git command that failed.
pub fn borrowed(
    repo: &Path,
    head: &str,
    base: &str,
    tips: &[String],
) -> Result<Vec<String>, WorktreeError> {
    git(repo, ["fetch", "--quiet", "--", "origin", base])?;
    let tracking = format!("origin/{base}");
    let mut carried = Vec::new();
    for tip in tips {
        if held(repo, tip)? && shares_beyond(repo, head, tip, &tracking)? {
            carried.push(tip.clone());
        }
    }
    Ok(carried)
}

// A tip pushed after the clone last fetched is fetched by its commit, and a refusal leaves it
// unheld. `rev-parse --verify --quiet` answers a commit git lacks with exit 1.
fn held(repo: &Path, tip: &str) -> Result<bool, WorktreeError> {
    let object = format!("{tip}^{{commit}}");
    let rev_parse = ["rev-parse", "--verify", "--quiet", &object];
    if answer(repo, &rev_parse)? {
        return Ok(true);
    }
    let _ = in_repo(repo)
        .args(["fetch", "--quiet", "--", "origin", tip])
        .stdin(Stdio::null())
        .output();
    answer(repo, &rev_parse)
}

// Some commit both histories hold is not in `base`. The best common ancestors are the highest
// of those, and `base` holds everything below one it holds, so any of them outside it settles it.
fn shares_beyond(repo: &Path, head: &str, tip: &str, base: &str) -> Result<bool, WorktreeError> {
    let args = ["merge-base", "--all", head, tip];
    let output = in_repo(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| WorktreeError::Spawn(e.to_string()))?;
    match output.status.code() {
        // Exit 1 is no common ancestor at all
        Some(0 | 1) => {}
        _ => {
            return Err(WorktreeError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).into(),
            });
        }
    }
    for common in String::from_utf8_lossy(&output.stdout).lines() {
        if !is_ancestor(repo, common, base)? {
            return Ok(true);
        }
    }
    Ok(false)
}

// `--is-ancestor` answers no with exit 1, and fails with any other code.
fn is_ancestor(repo: &Path, older: &str, newer: &str) -> Result<bool, WorktreeError> {
    answer(repo, &["merge-base", "--is-ancestor", older, newer])
}

fn answer(repo: &Path, args: &[&str]) -> Result<bool, WorktreeError> {
    let output = in_repo(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| WorktreeError::Spawn(e.to_string()))?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(WorktreeError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        }),
    }
}
