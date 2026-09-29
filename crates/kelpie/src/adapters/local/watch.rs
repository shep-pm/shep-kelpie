//! Watching a script's queue for the GPU lock
//!
//! The qwen-review script takes the lock itself and writes its own pid into
//! it. Kelpie sees the wait from outside: the time from the script starting
//! to the lock naming its pid.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::lease::gpu::GpuLock;

/// How often the lock is looked at
const POLL: Duration = Duration::from_millis(100);

/// A thread looking at the lock for one script's pid
pub(super) struct Watch {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Duration>,
}

impl Watch {
    /// Starts timing from now, until the lock names `pid`
    pub(super) fn start(lock: GpuLock, pid: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let began = Instant::now();
        let thread = thread::spawn(move || {
            // Where the last look found someone else holding the lock, for a
            // script that took it and let go between two looks.
            let mut blocked = Duration::ZERO;
            loop {
                match lock.holder() {
                    Some(h) if h.pid == Some(pid) => return began.elapsed(),
                    Some(_) => blocked = began.elapsed(),
                    None => {}
                }
                if stopped.load(Ordering::SeqCst) {
                    return blocked;
                }
                thread::sleep(POLL);
            }
        });
        Self { stop, thread }
    }

    /// Stops looking, and says how long the script queued: until the lock
    /// named it, or else until it was last seen held by another
    pub(super) fn finish(self) -> Duration {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.join().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::super::LocalReviewer;
    use crate::lease::gpu::{Attempt, Claim, GpuLock};
    use crate::ports::{Reviewer, Round};
    use crate::settings::{LocalCommand, LocalRound};
    use crate::test::{git, write_script};

    // The script queues on the lock the way the maintainer's does: it takes
    // the folder when it can, and writes its own pid into it.
    const SCRIPT: &str = "#!/bin/sh\nL=\"$TMPDIR/qwen-review/gpu.lock\"\n\
        mkdir -p \"$TMPDIR/qwen-review\"\n\
        : > \"$TMPDIR/started\"\n\
        until mkdir \"$L\" 2>/dev/null; do sleep 0.05; done\n\
        echo $$ > \"$L/pid\"\n\
        mkdir -p \"$QWEN_REVIEW_OUT\"\n\
        : > \"$QWEN_REVIEW_OUT/round-1.txt\"\n\
        : > \"$QWEN_REVIEW_OUT/round-1.txt.done\"\n\
        rm -rf \"$L\"\n";

    fn round(home: &Path, temp: &Path) -> Round {
        let script = home.join("review");
        write_script(&script, SCRIPT);
        let worktree = home.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        git(&worktree, &["init", "--quiet", "-b", "main"]);
        git(
            &worktree,
            &["commit", "--quiet", "--allow-empty", "-m", "init"],
        );
        let local = LocalRound::Command(LocalCommand {
            command: script,
            gpu_lease: false,
        });
        LocalReviewer::default()
            .with_temp_dir(temp.to_owned())
            .round(&local, &worktree, "origin/main", &home.join("out"), 1)
            .unwrap()
    }

    // Real time: the script is a child process, and the wait is how long
    // another process held a lock that no test clock reaches.
    #[test]
    fn a_script_queued_behind_another_holder_reports_the_wait() {
        let home = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let lock = GpuLock::under(temp.path());
        let pid = std::process::id();
        let claim = Claim {
            pid,
            what: "a session's round".into(),
        };
        assert_eq!(lock.try_take(&claim).unwrap(), Attempt::Taken);

        let (dir, held) = (home.path().to_owned(), temp.path().to_owned());
        let running = std::thread::spawn(move || round(&dir, &held));
        let started = temp.path().join("started");
        let limit = Instant::now() + Duration::from_secs(30);
        while !started.exists() {
            assert!(Instant::now() < limit, "the script never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(600));
        assert!(lock.release(pid).unwrap());
        let found = running.join().unwrap();

        assert!(found.findings.is_empty());
        assert!(
            found.gpu_wait >= Duration::from_millis(500),
            "{:?}",
            found.gpu_wait
        );
    }

    #[test]
    fn a_script_that_finds_the_lock_free_reports_next_to_no_wait() {
        let home = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let found = round(home.path(), temp.path());
        assert!(
            found.gpu_wait < Duration::from_secs(1),
            "{:?}",
            found.gpu_wait
        );
    }
}
