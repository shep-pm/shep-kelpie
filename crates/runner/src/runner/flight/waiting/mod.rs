//! A worker's turn waiting for its model, and the fallback that moves one
//!
//! A turn waits while it asks for a lease on its model, while its gateway
//! answers busy, or while it has shown no output for `agents.fallback_after`
//! (ten minutes with that off). Its first output clears the mark. A work
//! item's first turn that waited `agents.fallback_after` is stopped and moves
//! to the next implementer listed that no other item waits on. It has no
//! session yet, so nothing is lost. A later turn, one pinned by a `!` label,
//! or one whose own agent the project does not list, never moves. A new work
//! item opens on the next free one when its label's model has a queue.

use std::collections::BTreeMap;

use serde::Serialize;

use super::super::Runner;
use super::super::report::{ReviewCall, StepReport};
use super::Launch;
use crate::ports::{AgentError, AgentReply, CallActivity, SessionId, Timestamp, Wait};
use crate::settings::{AgentName, GatewayKind, GatewayName, LocalRound, ModelHost};
use crate::state::StateError;
use crate::ticket::TicketId;
use crate::work_item::{Turn, WorkItem, new_session_id};

/// How long a turn shows no output before it is marked waiting, in seconds,
/// with `agents.fallback_after` off
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

/// Where a first turn that waited too long goes
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Fallback {
    /// Nowhere yet
    Stays,
    /// To this implementer, on this new session, once its call has stopped
    Moving(AgentName, SessionId),
    /// Nowhere: no implementer after its own was free, so it goes on waiting
    Nowhere,
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
        (quiet >= self.silent_after()).then_some(waiting)
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

    // Seconds a turn shows nothing before it is marked waiting.
    fn silent_after(&self) -> u64 {
        let after = self.settings.agents.fallback_after;
        after.map_or(SILENT_AFTER, |m| u64::from(m.get()) * 60)
    }

    // When the soonest first turn in flight that still shows nothing could
    // fall back, so the loop wakes for it. A turn with output, or one that
    // cannot move, sets none: its deadline would pass and wake the loop at once.
    pub(super) fn fallback_due(&self) -> Option<Timestamp> {
        let after = self.settings.agents.fallback_after?;
        (self.flights.flying.iter())
            .filter(|(_, f)| f.first && f.fallback == Fallback::Stays && !f.ending.asked())
            .filter(|(ticket, _)| {
                self.state
                    .item(**ticket)
                    .is_some_and(|item| self.can_fall_back(item))
            })
            .filter_map(|(&ticket, _)| self.quiet_since(ticket))
            .map(|quiet| Timestamp(quiet.since.0.saturating_add(u64::from(after.get()) * 60)))
            .min()
    }

    // Stops each first turn that has waited `agents.fallback_after`, so it
    // can move to the next implementer listed.
    pub(super) fn fall_back(&mut self) {
        let Some(after) = self.settings.agents.fallback_after else {
            return;
        };
        let now = self.ports.clock.now();
        let due: Vec<(TicketId, AgentName)> = (self.flights.flying.iter())
            .filter(|(_, f)| f.first && f.fallback == Fallback::Stays && !f.ending.asked())
            .filter_map(|(&ticket, _)| {
                let waiting = self.model_wait(ticket)?;
                let waited = now.0.saturating_sub(waiting.since.0);
                let item = self.state.item(ticket)?;
                (waited >= u64::from(after.get()) * 60 && self.can_fall_back(item))
                    .then(|| (ticket, item.agent.clone()))
            })
            .collect();
        for (ticket, from) in due {
            let to = self.next_free(ticket, &from);
            // Drawn before the call stops, so a move never fails half done.
            let session = match to.as_ref().map(|_| new_session_id()) {
                Some(Err(e)) => {
                    self.notes.push(format!(
                        "#{ticket}: cannot draw a session id to move its first turn: {e}"
                    ));
                    continue;
                }
                Some(Ok(session)) => Some(session),
                None => None,
            };
            let Some(flight) = self.flights.flying.get_mut(&ticket) else {
                continue;
            };
            match to.zip(session) {
                Some((to, session)) => {
                    flight.ending.end();
                    flight.fallback = Fallback::Moving(to, session);
                }
                None => {
                    flight.fallback = Fallback::Nowhere;
                    self.notes.push(format!(
                        "#{ticket}: its first turn on {from} waits for its model, and no \
                         implementer listed after {from} is free to take it, so it waits on"
                    ));
                }
            }
        }
    }

    // Whether `item`'s first turn may move to another implementer: neither its
    // label nor the project manager's pick pins it, and the project lists its agent.
    fn can_fall_back(&self, item: &WorkItem) -> bool {
        !item.pinned && self.agents.implementer(&item.agent).is_some()
    }

    // The implementer a new work item for `ticket`, labelled for `from`, opens on
    // instead: `None` while `agents.fallback_after` is off, or no first turn of
    // this runner waits on `from`'s model and its gateway has no queue, or
    // `next_free` finds none. A silent turn never counts.
    pub(in crate::runner) fn opens_elsewhere(
        &self,
        ticket: TicketId,
        from: &AgentName,
    ) -> Option<AgentName> {
        self.settings.agents.fallback_after?;
        let waits_on_from = (self.state.work_items.iter())
            .filter(|item| item.agent == *from)
            .filter(|item| {
                self.flights
                    .flying
                    .get(&item.issue)
                    .is_some_and(|f| f.first)
            })
            .filter_map(|item| self.model_wait(item.issue))
            .any(|waiting| matches!(waiting.why, Wait::Lease | Wait::Busy));
        if !waits_on_from && !self.gateway_queued(from) {
            return None;
        }
        self.next_free(ticket, from)
    }

    // A failed read leaves the implementer available, as a gateway without leases does.
    fn gateway_queued(&self, name: &AgentName) -> bool {
        let Some(agent) = self.agents.implementer(name) else {
            return false;
        };
        let Some(server) = agent.model.harness.server() else {
            return false;
        };
        let ModelHost::Gateway(gateway) = &server.host else {
            return false;
        };
        // parity: gateway.lease-availability
        if self.gateways.get(gateway).map(|g| g.kind) != Some(GatewayKind::Paddock) {
            return false;
        }
        self.gateways.upstream(&server.host).ok().is_some_and(|up| {
            self.ports
                .local_leases
                .paddock_queued(&up, agent.model.model.as_str())
        })
    }

    // The implementer listed after `from` that no other turn waits on, or
    // is moving to.
    fn next_free(&self, ticket: TicketId, from: &AgentName) -> Option<AgentName> {
        let listed = self.agents.implementer_names();
        // An agent no longer listed has no implementers after it.
        let after = listed.iter().position(|n| n == from)? + 1;
        let waited_on = (self.state.work_items.iter())
            .filter(|i| i.issue != ticket && self.model_wait(i.issue).is_some())
            .map(|i| &i.agent);
        let moving_to = (self.flights.flying.values()).filter_map(|f| match &f.fallback {
            Fallback::Moving(to, _) => Some(to),
            Fallback::Stays | Fallback::Nowhere => None,
        });
        let busy: Vec<&AgentName> = waited_on.chain(moving_to).collect();
        listed[after..]
            .iter()
            .find(|name| !busy.contains(name) && !self.gateway_queued(name))
            .cloned()
    }

    // Moves `ticket`'s work item to the implementer its fallback chose, once
    // the turn it stopped has ended, with a new session for its first turn.
    // `None` when it was not moving, or its turn answered after all.
    pub(super) fn fell_back(
        &mut self,
        ticket: TicketId,
        result: &Result<AgentReply, AgentError>,
    ) -> Option<Result<Option<StepReport>, StateError>> {
        let flight = self.flights.flying.get(&ticket)?;
        let Fallback::Moving(to, session) = &flight.fallback else {
            return None;
        };
        if matches!(result, Ok(_) | Err(AgentError::Stopped)) {
            return None;
        }
        let (to, session) = (to.clone(), session.clone());
        let waited = self.model_wait(ticket).map(|w| w.since);
        let now = self.ports.clock.now();
        // Saved while its flight still runs, so the wait is charged as the
        // worker's time, as any turn's is.
        let mut next = self.state.clone();
        let item = next.item_mut(ticket)?;
        let from = std::mem::replace(&mut item.agent, to.clone());
        item.session = session;
        item.turn = Turn::Due;
        let issue = item.issue;
        let since = waited.unwrap_or(flight.watched.started);
        let report = StepReport::FellBack {
            issue,
            from,
            to,
            waited: now.0.saturating_sub(since.0),
        };
        Some(self.save(next).map(|()| Some(report)))
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
