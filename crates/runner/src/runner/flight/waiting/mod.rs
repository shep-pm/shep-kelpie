//! A worker's turn waiting for its model
//!
//! A turn waits while it asks for a lease on its model, while its gateway
//! answers busy, or while it has shown no output for ten minutes. Its first
//! output clears the mark. A wait moves no work item: the agent board's
//! handoff, asked before a due turn starts, is what moves one.

use std::collections::BTreeMap;

use serde::Serialize;

use super::super::Runner;
use super::super::report::ReviewCall;
use super::Launch;
use crate::ports::{CallActivity, Timestamp, Wait};
use crate::settings::{GatewayName, LocalRound, ModelHost};
use crate::ticket::TicketId;

/// How long a turn shows no output before it is marked waiting, in seconds
const SILENT_AFTER: u64 = 10 * 60;

/// A worker's turn waiting for its model, as `status` and the board show it
// wire format: changing this is a breaking change to `status`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Waiting {
    /// When it began to wait
    pub since: Timestamp,
    /// What it waits for
    pub why: Wait,
}

impl Runner {
    /// `ticket`'s worker turn in flight, if it waits for its model
    pub(in crate::runner) fn model_wait(&self, ticket: TicketId) -> Option<Waiting> {
        if !self.flights.flying.contains_key(&ticket) {
            return self.unreachable.waiting(ticket);
        }
        let waiting = self.quiet_since(ticket)?;
        if waiting.why != Wait::Silent {
            return Some(waiting);
        }
        let now = self.ports.clock.now();
        let quiet = now.0.saturating_sub(waiting.since.0);
        (quiet >= SILENT_AFTER).then_some(waiting)
    }

    // Since when `ticket`'s worker turn in flight has shown no output, and
    // what it said it waits for, however short the wait. `None` once it
    // has shown output, or when its harness keeps no record to read.
    fn quiet_since(&self, ticket: TicketId) -> Option<Waiting> {
        let flight = self.flights.flying.get(&ticket)?;
        flight.deadline?;
        let call = flight.watched.call.as_ref()?;
        let started = flight.watched.started;
        match self.ports.agents.last_active(call) {
            CallActivity::At(at) if at >= started => return None,
            CallActivity::Untracked if flight.watched.told.is_none() => return None,
            _ => {}
        }
        if let Some((since, why)) = flight.watched.told {
            return Some(Waiting { since, why });
        }
        Some(Waiting {
            since: flight.watched.granted.unwrap_or(started),
            why: Wait::Silent,
        })
    }

    // Records that `ticket`'s call waits for `wait`, keeping when it first did.
    pub(super) fn told(&mut self, ticket: TicketId, wait: Wait) {
        let now = self.ports.clock.now();
        if let Some(flight) = self.flights.flying.get_mut(&ticket) {
            let since = flight.watched.told.map_or(now, |(since, _)| since);
            flight.watched.told = Some((since, wait));
        }
        self.model_leases();
        self.board_changed();
    }

    // Sets each model's gauges of the calls waiting for, and holding, a lease on it. A model
    // whose calls have all ended is set to 0 rather than left at its last count.
    pub(in crate::runner) fn model_leases(&mut self) {
        let mut standing: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for flight in self.flights.flying.values() {
            let Some((_, model)) = &flight.watched.lease else {
                continue;
            };
            let (held, waiting) = standing.entry(model).or_default();
            match (flight.watched.told, flight.watched.granted) {
                (Some(_), _) => *waiting += 1,
                (None, Some(_)) => *held += 1,
                (None, None) => {}
            }
        }
        for model in &self.flights.leased_models {
            standing.entry(model).or_default();
        }
        for (model, (held, waiting)) in &standing {
            self.metrics.model_lease_standing(model, *held, *waiting);
        }
        let named = standing.keys().map(|model| (*model).to_owned()).collect();
        self.flights.leased_models = named;
    }
}

/// The gateway and the model `launch`'s call holds a lease on, `None` when its
/// harness or its round holds none
///
/// A gateway lease is what a call queues behind, so it is what a long wait
/// waits on.
pub(super) fn lease_of(launch: &Launch) -> Option<(GatewayName, String)> {
    let (host, model) = match launch {
        Launch::Turn { call, .. } | Launch::Review(ReviewCall::Session(call)) => {
            // parity: harness.local-model
            let server = call.harness.server()?;
            (server.host.clone(), call.model.clone())
        }
        Launch::Review(ReviewCall::Local {
            local: LocalRound::Endpoint(endpoint),
            ..
        }) => (endpoint.host.clone(), endpoint.model.as_str().to_owned()),
        Launch::Review(ReviewCall::Local {
            local: LocalRound::Command(_),
            ..
        }) => return None,
    };
    match host {
        ModelHost::Gateway(name) => Some((name, model)),
        ModelHost::Url(_) => None,
    }
}

#[cfg(test)]
mod tests;
