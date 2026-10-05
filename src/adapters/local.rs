//! The local round, over a command or an OpenAI-compatible endpoint
//!
//! A reviewer's definition chooses the kind. A command keeps the README's
//! contract, as the maintainer's qwen-review script does. An endpoint gets
//! kelpie's own reviewer. Either way kelpie holds a lease around a round
//! only when the definition names one: the qwen-review script takes the GPU
//! lock itself, and would wait forever behind kelpie's hold.

mod command;
mod endpoint;
mod ollama;
mod queue;
#[cfg(test)]
mod watched;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::process::Processes;
use crate::lease::gpu::{self, Attempt, Claim, GpuHold, GpuLock, LockHolder};
use crate::ports::{
    AgentError, Finding, LocalLeases, ModelSeat, Reviewer, ReviewerError, RoundStage,
};
use crate::settings::{LeaseName, LocalRound};

/// How often a round waiting on the GPU lock looks for a stop
const STOP_POLL: Duration = Duration::from_millis(100);

/// Runs each local round, of whichever kind the settings choose
///
/// Clones share their rounds in flight, so one clone can stop them all.
#[derive(Debug, Clone)]
pub struct LocalReviewer {
    temp_dir: PathBuf,
    processes: Processes,
    // Where the model sat when a round last looked, for `status`.
    seat: Arc<Mutex<Option<ModelSeat>>>,
    // How long a round waiting on the lock naps, given the seconds it has waited
    naps: fn(u64) -> Duration,
}

