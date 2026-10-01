//! The fake forge's merge queue

use std::sync::atomic::Ordering;

use super::FakeForge;
use crate::ports::{PullRequestState, QueueStanding};

impl FakeForge {
    /// Turns the repo's merge queue on: a merge then queues the pull request
    pub(crate) fn set_merge_queue(&self, on: bool) {
        self.queue_on.store(on, Ordering::SeqCst);
    }

    /// Lets the queue merge pull request `number`, as it does once the checks
    /// on top of the pull requests ahead of it pass
    pub(crate) fn queue_merges(&self, number: u64) {
        self.queue
            .lock()
            .unwrap()
            .entry(number)
            .or_insert_with(not_queued)
            .queued = false;
        self.set_state(number, PullRequestState::Merged);
    }

    /// Arms auto-merge on pull request `number`, as a merge call does when
    /// the queue is required and the pull request cannot be queued yet
    pub(crate) fn arm_auto_merge(&self, number: u64) {
        self.queue
            .lock()
            .unwrap()
            .entry(number)
            .or_insert_with(not_queued)
            .armed = true;
    }

    /// The pull requests whose auto-merge kelpie disarmed, in order
    pub(crate) fn disarmed(&self) -> Vec<u64> {
        self.disarmed.lock().unwrap().clone()
    }

    /// Puts pull request `number` in the queue, as a merge call that the
    /// runner never saw answered would have
    pub(crate) fn queue_enqueues(&self, number: u64) {
        self.queue
            .lock()
            .unwrap()
            .entry(number)
            .or_insert_with(not_queued)
            .queued = true;
    }

    /// Drops pull request `number` from the queue with no removal on record
    pub(crate) fn queue_forgets(&self, number: u64) {
        self.queue
            .lock()
            .unwrap()
            .entry(number)
            .or_insert_with(not_queued)
            .queued = false;
    }

    /// Has the queue remove pull request `number` unmerged, for `reason`
    pub(crate) fn queue_removes(&self, number: u64, reason: &str) {
        let mut queue = self.queue.lock().unwrap();
        let standing = queue.entry(number).or_insert_with(not_queued);
        standing.queued = false;
        standing.removals += 1;
        standing.reason = Some(reason.to_owned());
    }
}

impl FakeForge {
    // What the `Forge` impl answers for the queue
    pub(super) fn standing(&self, number: u64) -> QueueStanding {
        self.queue
            .lock()
            .unwrap()
            .get(&number)
            .cloned()
            .unwrap_or_else(not_queued)
    }

    pub(super) fn disarm(&self, number: u64) {
        self.queue
            .lock()
            .unwrap()
            .entry(number)
            .or_insert_with(not_queued)
            .armed = false;
        self.disarmed.lock().unwrap().push(number);
    }

    pub(super) fn enters_queue(&self, number: u64) {
        self.queue
            .lock()
            .unwrap()
            .entry(number)
            .or_insert_with(not_queued)
            .queued = true;
    }
}

pub(super) fn not_queued() -> QueueStanding {
    QueueStanding {
        queued: false,
        armed: false,
        removals: 0,
        reason: None,
    }
}
