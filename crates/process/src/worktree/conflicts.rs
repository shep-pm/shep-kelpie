//! Whether a head merges into the base without conflicts

use std::path::Path;
use std::process::Stdio;

use super::in_repo;

/// Whether merging `head` into `origin/<base>` has conflicts, which git says
/// without touching the checkout. [`super::base_of`] has just fetched both.
///
/// # Errors
///
/// The reason git could not judge: it failed to run or to merge, or said
/// nothing of `head`.
pub fn merge_conflicts(checkout: &Path, head: &str, base: &str) -> Result<bool, String> {
    let tip = format!("origin/{base}");
    let args = ["merge-tree", "--write-tree", "--no-messages", &tip, head];
    let output = in_repo(checkout)
        .arg("--no-replace-objects")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    // Exit 1 with a tree first is a merge with conflicts. A missing commit
    // also exits 1, with nothing on stdout, and git before 2.38 fails otherwise.
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => {
            let tree = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            match tree.is_empty() || !tree.bytes().all(|b| b.is_ascii_hexdigit()) {
                true => Err(format!("git merge-tree said nothing of {head}")),
                false => Ok(true),
            }
        }
        _ => Err(format!(
            "git merge-tree failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )),
    }
}
