//! The cross-process guard around starting the relay
//!
//! Every project is its own process, and all of them target the one fixed
//! relay name, so an in-process guard alone cannot stop two processes
//! racing to find none running and both start one. This is a plain mkdir
//! lock at `<folder>/starting.lock`, held across a find-then-start. A lock
//! whose holder pid has died is stale, and the next taker clears it, the
//! way [`crate::lease::gpu`] reads its own.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::lease::gpu::alive;
use crate::ports::RelayError;

/// How many times the lock is waited for, and how long between tries:
/// bounded, so a stuck holder parks a ruling rather than a runner.
const LOCK_TRIES: u32 = 40;
const LOCK_POLL: Duration = Duration::from_millis(250);

/// The lock at `<folder>/starting.lock`
#[derive(Debug)]
pub(super) struct StartLock {
    dir: PathBuf,
    tries: u32,
    poll: Duration,
}

/// Releases the lock's folder on drop, wherever the guarded code returns
#[derive(Debug)]
pub(super) struct StartGuard(PathBuf);

impl Drop for StartGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl StartLock {
    pub(super) fn under(folder: &Path) -> Self {
        Self {
            dir: folder.join("starting.lock"),
            tries: LOCK_TRIES,
            poll: LOCK_POLL,
        }
    }

    /// Blocks until the lock is ours, clearing a lock whose holder died
    ///
    /// # Errors
    ///
    /// [`RelayError::CannotStart`] when the lock cannot be made or cleared,
    /// or stays held past its tries.
    pub(super) fn acquire(&self) -> Result<StartGuard, RelayError> {
        for _ in 0..self.tries {
            match fs::create_dir_all(self.dir.parent().unwrap_or(&self.dir))
                .and_then(|()| fs::create_dir(&self.dir))
            {
                Ok(()) => {
                    let _ = fs::write(self.dir.join("pid"), std::process::id().to_string());
                    return Ok(StartGuard(self.dir.clone()));
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if !self.held_by_a_live_pid() {
                        // A failure here is surfaced rather than retried
                        // blind: a lock that cannot be cleared would
                        // otherwise time out as "held", hiding why.
                        fs::remove_dir_all(&self.dir)
                            .map_err(|e| RelayError::CannotStart(e.to_string()))?;
                        continue;
                    }
                }
                Err(e) => return Err(RelayError::CannotStart(e.to_string())),
            }
            thread::sleep(self.poll);
        }
        Err(RelayError::CannotStart(
            "the relay start lock is held".into(),
        ))
    }

    #[cfg(test)]
    fn under_with(folder: &Path, tries: u32, poll: Duration) -> Self {
        Self {
            dir: folder.join("starting.lock"),
            tries,
            poll,
        }
    }

    fn held_by_a_live_pid(&self) -> bool {
        fs::read_to_string(self.dir.join("pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .is_some_and(alive)
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    // A pid that was alive and is not now, the way `lease::gpu`'s own
    // tests make one.
    fn dead_pid() -> u32 {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    // Two `StartLock`s over the same folder stand in for two runner
    // processes racing to start the one relay they share by name.
    #[test]
    fn only_one_runner_holds_the_start_lock_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let a = StartLock::under(dir.path());
        let b = StartLock::under(dir.path());

        let held = a.acquire().unwrap();
        assert!(dir.path().join("starting.lock").is_dir());
        let waiting = thread::spawn(move || b.acquire());
        thread::sleep(Duration::from_millis(100));
        assert!(
            !waiting.is_finished(),
            "the second runner should still be waiting"
        );

        drop(held);
        let held2 = waiting
            .join()
            .unwrap()
            .expect("the freed lock should be taken");
        drop(held2);
        assert!(!dir.path().join("starting.lock").exists());
    }

    #[test]
    fn a_lock_held_by_a_live_pid_the_whole_wait_gives_up() {
        let dir = tempfile::tempdir().unwrap();
        let held = dir.path().join("starting.lock");
        fs::create_dir_all(&held).unwrap();
        fs::write(held.join("pid"), std::process::id().to_string()).unwrap();

        let lock = StartLock::under_with(dir.path(), 3, Duration::from_millis(5));
        let err = lock.acquire().unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot start the relay: the relay start lock is held"
        );
        assert!(held.is_dir(), "a live holder's lock is never removed");
    }

    #[test]
    fn a_start_lock_left_by_a_dead_runner_is_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let lock = StartLock::under(dir.path());
        let stale = dir.path().join("starting.lock");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("pid"), dead_pid().to_string()).unwrap();

        let held = lock.acquire().unwrap();
        assert_eq!(
            fs::read_to_string(stale.join("pid")).unwrap(),
            std::process::id().to_string()
        );
        drop(held);
    }
}
