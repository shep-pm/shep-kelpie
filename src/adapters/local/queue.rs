//! A command round's place in the GPU queue
//!
//! The qwen script takes the GPU lock itself and waits in line for it, out
//! of kelpie's sight. The lock's holder says whether the round still queues.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::lease::gpu::{GpuLock, LockHolder};
use crate::ports::RoundStage;

/// How often a running command's place in the queue is looked at
const QUEUE_POLL: Duration = Duration::from_millis(250);

/// Reports `Queued` while a live process outside the command's group holds
/// the lock, and `Running` once the command's own group holds it
///
/// The command's pid arrives on `spawned` once it runs, and the sender
/// dropping ends the watch. A free lock changes nothing: the round is still
/// in line until it holds the lock, and another process may take it first.
/// A watch that ends while queued reports `Running`, so the next run starts
/// from a known stage.
pub(super) fn watch_queue(
    lock: &GpuLock,
    spawned: &Receiver<u32>,
    watch: &(dyn Fn(RoundStage) + Sync),
) {
    let Ok(group) = spawned.recv() else { return };
    let mut queued = false;
    let mut groups = HashMap::new();
    loop {
        let holder = holder_of(lock.holder(), group, &mut groups, process_group);
        if let Some(stage) = change(queued, holder) {
            watch(stage);
            queued = stage == RoundStage::Queued;
        }
        match spawned.recv_timeout(QUEUE_POLL) {
            Err(RecvTimeoutError::Timeout) => {}
            _ => break,
        }
    }
    if queued {
        watch(RoundStage::Running);
    }
}

/// Who holds the GPU lock, as the round sees it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder {
    /// A live process outside the round's group
    Foreign,
    /// The round's own group
    Ours,
    /// Nobody, a dead claim, kelpie itself, or a group that cannot be read
    Neither,
}

// The stage to report, if the holder moves the round to another one.
fn change(queued: bool, holder: Holder) -> Option<RoundStage> {
    match holder {
        Holder::Foreign if !queued => Some(RoundStage::Queued),
        Holder::Ours if queued => Some(RoundStage::Running),
        _ => None,
    }
}

// A holder whose group cannot be read counts as neither, so a failed `ps`
// never inflates the wait. Kelpie's own hold is not a queue either.
fn holder_of(
    holder: Option<LockHolder>,
    group: u32,
    groups: &mut HashMap<u32, Option<u32>>,
    read: impl FnOnce(u32) -> Option<u32>,
) -> Holder {
    holder
        .filter(|h| h.live)
        .and_then(|h| h.pid)
        .filter(|pid| *pid != std::process::id())
        .and_then(|pid| group_of(groups, pid, read))
        .map_or(Holder::Neither, |holder_group| {
            if holder_group == group {
                Holder::Ours
            } else {
                Holder::Foreign
            }
        })
}

// A holder's group is read once, since a wait polls for as long as it lasts.
fn group_of(
    groups: &mut HashMap<u32, Option<u32>>,
    pid: u32,
    read: impl FnOnce(u32) -> Option<u32>,
) -> Option<u32> {
    *groups.entry(pid).or_insert_with(|| read(pid))
}

fn process_group(pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::mpsc::channel;
    use std::time::Instant;

    use super::*;
    use crate::lease::gpu::{Attempt, Claim};

    #[test]
    fn a_holders_group_is_read_once_per_pid() {
        let mut groups = HashMap::new();
        let reads = std::cell::Cell::new(0);
        let reads = &reads;
        let read = |group| {
            move |_| {
                reads.set(reads.get() + 1);
                group
            }
        };
        assert_eq!(group_of(&mut groups, 5, read(Some(9))), Some(9));
        assert_eq!(group_of(&mut groups, 5, read(Some(1))), Some(9));
        assert_eq!(group_of(&mut groups, 6, read(None)), None);
        assert_eq!(group_of(&mut groups, 6, read(Some(1))), None);
        assert_eq!(reads.get(), 2);
    }

    fn held_by(pid: u32, live: bool) -> Option<LockHolder> {
        Some(LockHolder {
            pid: Some(pid),
            live,
            what: String::new(),
            session: String::new(),
            since: None,
        })
    }

    #[test]
    fn a_holder_is_foreign_ours_or_neither() {
        let holder = |held, group: Option<u32>| holder_of(held, 7, &mut HashMap::new(), |_| group);
        let me = std::process::id();
        assert_eq!(holder(held_by(5, true), Some(9)), Holder::Foreign);
        assert_eq!(holder(held_by(5, true), Some(7)), Holder::Ours);
        assert_eq!(holder(None, Some(9)), Holder::Neither, "a free lock");
        assert_eq!(holder(held_by(5, false), Some(9)), Holder::Neither);
        assert_eq!(holder(held_by(5, true), None), Holder::Neither);
        assert_eq!(holder(held_by(me, true), Some(9)), Holder::Neither);
    }

    // Every report a watch makes over the holders it sees, one poll each.
    fn reports(holders: &[Holder]) -> Vec<RoundStage> {
        let mut queued = false;
        let mut told = Vec::new();
        for holder in holders {
            if let Some(stage) = change(queued, *holder) {
                told.push(stage);
                queued = stage == RoundStage::Queued;
            }
        }
        told
    }

    // The lock looks free for a poll between one holder letting go and the
    // next taking it, and the round has not run at any point in that gap.
    #[test]
    fn a_free_lock_between_holders_does_not_end_the_wait() {
        use Holder::{Foreign, Neither, Ours};
        assert_eq!(
            reports(&[Foreign, Neither, Foreign, Neither, Ours]),
            [RoundStage::Queued, RoundStage::Running]
        );
        assert_eq!(reports(&[Neither, Neither, Ours]), []);
        assert_eq!(reports(&[Foreign, Neither]), [RoundStage::Queued]);
    }

    // A watch that ends while queued leaves no round stuck in the queue,
    // and the next run of the command watches again from the start.
    #[test]
    fn a_watch_ended_while_queued_reports_running_and_the_next_starts_afresh() {
        let dir = tempfile::tempdir().unwrap();
        let lock = GpuLock::under(dir.path());
        let other = std::os::unix::process::parent_id();
        let claim = Claim {
            pid: other,
            what: "someone else's round".into(),
        };
        assert_eq!(lock.try_take(&claim).unwrap(), Attempt::Taken);
        let seen = Mutex::new(Vec::new());
        let watch = |stage| seen.lock().unwrap().push(stage);
        for _ in 0..2 {
            let (spawned, pid) = channel();
            std::thread::scope(|scope| {
                let (lock, watch) = (&lock, &watch);
                scope.spawn(move || watch_queue(lock, &pid, watch));
                // No process leads group 0, so the live holder is foreign.
                spawned.send(0).unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                while seen.lock().unwrap().len() % 2 == 0 {
                    assert!(Instant::now() < deadline, "never queued");
                    std::thread::sleep(Duration::from_millis(10));
                }
                drop(spawned);
            });
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                RoundStage::Queued,
                RoundStage::Running,
                RoundStage::Queued,
                RoundStage::Running
            ]
        );
    }
}
