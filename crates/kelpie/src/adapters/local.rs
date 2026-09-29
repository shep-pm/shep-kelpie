//! The local round, over a command or an OpenAI-compatible endpoint
//!
//! A project's settings choose the kind. A command keeps the README's
//! contract, as the maintainer's qwen-review script does. An endpoint gets
//! kelpie's own reviewer. Either way kelpie holds the GPU lock around a
//! round only when the settings ask it to: the qwen-review script takes that
//! lock itself, and would wait forever behind kelpie's hold.

mod command;
mod endpoint;
mod watch;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::process::Processes;
use crate::lease::gpu::{self, Attempt, Claim, GpuLock};
use crate::ports::{Reviewer, ReviewerError, Round};
use crate::settings::LocalRound;

/// How often a round waiting on the GPU lock looks for a stop
const STOP_POLL: Duration = Duration::from_millis(100);

/// Runs each local round, of whichever kind the settings choose
///
/// Clones share their rounds in flight, so one clone can stop them all.
#[derive(Debug, Clone)]
pub struct LocalReviewer {
    temp_dir: PathBuf,
    processes: Processes,
}

impl Default for LocalReviewer {
    fn default() -> Self {
        Self {
            temp_dir: gpu::temp_dir(),
            processes: Processes::default(),
        }
    }
}

impl LocalReviewer {
    /// Runs every round with `TMPDIR` set to `temp_dir`, the GPU lock's folder
    ///
    /// A reviewer starts with [`gpu::temp_dir`], the folder the dog's own GPU
    /// lock is under. The script builds its GPU lock under `${TMPDIR:-/tmp}`,
    /// and the runner under shep has no `TMPDIR`. Without this, its rounds
    /// lock `/tmp` while every interactive session locks the per-user temp
    /// folder, and the two queues run the GPU at once.
    pub fn with_temp_dir(mut self, temp_dir: PathBuf) -> Self {
        self.temp_dir = temp_dir;
        self
    }

    /// Ends every round in flight, and refuses new ones, as the runner stops
    pub fn stop(&self) {
        self.processes.stop();
    }

    // Waits on the scripts' own schedule, so kelpie keeps its place in line.
    fn hold_gpu(&self, round: u32, worktree: &Path) -> Result<GpuHold, ReviewerError> {
        let lock = GpuLock::under(&self.temp_dir);
        let claim = Claim {
            pid: std::process::id(),
            what: format!("kelpie local round {round} in {}", worktree.display()),
        };
        let cannot = |e: std::io::Error| {
            ReviewerError::Failed(format!("cannot take {}: {e}", lock.path().display()))
        };
        let began = Instant::now();
        let mut waited = 0;
        loop {
            match lock.try_take(&claim).map_err(cannot)? {
                Attempt::Taken => {
                    return Ok(GpuHold {
                        lock,
                        pid: claim.pid,
                        waited: began.elapsed(),
                    });
                }
                Attempt::Cleared(_) => continue,
                Attempt::Held(_) => {}
            }
            let nap = gpu::scripts_naps(waited);
            let mut slept = Duration::ZERO;
            while slept < nap {
                if self.processes.stopping() {
                    return Err(ReviewerError::Stopped);
                }
                thread::sleep(STOP_POLL);
                slept += STOP_POLL;
            }
            waited += nap.as_secs().max(1);
        }
    }
}

/// The GPU lock, held for one round and let go when dropped
struct GpuHold {
    lock: GpuLock,
    pid: u32,
    waited: Duration,
}

impl Drop for GpuHold {
    fn drop(&mut self) {
        let _ = self.lock.release(self.pid);
    }
}

impl Reviewer for LocalReviewer {
    fn check(&self, local: &LocalRound) -> Result<(), String> {
        match local {
            LocalRound::Off {} => Ok(()),
            LocalRound::Command(local) => check_command(&local.command),
            LocalRound::Endpoint(endpoint) => self.check_endpoint(endpoint),
        }
    }

    fn round(
        &self,
        local: &LocalRound,
        worktree: &Path,
        base: &str,
        out: &Path,
        round: u32,
    ) -> Result<Round, ReviewerError> {
        let hold = match local.gpu_lease() {
            true => Some(self.hold_gpu(round, worktree)?),
            false => None,
        };
        let mut found = match local {
            LocalRound::Off {} => Round::default(),
            LocalRound::Command(local) => self.command_round(local, worktree, base, out, round)?,
            LocalRound::Endpoint(endpoint) => Round {
                findings: self.endpoint_round(endpoint, worktree, base, out, round)?,
                gpu_wait: Duration::ZERO,
            },
        };
        found.gpu_wait += hold.map_or(Duration::ZERO, |hold| hold.waited);
        Ok(found)
    }
}

