//! What wakes the project manager besides a pick and a `tell`: a work item
//! parked on a stuck item's ruling, a pair of open branches newly in
//! conflict, and, once it is back, whatever it missed while it was not

use std::collections::BTreeMap;

use super::{Pick, Wake};
use crate::runner::Runner;
use crate::state::{ProjectState, RulingKind, Stuck};
use crate::work_item::Phase;

impl Runner {
    // Queues a wake for each item still stuck and each conflict still open,
    // once it is back after giving some up.
    pub(super) fn still_due(&mut self) {
        let mut wakes = Vec::new();
        for item in &self.state.work_items {
            let issue = item.issue;
            let parked = match item.phase {
                Phase::Ruling { id } => self.state.rulings.iter().find(|r| r.id == id),
                _ => None,
            };
            if let Some(wake) = parked.and_then(|r| stuck_wake(issue, r.id, &r.kind)) {
                wakes.push(wake);
            } else if self.flights.watched(issue).is_some_and(|w| w.idle) {
                let what = "its worker has shown no tool call or output for 10 minutes or more";
                wakes.push(Wake::Stuck(issue, what.to_owned()));
            }
        }
        for (&(a, b), files) in &self.pm.conflicts {
            wakes.push(Wake::Conflict(a, b, files.clone()));
        }
        for wake in wakes {
            self.pm_due(wake);
        }
    }

    /// Notes what saving `now` changes that wakes the project manager: a
    /// work item parked on a stuck ruling, or one closing, which lets its
    /// holds go
    pub(in crate::runner) fn pm_noticed(&mut self, now: &ProjectState) {
        if self.agents.pm.is_none() {
            return;
        }
        let was = &self.state;
        let raised = (now.rulings.iter()).filter(|r| !was.rulings.iter().any(|w| w.id == r.id));
        let stuck: Vec<Wake> = raised
            .filter_map(|ruling| {
                let issue = ruling.issue?;
                let ended = self.pm.after_end.contains_key(&issue);
                stuck_wake(issue, ruling.id, &ruling.kind).filter(|_| !ended)
            })
            .collect();
        let closed = (was.work_items.iter()).any(|i| now.item(i.issue).is_none());
        let opened = (now.work_items.iter()).any(|i| was.item(i.issue).is_none());
        for wake in stuck {
            self.pm_due(wake);
        }
        // A pick was made on a board that has changed since.
        if (opened || closed) && matches!(self.pm.pick, Some(Pick::Issue(_))) {
            self.pm.pick = None;
        }
        if closed {
            self.pm.held.clear();
            if matches!(self.pm.pick, Some(Pick::Nothing(_))) {
                self.pm.pick = None;
            }
            let open = now.open_issues();
            self.pm.after_end.retain(|issue, _| open.contains(issue));
            (self.pm.conflicts).retain(|(a, b), _| open.contains(a) && open.contains(b));
        }
    }

    /// Wakes the project manager for each pair of open branches newly in
    /// conflict, given every pair in conflict now
    pub(in crate::runner) fn pm_conflicts(&mut self, now: BTreeMap<(u64, u64), Vec<String>>) {
        if self.agents.pm.is_none() {
            return;
        }
        for (&(a, b), files) in &now {
            if !self.pm.conflicts.contains_key(&(a, b)) {
                self.pm_due(Wake::Conflict(a, b, files.clone()));
            }
        }
        self.pm.conflicts = now;
    }
}

// The wake for `issue`, parked on ruling `id` of `kind`, if that is a stuck item's.
fn stuck_wake(issue: u64, id: u64, kind: &RulingKind) -> Option<Wake> {
    let what = stuck_on(kind)?;
    Some(Wake::Stuck(
        issue,
        format!("{what}, and ruling {id} asks the maintainer"),
    ))
}

/// What a ruling of `kind` says of a stuck work item, or none for one that
/// is not about a stuck item
pub(in crate::runner) fn stuck_on(kind: &RulingKind) -> Option<&'static str> {
    match kind {
        RulingKind::Stuck(Stuck::TurnFailed { .. }) => {
            Some("its worker's turn failed or stopped short twice")
        }
        RulingKind::Stuck(Stuck::TurnTimeout { .. }) => {
            Some("its worker's turn ran past its ceiling")
        }
        RulingKind::Stuck(Stuck::StillRed { .. }) => {
            Some("CI failed again and the worker pushed no fix")
        }
        _ => None,
    }
}