impl Default for LocalReviewer {
    fn default() -> Self {
        Self {
            temp_dir: gpu::temp_dir(),
            processes: Processes::default(),
            seat: Arc::default(),
            naps: gpu::scripts_naps,
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

    #[cfg(test)]
    fn with_naps(mut self, naps: fn(u64) -> Duration) -> Self {
        self.naps = naps;
        self
    }

    /// Ends every round in flight, and refuses new ones, as the runner stops
    pub fn stop(&self) {
        self.processes.stop();
    }

    // Waits on the scripts' own schedule, so kelpie keeps its place in line.
    fn hold(
        &self,
        lease: &str,
        what: String,
        watch: &(dyn Fn(RoundStage) + Sync),
    ) -> Result<GpuHold, Unheld> {
        let lock = GpuLock::named(&self.temp_dir, lease);
        let claim = Claim {
            pid: std::process::id(),
            what,
        };
        let cannot = |e: std::io::Error| {
            Unheld::Failed(format!("cannot take {}: {e}", lock.path().display()))
        };
        let mut waited = 0;
        let mut queued = false;
        loop {
            match lock.try_take(&claim).map_err(cannot)? {
                Attempt::Taken => {
                    if queued {
                        watch(RoundStage::Running);
                    }
                    return Ok(GpuHold::new(lock, claim.pid));
                }
                Attempt::Cleared(_) => continue,
                Attempt::Held(_) => {
                    if !queued {
                        watch(RoundStage::Queued);
                        queued = true;
                    }
                }
            }
            let nap = (self.naps)(waited);
            let mut slept = Duration::ZERO;
            while slept < nap {
                if self.processes.stopping() {
                    return Err(Unheld::Stopped);
                }
                thread::sleep(STOP_POLL);
                slept += STOP_POLL;
            }
            waited += nap.as_secs().max(1);
        }
    }
}

// Why a lease was not taken
enum Unheld {
    Stopped,
    Failed(String),
}

impl LocalLeases for LocalReviewer {
    fn hold(&self, lease: &LeaseName, what: &str) -> Result<GpuHold, AgentError> {
        LocalReviewer::hold(self, lease.as_str(), what.to_owned(), &|_| {}).map_err(|e| match e {
            Unheld::Stopped => AgentError::Stopped,
            Unheld::Failed(reason) => AgentError::Setup(reason),
        })
    }

    fn holder(&self, lease: &LeaseName) -> Option<LockHolder> {
        GpuLock::named(&self.temp_dir, lease.as_str()).holder()
    }
}

impl Reviewer for LocalReviewer {
    fn check(&self, local: &LocalRound) -> Result<(), String> {
        match local {
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
        criteria: &str,
    ) -> Result<Vec<Finding>, ReviewerError> {
        self.round_watched(local, worktree, base, out, round, criteria, &|_| {})
    }

    fn round_watched(
        &self,
        local: &LocalRound,
        worktree: &Path,
        base: &str,
        out: &Path,
        round: u32,
        criteria: &str,
        watch: &(dyn Fn(RoundStage) + Sync),
    ) -> Result<Vec<Finding>, ReviewerError> {
        let lease = local.lease();
        let _hold = match &lease {
            Some(lease) => {
                let what = format!("kelpie local round {round} in {}", worktree.display());
                let held = self.hold(lease.as_str(), what, watch).map_err(|e| match e {
                    Unheld::Stopped => ReviewerError::Stopped,
                    Unheld::Failed(reason) => ReviewerError::Failed(reason),
                });
                Some(held?)
            }
            None => None,
        };
        self.check_seat(local)?;
        match local {
            LocalRound::Command(command) => {
                // A command under a lease of kelpie's is not queued on the gpu
                // lock the watcher reads. One with none is assumed to queue
                // on it, as the qwen script does.
                let queue_watch = lease.is_none().then_some(watch);
                self.command_round(command, worktree, base, out, round, criteria, queue_watch)
            }
            LocalRound::Endpoint(endpoint) => {
                self.endpoint_round(endpoint, worktree, base, out, round, criteria)
            }
        }
    }

    fn seat(&self) -> Option<ModelSeat> {
        self.last_seat()
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
    use crate::settings::{LeaseName, LocalCommand};
    use crate::test::write_script;

    fn command(path: &Path, gpu_lease: bool) -> LocalRound {
        leased(path, gpu_lease.then(LeaseName::gpu))
    }

    fn leased(path: &Path, lease: Option<LeaseName>) -> LocalRound {
        LocalRound::Command(LocalCommand {
            command: path.to_owned(),
            lease,
            ollama: None,
            ollama_model: None,
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
                .round(&local, &worktree, "main", &out, 1, "")
                .unwrap();
            assert_eq!(findings[0].file, seen, "gpu_lease = {gpu_lease}");
            assert!(
                lock.holder().is_none(),
                "the lock is let go after the round"
            );
        }
    }

    // Another holder keeps the other lease the whole time: a round that
    // waited on it would never finish.
    #[test]
    fn each_reviewer_takes_only_its_own_lease() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("tmp");
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        crate::test::git(&worktree, &["init", "--quiet", "-b", "main"]);
        crate::test::git(
            &worktree,
            &["commit", "--quiet", "--allow-empty", "-m", "init"],
        );
        let reviewer = LocalReviewer::default().with_temp_dir(temp.clone());
        let parent = Claim {
            pid: std::os::unix::process::parent_id(),
            what: "someone else's round".into(),
        };
        for (own, other) in [("gpu", "gpu-box"), ("gpu-box", "gpu")] {
            let (own_lock, other_lock) = (GpuLock::named(&temp, own), GpuLock::named(&temp, other));
            assert_ne!(own_lock.path(), other_lock.path());
            assert_eq!(other_lock.try_take(&parent).unwrap(), Attempt::Taken);
            let script = dir.path().join(format!("review-{own}"));
            write_script(
                &script,
                &format!(
                    "#!/bin/sh\nmkdir -p \"$QWEN_REVIEW_OUT\"\n\
                     if [ -d '{lock}' ]; then held=held; else held=free; fi\n\
                     printf 'LOW|%s:1|seen|seen\\n' \"$held\" > \"$QWEN_REVIEW_OUT/round-1.txt\"\n\
                     : > \"$QWEN_REVIEW_OUT/round-1.txt.done\"\n",
                    lock = own_lock.path().display(),
                ),
            );
            let lease = LeaseName::try_from(own.to_owned()).unwrap();
            let local = leased(&script, Some(lease));
            let out = dir.path().join(format!("out-{own}"));
            let findings = reviewer
                .round(&local, &worktree, "main", &out, 1, "")
                .unwrap();
            assert_eq!(findings[0].file, "held", "{own} is held around its round");
            assert!(own_lock.holder().is_none(), "{own} is let go after");
            assert_eq!(other_lock.holder().and_then(|h| h.pid), Some(parent.pid));
            other_lock.release(parent.pid).unwrap();
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
        let result = reviewer.round(&local, dir.path(), "main", &out, 1, "");
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