fn check_command(command: &Path) -> Result<(), String> {
    let cannot = |why: &str| format!("cannot run {}: {why}", command.display());
    let meta = std::fs::metadata(command).map_err(|e| cannot(&e.kind().to_string()))?;
    if !meta.is_file() {
        return Err(cannot("not a file"));
    }
    if meta.permissions().mode() & 0o111 == 0 {
        return Err(cannot("not executable"));
    }
    Ok(())
}

/// Removes round `round`'s findings file and marker from `out`, so a rework
/// or a retried round never reads the last run's
fn clear_round(out: &Path, round: u32) -> Result<(), ReviewerError> {
    for name in [
        format!("round-{round}.txt"),
        format!("round-{round}.txt.done"),
    ] {
        let path = out.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ReviewerError::Failed(format!(
                    "cannot remove {}: {e}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

/// The worktree's head commit
fn head(worktree: &Path) -> Result<String, ReviewerError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "HEAD"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| ReviewerError::Spawn(e.to_string()))?;
    if !output.status.success() {
        return Err(ReviewerError::Failed(format!(
            "cannot read the head of {}: {}",
            worktree.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::LocalCommand;
    use crate::test::write_script;

    fn command(path: &Path, gpu_lease: bool) -> LocalRound {
        LocalRound::Command(LocalCommand {
            command: path.to_owned(),
            gpu_lease,
        })
    }

    #[test]
    fn a_command_is_checked_for_being_there_and_runnable() {
        let dir = tempfile::tempdir().unwrap();
        let reviewer = LocalReviewer::default();
        let gone = dir.path().join("gone");
        let err = reviewer.check(&command(&gone, false)).unwrap_err();
        assert_eq!(
            err,
            format!("cannot run {}: entity not found", gone.display())
        );
        let plain = dir.path().join("plain.txt");
        std::fs::write(&plain, "text\n").unwrap();
        let err = reviewer.check(&command(&plain, false)).unwrap_err();
        assert!(err.ends_with("plain.txt: not executable"), "{err}");
        let err = reviewer.check(&command(dir.path(), false)).unwrap_err();
        assert!(err.ends_with(": not a file"), "{err}");
        let script = dir.path().join("review");
        write_script(&script, "#!/bin/sh\nexit 0\n");
        assert_eq!(reviewer.check(&command(&script, false)), Ok(()));
        assert_eq!(reviewer.check(&LocalRound::Off {}), Ok(()));
    }

    // The stand-in reports whether the lock was there while it ran.
    #[test]
    fn the_gpu_lock_is_held_around_a_round_only_when_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("tmp");
        let lock = GpuLock::under(&temp);
        let script = dir.path().join("review");
        let contents = format!(
            "#!/bin/sh\nmkdir -p \"$QWEN_REVIEW_OUT\"\n\
             if [ -d '{lock}' ]; then held=held; else held=free; fi\n\
             printf 'LOW|%s:1|seen|seen\\n' \"$held\" > \"$QWEN_REVIEW_OUT/round-1.txt\"\n\
             : > \"$QWEN_REVIEW_OUT/round-1.txt.done\"\n",
            lock = lock.path().display(),
        );
        write_script(&script, &contents);
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        crate::test::git(&worktree, &["init", "--quiet", "-b", "main"]);
        crate::test::git(
            &worktree,
            &["commit", "--quiet", "--allow-empty", "-m", "init"],
        );
        let reviewer = LocalReviewer::default().with_temp_dir(temp.clone());

        for (gpu_lease, seen) in [(false, "free"), (true, "held")] {
            let out = dir.path().join(format!("out-{gpu_lease}"));
            let local = command(&script, gpu_lease);
            let findings = reviewer
                .round(&local, &worktree, "main", &out, 1)
                .map(|round| round.findings)
                .unwrap();
            assert_eq!(findings[0].file, seen, "gpu_lease = {gpu_lease}");
            assert!(
                lock.holder().is_none(),
                "the lock is let go after the round"
            );
        }
    }

    #[test]
    fn a_round_waiting_on_the_gpu_lock_ends_with_the_runner() {
        let dir = tempfile::tempdir().unwrap();
        let lock = GpuLock::under(dir.path());
        let parent = Claim {
            pid: std::os::unix::process::parent_id(),
            what: "someone else's round".into(),
        };
        assert_eq!(lock.try_take(&parent).unwrap(), Attempt::Taken);
        let reviewer = LocalReviewer::default().with_temp_dir(dir.path().to_owned());
        let stopper = reviewer.clone();
        let stopping = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            stopper.stop();
        });
        let local = command(Path::new("/bin/true"), true);
        let out = dir.path().join("out");
        let started = std::time::Instant::now();
        let result = reviewer
            .round(&local, dir.path(), "main", &out, 1)
            .map(|round| round.findings);
        stopping.join().unwrap();
        assert_eq!(result, Err(ReviewerError::Stopped));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(lock.holder().and_then(|h| h.pid), Some(parent.pid));
    }
}
