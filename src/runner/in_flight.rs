//! The calls in flight as the usage ledger knows them, shared outside the
//! runner's lock
//!
//! Each call's draft goes in as it launches and comes out when the runner
//! hears it end, which writes its line. A runner that stops hears no more
//! ends, so the stop takes every draft left and writes each a `stopped`
//! line, even while a pass still holds the runner's lock. Whichever takes a
//! draft first writes its line, so none is written twice.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use crate::ports::{Cost, Role, Timestamp};
use crate::usage::{CallLine, Draft, Ended, Line, PacerLine, append_to};

/// One call in flight
#[derive(Debug, Clone)]
pub(super) struct Open {
    /// Its line as it started
    pub draft: Draft,
    /// Each account's pacer reading as it started
    pub pacer: BTreeMap<String, PacerLine>,
    /// When a local round began to run on the GPU, past any queue
    pub ran_from: Option<Timestamp>,
    // Whether it ended before reaching a model, unheard as yet
    no_model: bool,
}

impl Open {
    /// Its line, ended at `at` as `ended` with nothing reported, as for a
    /// call that was stopped, timed out, failed or panicked
    pub fn unreported(&self, at: Timestamp, ended: Ended) -> CallLine {
        let mut line = self
            .draft
            .line(at, ended, None, Cost(0), self.pacer.clone());
        if self.draft.session().is_none() {
            line.gpu_seconds = gpu_seconds(self.ran_from, at, line.seconds);
        }
        line
    }
}

/// A local round's seconds on the GPU: from when it began to run, or the
/// whole call when it was never told queued
pub(super) fn gpu_seconds(
    ran_from: Option<Timestamp>,
    at: Timestamp,
    seconds: Option<u64>,
) -> Option<u64> {
    match ran_from {
        Some(from) => Some(at.0.saturating_sub(from.0)),
        None => seconds,
    }
}

// Keyed by the work item's issue, and none for the project manager's call
#[derive(Debug, Default)]
struct Calls {
    open: BTreeMap<Option<u64>, Open>,
    closed: bool,
}

/// The calls in flight, shared between the runner and its stop
#[derive(Debug, Clone, Default)]
pub(super) struct InFlight(Arc<Mutex<Calls>>);

impl InFlight {
    fn calls(&self) -> std::sync::MutexGuard<'_, Calls> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds the call `key` launched as `draft`, unless the runner is stopping
    pub fn open(&self, key: Option<u64>, draft: Draft, pacer: BTreeMap<String, PacerLine>) {
        let mut calls = self.calls();
        if !calls.closed {
            let open = Open {
                draft,
                pacer,
                ran_from: None,
                no_model: false,
            };
            calls.open.insert(key, open);
        }
    }

    /// Marks that `key`'s local round began to run at `at`
    pub fn ran(&self, key: Option<u64>, at: Timestamp) {
        if let Some(open) = self.calls().open.get_mut(&key) {
            open.ran_from.get_or_insert(at);
        }
    }

    /// Marks that `key`'s call ended before it reached a model
    pub fn no_model(&self, key: Option<u64>) {
        if let Some(open) = self.calls().open.get_mut(&key) {
            open.no_model = true;
        }
    }

    /// Takes `key`'s call as its end is heard, or `None` once the stop has
    pub fn take(&self, key: Option<u64>) -> Option<Open> {
        self.calls().open.remove(&key)
    }
}

/// What a stopping runner needs to write the lines of its calls in flight
#[derive(Debug, Clone)]
pub struct Stopping {
    calls: InFlight,
    ledger: PathBuf,
}

impl Stopping {
    pub(super) fn new(calls: InFlight, ledger: PathBuf) -> Self {
        Self { calls, ledger }
    }

    /// Writes a `stopped` line at `at` for each call still in flight that
    /// reached a model, and returns each session's issue and role, the
    /// project manager's and local rounds left out
    ///
    /// Takes no lock of the runner's, so it runs while a pass holds it. One
    /// append each; a line that cannot be written is told and let go.
    pub fn record(&self, at: Timestamp) -> Vec<(u64, Role)> {
        let open = {
            let mut calls = self.calls.calls();
            calls.closed = true;
            std::mem::take(&mut calls.open)
        };
        let mut stopped = Vec::new();
        for (key, open) in open.into_iter().filter(|(_, open)| !open.no_model) {
            let line = open.unreported(at, Ended::Stopped);
            let (role, session) = (line.role, open.draft.session().is_some());
            if let Err(e) = append_to(&self.ledger, &Line::Call(line)) {
                eprintln!(
                    "cannot add a stopped call to {}: {e}",
                    self.ledger.display()
                );
                continue;
            }
            // A local round is counted as a round, not as a reviewer's call.
            if let (Some(issue), true) = (key, session) {
                stopped.push((issue, role));
            }
        }
        stopped
    }
}
