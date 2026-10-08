//! The GPU lease: the lock the maintainer's qwen scripts already take
//!
//! A folder at `$TMPDIR/qwen-review/gpu.lock`, made with `mkdir` so only
//! one taker wins, holding `pid`, `what` and `session` files. A waiter
//! polls on the scripts' own schedule, so everyone keeps their place in
//! the line. A lock whose pid is gone is stale, and the next waiter
//! clears it.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, UNIX_EPOCH};

use serde::Serialize;

use crate::ports::Timestamp;

/// The lock folder under the temporary folder, as the scripts name it
const LOCK: &str = "qwen-review/gpu.lock";

/// The folder under the temporary folder holding every other named lease's lock
const LEASES: &str = "kelpie-leases";

/// The temporary folder the maintainer's qwen scripts use
///
/// The scripts use `${TMPDIR:-/tmp}`. A sheep runs without `TMPDIR`, while
/// the maintainer's shell has macOS's per-user temporary folder, so an
/// unset or empty `TMPDIR` falls back to that folder before `/tmp`.
pub fn temp_dir() -> PathBuf {
    temp_dir_from(std::env::var_os("TMPDIR"), darwin_user_temp_dir)
}

fn temp_dir_from(tmpdir: Option<OsString>, darwin: impl FnOnce() -> Option<PathBuf>) -> PathBuf {
    match tmpdir {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => darwin().unwrap_or_else(|| PathBuf::from("/tmp")),
    }
}

// `getconf` reads the same confstr macOS sets TMPDIR from at login.
fn darwin_user_temp_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = crate::spawn::command("getconf")
        .arg("DARWIN_USER_TEMP_DIR")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let dir = String::from_utf8(output.stdout).ok()?;
    let dir = dir.trim_end_matches('\n');
    (output.status.success() && !dir.is_empty()).then(|| PathBuf::from(dir))
}

/// Whether process `pid` is alive, the way the scripts' `kill -0` asks
pub fn alive(pid: u32) -> bool {
    crate::spawn::command("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Who holds the lock, read from its files
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LockHolder {
    /// The holder's pid, when its `pid` file names one
    pub pid: Option<u32>,
    /// Whether that pid is alive. A dead one makes the lock stale.
    pub live: bool,
    /// What it is running, from `what`
    pub what: String,
    /// Its Claude session's messaging socket, from `session`, or empty
    pub session: String,
    /// When its `pid` file was written
    pub since: Option<Timestamp>,
}

/// What a holder writes into the lock
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The process whose life the lock lasts
    pub pid: u32,
    /// What it runs, for anyone waiting
    pub what: String,
}

/// One try at the lock
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// The lock is ours
    Taken,
    /// A live holder has it
    Held(LockHolder),
    /// A stale lock was cleared, naming its dead pid if it had one
    Cleared(Option<u32>),
}

/// The GPU lock under one temporary folder
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuLock {
    dir: PathBuf,
}

impl GpuLock {
    /// The lock the scripts take under `temp_dir`
    pub fn under(temp_dir: &Path) -> Self {
        Self {
            dir: temp_dir.join(LOCK),
        }
    }

    /// The lock a local reviewer's lease names under `temp_dir`
    ///
    /// `gpu` is the scripts' own lock. Any other name is a lock of the same
    /// format beside it, such as one for another machine's GPU.
    pub fn named(temp_dir: &Path, lease: &str) -> Self {
        if lease == super::GPU {
            return Self::under(temp_dir);
        }
        Self {
            dir: temp_dir.join(LEASES).join(format!("{lease}.lock")),
        }
    }

