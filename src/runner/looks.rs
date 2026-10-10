//! How often a pass reads the forge
//!
//! The loop wakes for every call's news and every trigger that asks for
//! work, so a pass is no measure of time. The board is read at most once a
//! [`BOARD_POLL`], and at once after a change to what it would see: a work
//! item opening or ending, a ruling leaving the list, the adopted pull
//! requests queued changing, `finishing` ending, the project manager
//! answering, the issue writer's call ending, and the board's briefing
//! reading paths it had not compared. A step that fails waits [`RETRY_FIRST`] before it runs again,
//! twice as long after each failure in a row up to [`RETRY_MAX`], and no
//! less than the forge's rate-limit hold. Any other outcome ends the wait.

use std::collections::BTreeMap;
use std::time::Duration;

use super::Runner;
use super::alert::backoff;
use super::report::{Begin, StepReport};
use super::turn::Slot;
use crate::ports::Timestamp;
use crate::state::ProjectState;

/// How often the board is read, however often the loop wakes
///
/// A read is two `gh` calls, 120 an hour, against GitHub's 5,000 an hour
/// for the maintainer's login.
pub const BOARD_POLL: Duration = Duration::from_secs(60);

/// Seconds a failed step waits before it runs again
const RETRY_FIRST: u64 = 15;

/// The longest a step that keeps failing waits, in seconds
const RETRY_MAX: u64 = 10 * 60;

/// When the board was last read and each failing step runs again, in memory only
#[derive(Debug, Default)]
pub(super) struct Looks {
    board_read: Option<Timestamp>,
    // Whether something changed what the board would see since its last read
    board_moved: bool,
    // Each failing step by its slot: its failures in a row, and when it runs again
    retries: BTreeMap<Slot, (u32, Timestamp)>,
}

impl Looks {
    /// Whether `slot`'s step may run at `now`
    pub(super) fn due(&self, slot: Slot, now: Timestamp) -> bool {
        let waited = (self.retries.get(&slot)).is_none_or(|&(_, at)| now >= at);
        let looked = match slot {
            Slot::Board => self.board_moved || self.board_due_at().is_none_or(|at| now >= at),
            Slot::Item(_) => true,
        };
        waited && looked
    }

    /// Records that the board is read at `now`, before the read, so a
    /// change the read itself makes lets the next pass read it again
    pub(super) fn reading_board(&mut self, now: Timestamp) {
        self.board_read = Some(now);
        self.board_moved = false;
    }

    /// Records what `slot`'s step began at `now`, with the forge held until `held`
    pub(super) fn stepped(
        &mut self,
        slot: Slot,
        begin: &Begin,
        now: Timestamp,
        held: Option<Timestamp>,
    ) {
        let failed = matches!(
            begin,
            Begin::Report(
                StepReport::GateFailed { .. }
                    | StepReport::BoardFailed { .. }
                    | StepReport::ParentCloseFailed { .. }
            )
        );
        if !failed {
            self.retries.remove(&slot);
            return;
        }
        let failures = self.retries.get(&slot).map_or(0, |&(n, _)| n) + 1;
        let wait = backoff(RETRY_FIRST, failures, RETRY_MAX);
        let at = Timestamp(now.0.saturating_add(wait)).max(held.unwrap_or(now));
        self.retries.insert(slot, (failures, at));
    }

    /// Lets the board be read on the next pass
    pub(super) fn board_moved(&mut self) {
        self.board_moved = true;
    }

    /// How long until the next read of the board or retry of a failed step,
    /// or `None` with neither waiting
    pub(super) fn next_due(&self, now: Timestamp) -> Option<Duration> {
        let retries = self.retries.values().map(|&(_, at)| at);
        (retries.chain(self.board_due_at()))
            .min()
            .map(|at| Duration::from_secs(at.0.saturating_sub(now.0)))
    }

    fn board_due_at(&self) -> Option<Timestamp> {
        let poll = BOARD_POLL.as_secs();
        (self.board_read).map(|at| Timestamp(at.0.saturating_add(poll)))
    }
}

/// Whether a save from `before` to `after` changes what the board would see
pub(super) fn board_moved(before: &ProjectState, after: &ProjectState) -> bool {
    let issues = |s: &ProjectState| s.work_items.iter().map(|i| i.issue).collect::<Vec<_>>();
    let ruled = (before.rulings.iter()).any(|r| !after.rulings.iter().any(|a| a.id == r.id));
    issues(before) != issues(after)
        || ruled
        || before.adopted != after.adopted
        || (before.finishing && !after.finishing)
}

impl Runner {
    /// How long until the runner next reads the board or retries a failed
    /// step, or `None` with neither waiting
    pub fn next_look(&self) -> Option<Duration> {
        self.looks.next_due(self.ports.clock.now())
    }
}

#[cfg(test)]
mod tests;
