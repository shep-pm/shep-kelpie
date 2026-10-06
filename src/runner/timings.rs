//! Charging a work item's time to its phases
//!
//! Every save charges each open work item the time since its last charge,
//! to the phase its state put it in until now. Reads charge nothing.

use std::sync::Mutex;

use serde::Serialize;

use super::Runner;
use super::trigger::{issue_list, lock};
use crate::ports::{RoundStage, Timestamp};
use crate::state::{ProjectState, StateError};
use crate::work_item::{CallKind, Seconds, TimingPhase, Timings, WorkItem};

#[cfg(test)]
mod tests;

/// Seconds an open work item may go uncharged while the runner steps
///
/// The runner's loop beats on each pass, and passes while calls run in
/// flight, so a crash usually loses about this much plus a minute to `other`.
const HEARTBEAT: u64 = 30;

/// What `timings` answers
// wire format: changing this is a breaking change to the `timings` reply
#[derive(Debug, Serialize)]
pub struct Totals {
    /// The project
    pub project: String,
    /// How many finished work items the totals cover
    pub items: usize,
    /// Their issues, oldest first
    pub issues: Vec<u64>,
    /// Their wall time together
    pub wall: u64,
    /// Every phase together, summing to `wall`
    pub seconds: Seconds,
    /// The same as a plain-text table
    pub table: String,
}

impl Runner {
    // Charges each work item in `next` that is also open now, to the phase
    // the open one is in. A work item new to `next` has nothing to charge.
    pub(super) fn charge(&self, next: &mut ProjectState) {
        let now = self.ports.clock.now();
        for item in &mut next.work_items {
            let held = self.state.item(item.issue);
            if let (Some(held), Some(timings)) = (held, item.timings.as_mut()) {
                timings.charge(now, self.timing_phase(held));
            }
        }
    }

    // The phase `item`'s time counts in while the runner holds it as it is
    pub(super) fn timing_phase(&self, item: &WorkItem) -> TimingPhase {
        item.timing_phase(self.flights.runs_turn(item.issue))
    }

    /// The totals over the last `last` finished work items, or all of them
    pub fn totals(&self, last: usize) -> Totals {
        let history = &self.state.history;
        let taken = &history[history.len().saturating_sub(last)..];
        let mut seconds = Seconds::default();
        let mut wall = 0u64;
        for record in taken {
            wall = wall.saturating_add(record.wall);
            seconds.add_all(&record.seconds);
        }
        let issues: Vec<u64> = taken.iter().map(|r| r.issue).collect();
        Totals {
            project: self.project.as_str().to_owned(),
            items: taken.len(),
            table: table(&issues, wall, &seconds),
            issues,
            wall,
            seconds,
        }
    }

    /// Records that the local round in flight queues for the GPU, or no longer does
    ///
    /// # Errors
    ///
    /// [`StateError::Write`] when the change cannot be saved.
    pub(super) fn round_stage(&mut self, stage: RoundStage) -> Result<(), StateError> {
        let queued = stage == RoundStage::Queued;
        let changes = self.current().is_some_and(|item| {
            item.timings
                .as_ref()
                .is_some_and(|t| t.call == Some(CallKind::Local) && t.queued != queued)
        });
        if !changes {
            return Ok(());
        }
        self.update(|item| {
            if let Some(timings) = &mut item.timings {
                timings.queued = queued;
            }
        })
    }

    /// Saves the charge when a work item has gone [`HEARTBEAT`] seconds without one
    ///
    /// A save that fails is told and let go: the same time is charged by the
    /// next save, so the step carries on.
    pub(super) fn beat(&mut self) {
        let now = self.ports.clock.now();
        let stale = |item: &WorkItem| {
            let since = item.timings.as_ref().map_or(now.0, |t| t.since.0);
            now.0.saturating_sub(since) >= HEARTBEAT
        };
        if !self.state.work_items.iter().any(stale) {
            return;
        }
        if let Err(e) = self.save_time() {
            eprintln!("cannot save the work items' time: {e}");
        }
    }

    /// Charges and saves every open work item's time as it stands
    ///
    /// # Errors
    ///
    /// [`StateError::Write`] when the change cannot be saved.
    pub(super) fn save_time(&mut self) -> Result<(), StateError> {
        self.save(self.state.clone())
    }
}

fn row(name: &str, seconds: u64, wall: u64) -> String {
    let time = format!(
        "{}h{:02}m{:02}s",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    );
    let share = if wall == 0 {
        "-".to_owned()
    } else {
        format!("{:.1}%", 100.0 * seconds as f64 / wall as f64)
    };
    format!("{name:<20}{seconds:>8}  {time:>9}  {share:>6}")
}

fn table(issues: &[u64], wall: u64, seconds: &Seconds) -> String {
    let title = match issues.len() {
        0 => "no finished work items".to_owned(),
        1 => format!("1 work item: {}", issue_list(issues)),
        n => format!("{n} work items: {}", issue_list(issues)),
    };
    let head = format!(
        "{:<20}{:>8}  {:>9}  {:>6}",
        "phase", "seconds", "duration", "share"
    );
    let phases = TimingPhase::ALL
        .iter()
        .map(|p| row(p.name(), seconds.get(*p), wall));
    let total = row("total", wall, wall);
    std::iter::once(title)
        .chain(std::iter::once(head))
        .chain(phases)
        .chain(std::iter::once(total))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Charges and saves every open work item's time now
///
/// Run as the runner stops, so the time until the next start is `other`.
///
/// # Errors
///
/// [`StateError::Write`] when the change cannot be saved.
pub fn settle(runner: &Mutex<Runner>) -> Result<(), StateError> {
    let mut runner = lock(runner);
    if runner.state.work_items.is_empty() {
        return Ok(());
    }
    let next = runner.state.clone();
    runner.save(next)
}

/// Readies the work items a state file holds for a runner starting at `now`
///
/// One with no timings counts from now. One with timings charges the time
/// since its last charge to `other`, as nothing was running it. Returns
/// whether anything changed.
pub(super) fn reload(state: &mut ProjectState, now: Timestamp) -> bool {
    for item in &mut state.work_items {
        match &mut item.timings {
            Some(timings) => timings.charge(now, TimingPhase::Other),
            None => item.timings = Some(Timings::starting(now)),
        }
    }
    !state.work_items.is_empty()
}