    /// The lock folder
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Who holds the lock, or `None` when it is free
    pub fn holder(&self) -> Option<LockHolder> {
        if !self.dir.is_dir() {
            return None;
        }
        let read = |name: &str| {
            fs::read_to_string(self.dir.join(name))
                .map(|s| s.trim_end_matches('\n').to_owned())
                .unwrap_or_default()
        };
        let pid = read("pid").parse().ok();
        let since = fs::metadata(self.dir.join("pid"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| Timestamp(d.as_secs()));
        Some(LockHolder {
            pid,
            live: pid.is_some_and(alive),
            what: read("what"),
            session: read("session"),
            since,
        })
    }

    /// Takes the lock for `claim` if it is free, or clears it if stale
    ///
    /// # Errors
    ///
    /// Any error making the folder or writing its files, except that the
    /// lock already exists.
    pub fn try_take(&self, claim: &Claim) -> io::Result<Attempt> {
        if let Some(parent) = self.dir.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::create_dir(&self.dir) {
            Ok(()) => {
                self.write_claim(claim)?;
                Ok(Attempt::Taken)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match self.holder() {
                Some(holder) if holder.live => Ok(Attempt::Held(holder)),
                holder => {
                    // The scripts clear a lock with no live pid the same way.
                    fs::remove_dir_all(&self.dir).or_else(ignore_not_found)?;
                    Ok(Attempt::Cleared(holder.and_then(|h| h.pid)))
                }
            },
            Err(e) => Err(e),
        }
    }

    // The scripts write pid first: a lock without one reads as stale.
    fn write_claim(&self, claim: &Claim) -> io::Result<()> {
        let session = std::env::var("CLAUDE_CODE_MESSAGING_SOCKET").unwrap_or_default();
        fs::write(self.dir.join("pid"), format!("{}\n", claim.pid))?;
        fs::write(self.dir.join("what"), format!("{}\n", claim.what))?;
        fs::write(self.dir.join("session"), format!("{session}\n"))
    }

    /// Waits for the lock on the scripts' schedule, then takes it
    ///
    /// `report` hears who holds it when the wait starts and every five
    /// minutes after, and each stale lock cleared. Returns the seconds
    /// waited. Safe to cancel: the lock is only ever taken between two
    /// waits, never across one.
    ///
    /// # Errors
    ///
    /// As [`GpuLock::try_take`].
    pub async fn take(
        &self,
        claim: &Claim,
        naps: impl Fn(u64) -> Duration,
        mut report: impl FnMut(Waiting<'_>),
    ) -> io::Result<u64> {
        let mut waited = 0;
        let mut next_report = 0;
        loop {
            match self.try_take(claim)? {
                Attempt::Taken => return Ok(waited),
                Attempt::Cleared(pid) => {
                    report(Waiting::Cleared(pid));
                    continue;
                }
                Attempt::Held(holder) if waited >= next_report => {
                    report(Waiting::Held {
                        waited,
                        holder: &holder,
                    });
                    next_report = waited + REPORT_EVERY;
                }
                Attempt::Held(_) => {}
            }
            let nap = naps(waited);
            tokio::time::sleep(nap).await;
            waited += nap.as_secs().max(1);
        }
    }

    /// Removes the lock if `pid` holds it, and says whether it did
    ///
    /// # Errors
    ///
    /// Any error removing the folder.
    pub fn release(&self, pid: u32) -> io::Result<bool> {
        match self.holder() {
            Some(holder) if holder.pid == Some(pid) => {
                fs::remove_dir_all(&self.dir).or_else(ignore_not_found)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

/// A lock taken for one call or round, let go when dropped
#[derive(Debug)]
pub struct GpuHold {
    lock: GpuLock,
    pid: u32,
}

impl GpuHold {
    /// The hold `pid` has on `lock`, which it took
    pub fn new(lock: GpuLock, pid: u32) -> Self {
        Self { lock, pid }
    }
}

impl Drop for GpuHold {
    fn drop(&mut self) {
        let _ = self.lock.release(self.pid);
    }
}

fn ignore_not_found(e: io::Error) -> io::Result<()> {
    match e.kind() {
        io::ErrorKind::NotFound => Ok(()),
        _ => Err(e),
    }
}

/// What a waiter hears while it waits
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting<'a> {
    /// The lock is held, after waiting this many seconds
    Held {
        /// Seconds waited so far
        waited: u64,
        /// Who holds it
        holder: &'a LockHolder,
    },
    /// A stale lock was cleared, naming its dead pid if it had one
    Cleared(Option<u32>),
}

/// How often a waiter says who it waits on, as the scripts do: five minutes
const REPORT_EVERY: u64 = 300;

/// The qwen scripts' naps, in seconds, by seconds waited so far
///
/// A waiter naps less the longer it has waited, so a freed lock goes to
/// the oldest waiter. Copied from `qwen-review.sh`: napping on any other
/// schedule would move kelpie up or down the line.
pub fn scripts_naps(waited: u64) -> Duration {
    Duration::from_secs(match waited {
        0..300 => 15,
        300..900 => 5,
        900..1800 => 2,
        _ => 1,
    })
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn lock() -> (tempfile::TempDir, GpuLock) {
        let temp = tempfile::tempdir().unwrap();
        let lock = GpuLock::under(temp.path());
        (temp, lock)
    }

    fn claim(pid: u32, what: &str) -> Claim {
        Claim {
            pid,
            what: what.into(),
        }
    }

    // A pid that was alive and is not now.
    fn dead_pid() -> u32 {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn a_taken_lock_is_in_the_scripts_format() {
        let (temp, lock) = lock();
        let me = std::process::id();
        assert_eq!(
            lock.try_take(&claim(me, "shep kelpie lease run: true"))
                .unwrap(),
            Attempt::Taken
        );
        let dir = temp.path().join("qwen-review/gpu.lock");
        let read = |name| fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(read("pid"), format!("{me}\n"));
        assert_eq!(read("what"), "shep kelpie lease run: true\n");
        assert!(read("session").ends_with('\n'));
    }

    #[test]
    fn a_live_holder_keeps_the_lock_and_is_named() {
        let (_temp, lock) = lock();
        let me = std::process::id();
        lock.try_take(&claim(me, "round 2 in /tmp/hunks")).unwrap();
        let Attempt::Held(holder) = lock.try_take(&claim(1, "another")).unwrap() else {
            panic!("the lock changed hands");
        };
        assert_eq!(
            (holder.pid, holder.live, holder.what.as_str()),
            (Some(me), true, "round 2 in /tmp/hunks")
        );
        assert!(holder.since.is_some());
    }

    #[test]
    fn a_lock_whose_pid_is_gone_is_cleared_then_taken() {
        let (_temp, lock) = lock();
        let dead = dead_pid();
        lock.try_take(&claim(dead, "a killed round")).unwrap();
        assert!(!lock.holder().unwrap().live);
        let me = std::process::id();
        assert_eq!(
            lock.try_take(&claim(me, "next")).unwrap(),
            Attempt::Cleared(Some(dead))
        );
        assert_eq!(lock.try_take(&claim(me, "next")).unwrap(), Attempt::Taken);
    }

    #[test]
    fn a_lock_with_no_pid_file_is_stale_as_the_scripts_read_it() {
        let (_temp, lock) = lock();
        fs::create_dir_all(lock.path()).unwrap();
        assert_eq!(
            lock.try_take(&claim(std::process::id(), "x")).unwrap(),
            Attempt::Cleared(None)
        );
    }

    #[test]
    fn release_removes_only_its_own_lock() {
        let (_temp, lock) = lock();
        let me = std::process::id();
        lock.try_take(&claim(me, "mine")).unwrap();
        assert!(!lock.release(me + 1).unwrap());
        assert!(lock.holder().is_some());
        assert!(lock.release(me).unwrap());
        assert_eq!(lock.holder(), None);
        assert!(!lock.release(me).unwrap());
    }

    #[tokio::test]
    async fn a_waiter_takes_the_lock_once_it_is_released() {
        let (_temp, lock) = lock();
        let mut holder = Command::new("sleep").arg("30").spawn().unwrap();
        lock.try_take(&claim(holder.id(), "a round")).unwrap();

        let waiter = lock.clone();
        let mut heard = Vec::new();
        let take = tokio::spawn(async move {
            let me = claim(std::process::id(), "kelpie");
            let naps = |_| Duration::from_millis(10);
            let waited = waiter
                .take(&me, naps, |w| heard.push(format!("{w:?}")))
                .await;
            (waited, heard)
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!take.is_finished(), "took a held lock");
        assert!(lock.release(holder.id()).unwrap());
        let (waited, heard) = tokio::time::timeout(Duration::from_secs(5), take)
            .await
            .expect("the waiter never took the freed lock")
            .unwrap();
        holder.kill().unwrap();
        holder.wait().unwrap();

        assert!(waited.unwrap() > 0);
        assert_eq!(heard.len(), 1, "one report at the start: {heard:?}");
        assert!(heard[0].contains("a round"), "{heard:?}");
        assert_eq!(lock.holder().unwrap().pid, Some(std::process::id()));
    }

    #[tokio::test]
    async fn a_waiter_clears_a_stale_lock_and_says_so() {
        let (_temp, lock) = lock();
        let dead = dead_pid();
        lock.try_take(&claim(dead, "a killed round")).unwrap();
        let mut heard = Vec::new();
        let me = claim(std::process::id(), "kelpie");
        let waited = lock
            .take(&me, |_| Duration::ZERO, |w| heard.push(format!("{w:?}")))
            .await
            .unwrap();
        assert_eq!(waited, 0);
        assert_eq!(heard, [format!("{:?}", Waiting::Cleared(Some(dead)))]);
    }

    #[test]
    fn the_naps_are_the_scripts_own() {
        let naps: Vec<u64> = [0, 299, 300, 899, 900, 1799, 1800, 99_999]
            .map(|w| scripts_naps(w).as_secs())
            .into();
        assert_eq!(naps, [15, 15, 5, 5, 2, 2, 1, 1]);
    }

    #[test]
    fn tmpdir_wins_when_set_and_not_empty() {
        let never = || panic!("asked macOS with TMPDIR set");
        assert_eq!(
            temp_dir_from(Some("/var/folders/x/T/".into()), never),
            PathBuf::from("/var/folders/x/T/")
        );
    }

    #[test]
    fn no_tmpdir_falls_back_to_the_users_temp_folder_then_tmp() {
        let user = || Some(PathBuf::from("/var/folders/x/T/"));
        assert_eq!(
            temp_dir_from(None, user),
            PathBuf::from("/var/folders/x/T/")
        );
        assert_eq!(
            temp_dir_from(Some("".into()), user),
            PathBuf::from("/var/folders/x/T/")
        );
        assert_eq!(temp_dir_from(None, || None), PathBuf::from("/tmp"));
    }

    #[test]
    fn the_lock_sits_where_the_scripts_put_it() {
        let lock = GpuLock::under(Path::new("/var/folders/x/T/"));
        assert_eq!(
            lock.path(),
            Path::new("/var/folders/x/T/qwen-review/gpu.lock")
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_names_a_per_user_temp_folder() {
        let dir = darwin_user_temp_dir().expect("getconf DARWIN_USER_TEMP_DIR");
        assert!(dir.is_dir(), "{}", dir.display());
    }
}
