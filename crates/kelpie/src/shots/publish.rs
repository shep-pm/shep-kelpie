//! Putting a shots run on its pull request, off the pull request's branch
//!
//! A forge takes no image through its API, so kelpie commits the PNGs to
//! `kelpie-shots/<number>` on `origin` and links them from one comment,
//! which each later run edits in place. The work item's own branch never
//! carries them.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use super::{ShotsRun, VARIANTS};
use crate::settings::ForgeSlug;

/// Opens the shots comment, so it reads as kelpie's own
pub const MARKER: &str = "<!-- kelpie-shots -->";

/// Where pull request `number`'s shots live on `origin`
pub fn branch(number: u64) -> String {
    format!("kelpie-shots/{number}")
}

/// Commits `run`'s PNGs, and nothing else, to `branch` on `origin`
///
/// Each commit's parent is the branch's last, so the push never forces.
/// Returns the new commit's hash.
///
/// # Errors
///
/// The git command that failed and what it said.
pub fn push(repo: &Path, branch: &str, run: &ShotsRun, message: &str) -> Result<String, String> {
    let mut tree = String::new();
    for file in run.files() {
        let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let blob = git(
            repo,
            &["hash-object", "-w", "--", &file.to_string_lossy()],
            None,
        )?;
        let _ = writeln!(tree, "100644 blob {blob}\t{name}");
    }
    let tree = git(repo, &["mktree"], Some(&tree))?;
    let remote = format!("refs/heads/{branch}");
    let parent = git(repo, &["fetch", "--quiet", "origin", &remote], None)
        .and_then(|_| {
            git(
                repo,
                &["rev-parse", "--verify", "--quiet", "FETCH_HEAD"],
                None,
            )
        })
        .ok();
    let mut args = vec!["commit-tree", &tree, "-m", message];
    if let Some(parent) = &parent {
        args.extend(["-p", parent]);
    }
    let commit = git(repo, &args, None)?;
    git(
        repo,
        &["push", "--quiet", "origin", &format!("{commit}:{remote}")],
        None,
    )?;
    Ok(commit)
}

/// Deletes `branch` from `origin`, if it is there
///
/// # Errors
///
/// The git command that failed and what it said.
pub fn delete(repo: &Path, branch: &str) -> Result<(), String> {
    let remote = format!("refs/heads/{branch}");
    let listed = git(repo, &["ls-remote", "origin", &remote], None)?;
    if listed.is_empty() {
        return Ok(());
    }
    git(
        repo,
        &["push", "--quiet", "origin", "--delete", &remote],
        None,
    )
    .map(drop)
}

/// The comment for `run`, taken of `head`, its images read from `commit`
pub fn comment(
    forge: &ForgeSlug,
    number: u64,
    head: &str,
    commit: Option<&str>,
    run: &ShotsRun,
) -> String {
    let short = head.get(..7).unwrap_or(head);
    let mut body = format!("{MARKER}\n");
    if let Some(reason) = &run.failed {
        let _ = writeln!(body, "Kelpie could not take shots of {short}: {reason}");
        return body;
    }
    let _ = writeln!(
        body,
        "Kelpie's shots of {short}. They live on the `{}` branch, not this pull request's, until it closes.\n",
        branch(number)
    );
    body.push_str("| Route |");
    for (viewport, scheme) in VARIANTS {
        let _ = write!(
            body,
            " {viewport:?}, {} |",
            format!("{scheme:?}").to_lowercase()
        );
    }
    body.push_str("\n| --- |");
    body.push_str(&" --- |".repeat(VARIANTS.len()));
    body.push('\n');
    for route in run.shots.chunks(VARIANTS.len()) {
        let Some(first) = route.first() else { continue };
        let _ = write!(body, "| `{}` |", first.route.as_str());
        for shot in route {
            let name = shot
                .file
                .as_deref()
                .and_then(|f| f.file_name())
                .and_then(|n| n.to_str());
            match (name, commit) {
                (Some(name), Some(commit)) => {
                    let url = format!(
                        "https://github.com/{}/blob/{commit}/{name}?raw=true",
                        forge.as_str()
                    );
                    let _ = write!(body, " <img src=\"{url}\" width=\"180\"> |");
                }
                _ => body.push_str(" none |"),
            }
        }
        body.push('\n');
    }
    let problems = run.all_problems();
    if !problems.is_empty() {
        body.push_str("\nWhat went wrong:\n\n");
        for problem in problems {
            // The page wrote some of this, so it is shown, never rendered.
            let _ = writeln!(body, "- `{}`", problem.replace('`', "'"));
        }
    }
    body
}

// Git in the project's own checkout, with hooks off and kelpie as the committer.
fn git(repo: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=kelpie"])
        .args([
            "-c",
            "user.email=kelpie@localhost",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(text.as_bytes())
            .map_err(|e| format!("cannot feed git {}: {e}", args[0]))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
