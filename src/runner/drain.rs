//! Draining: a runner that starts no new call until it restarts
//!
//! `drain` holds back every call the runner would start next, a worker's
//! turn, a reviewer's session or local round, and the project manager's
//! wake, while the calls already in flight run to their end and the rest of
//! each pass goes on, merges included. `status` then lists the calls still
//! running, so `shep kelpie upgrade` can restart the runner once none is.
//! Draining is kept in memory only: a restart ends it, as `undrain` does.

use serde::Serialize;

use super::Runner;

/// What `status` shows while the runner drains
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Draining {
    /// The calls still running: each work item's, then the project manager's
    pub calls: Vec<CallRunning>,
    /// The longest a call the runner starts may run before it is ended, in
    /// seconds: the turn ceiling, or the project manager's or the issue
    /// writer's in flight when longer
    pub ceiling: u64,
}

/// A call in flight
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CallRunning {
    /// The issue of the work item it is for, and none for the project manager's
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<u64>,
    /// Whose call it is
    pub role: CallRole,
}

/// Whose call is in flight
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallRole {
    /// A worker's turn
    Worker,
    /// A reviewer's session or local round
    Reviewer,
    /// The project manager's wake
    Pm,
    /// The issue writer, labelling an issue
    IssueWriter,
}

impl Runner {
    /// Starts no new call from now until a restart or [`Runner::undrain`]
    pub(super) fn drain(&mut self) {
        self.draining = true;
    }

    /// Starts calls again
    pub(super) fn undrain(&mut self) {
        self.draining = false;
    }

    /// What `status` shows of the drain, while the runner drains
    pub(super) fn draining_status(&self) -> Option<Draining> {
        if !self.draining {
            return None;
        }
        let pm = self.agents.pm.is_some().then_some(super::pm::CEILING);
        let labelling = (self.flights.labelling()).map(|_| super::flight::label::CEILING);
        let retro = self
            .flights
            .retro_running()
            .then_some(super::retro::CEILING);
        let ceiling = (self.turn_ceiling().as_secs())
            .max(pm.unwrap_or(0))
            .max(labelling.unwrap_or(0))
            .max(retro.unwrap_or(0));
        Some(Draining {
            calls: self.flights.running(),
            ceiling,
        })
    }
}

#[cfg(test)]
mod tests;
