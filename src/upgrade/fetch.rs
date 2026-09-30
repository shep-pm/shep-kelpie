//! Getting a build to install: a release's download, or a build of a git ref
//!
//! Both leave the binary in a folder under kelpie's home and hand back its
//! path. Nothing here touches the installed build.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The repo a release or a ref comes from, unless `KELPIE_SOURCE` names another
pub const REPO: &str = "https://github.com/shep-pm/shep-kelpie.git";

/// The repo's slug, which `gh` takes
const SLUG: &str = "shep-pm/shep-kelpie";

// Runs `command` to its end with its output going to ours, so a build's
// progress shows, and says which command failed.
fn run(command: &mut Command) -> Result<(), String> {
    let what = format!("{command:?}");
    let status = command
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run {what}: {e}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{what} exited {status}"))
}

// A command's trimmed standard output, or none when it ran and refused.
fn ask(command: &mut Command) -> Result<Option<String>, String> {
    let what = format!("{command:?}");
    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {what}: {e}"))?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
}

/// Builds kelpie at `reference` of the repo at `repo`, in a clone kept under `work`
///
/// The clone stays between upgrades, so its `target` folder does too and a
/// later build compiles only what changed. `reference` is a branch, a tag or
/// a commit.
///
/// # Errors
///
/// A message naming the git or cargo command that failed.
pub fn build_ref(work: &Path, repo: &str, reference: &str) -> Result<PathBuf, String> {
    if reference.is_empty() || reference.starts_with('-') {
        return Err(format!("{reference:?} is not a git ref"));
    }
    let clone = work.join("source");
    std::fs::create_dir_all(work).map_err(|e| format!("cannot make {}: {e}", work.display()))?;
    let git = |args: &[&str]| {
        let mut git = Command::new("git");
        git.arg("-C").arg(&clone).args(args);
        git
    };
    if clone.join(".git").exists() {
        run(&mut git(&[
            "fetch", "--quiet", "--tags", "--force", "origin",
        ]))?;
    } else {
        run(Command::new("git")
            .args(["clone", "--quiet", "--", repo])
            .arg(&clone))?;
    }
    // A branch is the remote's, not the stale local one a clone left behind.
    let commit = [format!("origin/{reference}"), reference.to_owned()]
        .iter()
        .find_map(|name| {
            let spec = format!("{name}^{{commit}}");
            ask(&mut git(&["rev-parse", "--verify", "--quiet", &spec]))
                .ok()
                .flatten()
        })
        .ok_or_else(|| format!("{repo} has no ref {reference}"))?;
    run(&mut git(&[
        "checkout", "--quiet", "--force", "--detach", &commit,
    ]))?;
    // Its own target folder, wherever the maintainer's `CARGO_TARGET_DIR` points.
    let target = clone.join("target");
    run(Command::new("cargo")
        .args(["build", "--release", "--locked"])
        .env("CARGO_TARGET_DIR", &target)
        .current_dir(&clone))?;
    let binary = target.join("release/shep-kelpie");
    binary
        .is_file()
        .then_some(binary)
        .ok_or_else(|| format!("the build of {reference} made no target/release/shep-kelpie"))
}

/// The release asset that holds a build for this machine
pub fn asset() -> String {
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        _ => "unknown-linux-gnu",
    };
    format!("shep-kelpie-{}-{os}.tar.gz", std::env::consts::ARCH)
}

/// Downloads release `version` of kelpie into a folder under `work`
///
/// The release is the tag `v<version>`, and its asset for this machine is a
/// gzipped tar holding `shep-kelpie`.
///
/// # Errors
///
/// A message when `version` is not a version, or `gh` or `tar` fails.
pub fn download_release(work: &Path, version: &str) -> Result<PathBuf, String> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+');
    if !version.starts_with(|c: char| c.is_ascii_digit()) || !version.chars().all(plain) {
        return Err(format!("{version:?} is not a release version"));
    }
    let folder = work.join(format!("release-{version}"));
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
    let asset = asset();
    run(Command::new("gh")
        .args(["release", "download", &format!("v{version}")])
        .args(["--repo", SLUG, "--pattern", &asset, "--clobber", "--dir"])
        .arg(&folder))?;
    run(Command::new("tar")
        .arg("-xzf")
        .arg(folder.join(&asset))
        .arg("-C")
        .arg(&folder))?;
    let binary = folder.join("shep-kelpie");
    binary
        .is_file()
        .then_some(binary)
        .ok_or_else(|| format!("{asset} held no shep-kelpie"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ref_that_looks_like_an_option_is_refused() {
        let work = tempfile::tempdir().unwrap();
        for reference in ["", "--upload-pack=x", "-b"] {
            let err = build_ref(work.path(), "/nowhere", reference).unwrap_err();
            assert!(err.contains("is not a git ref"), "{err}");
        }
    }

    #[test]
    fn a_release_is_named_by_a_plain_version() {
        let work = tempfile::tempdir().unwrap();
        for version in ["", "../0.3.0", "0.3.0 --clobber", "latest"] {
            let err = download_release(work.path(), version).unwrap_err();
            assert!(err.contains("is not a release version"), "{err}");
        }
    }

    #[test]
    fn the_asset_names_this_machine() {
        let asset = asset();
        assert!(asset.starts_with("shep-kelpie-"), "{asset}");
        assert!(asset.contains(std::env::consts::ARCH), "{asset}");
        assert!(asset.ends_with(".tar.gz"), "{asset}");
    }
}
