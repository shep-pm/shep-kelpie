//! Getting a build to install: a release's download, or a build of a git ref
//!
//! Both leave the binary in a folder under kelpie's home and hand back its
//! path. Nothing here touches the installed build.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::interrupt::{Caught, Interrupts};
use crate::tree::{Lead, Signal, Tree};

/// The repo a release or a ref comes from, unless `KELPIE_SOURCE` names another
pub const REPO: &str = "https://github.com/shep-pm/shep-kelpie.git";

/// The repo's slug, which `gh` takes
const SLUG: &str = "shep-pm/shep-kelpie";

/// How long each command of a ref build may run before it is ended
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// For each `git` command: clone, fetch and checkout
    pub git: Duration,
    /// For the `cargo build`
    pub cargo: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            git: NETWORK,
            cargo: Duration::from_secs(45 * 60),
        }
    }
}

/// How long a command that talks to the network gets
const NETWORK: Duration = Duration::from_secs(5 * 60);

// The tree of the command `run` is waiting on, or none between commands
static RUNNING: Mutex<Option<Arc<Tree>>> = Mutex::new(None);

// Kills the running command's tree, if one is running.
fn end_running() {
    let running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(tree) = running.as_ref() {
        tree.send(Signal::Kill);
    }
}

// A command's own tree does not get the terminal's Ctrl-C, so it reaches only
// kelpie. This thread, started once, ends the running command's tree before
// kelpie follows the signal out.
fn end_with_kelpie() {
    static WATCHER: OnceLock<()> = OnceLock::new();
    WATCHER.get_or_init(|| {
        let (ready, registered) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async {
                let kinds = [Caught::Interrupt, Caught::Terminate, Caught::Hangup];
                let Ok(mut interrupts) = Interrupts::listen(&kinds) else {
                    return;
                };
                let _ = ready.send(());
                let caught = interrupts.recv().await;
                end_running();
                std::process::exit(i32::from(caught.number()));
            });
        });
        let _ = registered.recv();
    });
}

// Marks a tree as the one a signal to kelpie ends, until dropped.
struct Running;

impl Running {
    fn tree(tree: &Arc<Tree>) -> Self {
        *RUNNING.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(tree));
        Self
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        *RUNNING.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

// Runs `command` to its end with its output going to ours, so a build's
// progress shows, and says which command failed. A command still running at
// `limit` is ended with everything it started, which its own tree holds.
fn run(command: &mut Command, limit: Duration) -> Result<(), String> {
    let what = format!("{command:?}");
    end_with_kelpie();
    let mut child = command
        .stdin(Stdio::null())
        .lead()
        .spawn()
        .map_err(|e| format!("cannot run {what}: {e}"))?;
    let tree = match Tree::of(&child) {
        Ok(tree) => Arc::new(tree),
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("cannot hold {what}: {e}"));
        }
    };
    let _running = Running::tree(&tree);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= limit => {
                tree.send(Signal::Kill);
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{what} did not end in {limit:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("cannot wait for {what}: {e}")),
        }
    };
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{what} exited {status}"))
}

