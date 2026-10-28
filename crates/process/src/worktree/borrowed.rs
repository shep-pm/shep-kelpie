//! The commits of other pull requests a head carries

use std::path::Path;
use std::process::Stdio;

use super::{WorktreeError, git, in_repo};

/// The `tips` that are ancestors of `head` and not of `origin/<base>`, fetched first
///
/// A tip git does not hold is not carried, since a head that carries a commit holds it.
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
        if held(repo, tip)? && is_ancestor(repo, tip, head)? && !is_ancestor(repo, tip, &tracking)?
        {
            carried.push(tip.clone());
        }
    }
    Ok(carried)
}

// `rev-parse --verify --quiet` answers a commit git lacks with exit 1.
fn held(repo: &Path, tip: &str) -> Result<bool, WorktreeError> {
    let object = format!("{tip}^{{commit}}");
    answer(repo, &["rev-parse", "--verify", "--quiet", &object])
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