// A command's trimmed standard output, or none when it ran and refused. It
// has no limit: its one use reads local refs, which wait on no network.
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
/// a commit. Each command gets its limit from `limits`.
///
/// # Errors
///
/// A message naming the git or cargo command that failed or outlasted its limit.
pub fn build_ref(
    work: &Path,
    repo: &str,
    reference: &str,
    limits: Limits,
) -> Result<PathBuf, String> {
    if reference.is_empty() || reference.starts_with('-') {
        return Err(format!("{reference:?} is not a git ref"));
    }
    let clone = work.join("source");
    std::fs::create_dir_all(work).map_err(|e| format!("cannot make {}: {e}", work.display()))?;
    let git = |args: &[&str]| {
        let mut git = crate::spawn::command("git");
        git.env("GIT_TERMINAL_PROMPT", "0")
            .arg("-C")
            .arg(&clone)
            .args(args);
        git
    };
    if clone.join(".git").exists() {
        run(
            &mut git(&["fetch", "--quiet", "--tags", "--force", "origin"]),
            limits.git,
        )?;
    } else {
        run(
            crate::spawn::command("git")
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(["clone", "--quiet", "--", repo])
                .arg(&clone),
            limits.git,
        )?;
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
    run(
        &mut git(&["checkout", "--quiet", "--force", "--detach", &commit]),
        limits.git,
    )?;
    // Its own target folder, wherever the maintainer's `CARGO_TARGET_DIR` points.
    let target = clone.join("target");
    run(
        crate::spawn::command("cargo")
            .args(["build", "--release", "--locked"])
            .env("CARGO_TARGET_DIR", &target)
            .current_dir(&clone),
        limits.cargo,
    )?;
    let name = format!("shep-kelpie{}", std::env::consts::EXE_SUFFIX);
    let binary = target.join("release").join(&name);
    binary
        .is_file()
        .then_some(binary)
        .ok_or_else(|| format!("the build of {reference} made no target/release/{name}"))
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
    run(
        crate::spawn::command("gh")
            .args(["release", "download", &format!("v{version}")])
            .args(["--repo", SLUG, "--pattern", &asset, "--clobber", "--dir"])
            .arg(&folder),
        NETWORK,
    )?;
    run(
        crate::spawn::command("tar")
            .arg("-xzf")
            .arg(folder.join(&asset))
            .arg("-C")
            .arg(&folder),
        NETWORK,
    )?;
    let binary = folder.join("shep-kelpie");
    binary
        .is_file()
        .then_some(binary)
        .ok_or_else(|| format!("{asset} held no shep-kelpie"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::test::{Fake, write_fake};

    #[test]
    fn a_ref_that_looks_like_an_option_is_refused() {
        let work = tempfile::tempdir().unwrap();
        for reference in ["", "--upload-pack=x", "-b"] {
            let err = build_ref(work.path(), "/nowhere", reference, Limits::default()).unwrap_err();
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

    #[cfg(unix)]
    #[test]
    fn a_command_that_ends_in_time_returns_its_exit_status() {
        let dir = tempfile::tempdir().unwrap();
        let program = |name, code| write_fake(dir.path(), name, &Fake::new().exits(code));
        run(
            &mut Command::new(program("true", 0)),
            Duration::from_secs(60),
        )
        .unwrap();
        let err = run(
            &mut Command::new(program("false", 1)),
            Duration::from_secs(60),
        )
        .unwrap_err();
        assert!(err.contains("false") && err.contains("exited"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_command_that_outlasts_its_limit_is_ended_with_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        // On a loaded machine the stand-in can be ended before it writes its pid.
        let pid: u32 = (0..5)
            .find_map(|_| {
                let _ = std::fs::remove_file(&pid_file);
                let started = Instant::now();
                let err = run(
                    Command::new(crate::spawn::sh())
                        .args(["-c", "sleep 300 & echo $! > \"$0\"; wait"])
                        .arg(&pid_file),
                    Duration::from_millis(300),
                )
                .unwrap_err();
                assert!(started.elapsed() < Duration::from_secs(30), "{err}");
                assert!(err.contains("sleep 300") && err.contains("300ms"), "{err}");
                std::fs::read_to_string(&pid_file).ok()?.trim().parse().ok()
            })
            .expect("the stand-in wrote its pid before its limit");
        // The orphan is reaped by init a moment after the kill.
        let gone = (0..200).any(|_| {
            let gone = !crate::tree::alive(pid);
            if !gone {
                std::thread::sleep(Duration::from_millis(25));
            }
            gone
        });
        assert!(gone, "pid {pid} outlived its command");
    }

    #[test]
    fn the_asset_names_this_machine() {
        let asset = asset();
        assert!(asset.starts_with("shep-kelpie-"), "{asset}");
        assert!(asset.contains(std::env::consts::ARCH), "{asset}");
        assert!(asset.ends_with(".tar.gz"), "{asset}");
    }
}
