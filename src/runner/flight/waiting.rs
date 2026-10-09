//! A worker's turn waiting for its model, and the fallback that moves one
//!
//! A turn waits while it asks for a lease on its model, while its gateway
//! answers busy, or while it has shown no output for `agents.fallback_after`
//! (ten minutes with that off). Its first output clears the mark. A work
//! item's first turn that waited `agents.fallback_after` is stopped and moves
//! to the next implementer listed that no other item waits on. It has no
//! session yet, so nothing is lost. A later turn, or one pinned by a `!`
//! label, never moves.

use serde::Serialize;

use super::super::Runner;
use super::super::report::StepReport;
use crate::ports::{AgentError, AgentReply, CallActivity, SessionId, Timestamp, Wait};
use crate::settings::AgentName;
use crate::state::StateError;
use crate::work_item::{Turn, new_session_id};

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
    /// `issue`'s worker turn in flight, if it waits for its model
    pub(in crate::runner) fn model_wait(&self, issue: u64) -> Option<Waiting> {
        let waiting = self.quiet_since(issue)?;
        if waiting.why != Wait::Silent {
            return Some(waiting);
        }
        let now = self.ports.clock.now();
        let quiet = now.0.saturating_sub(waiting.since.0);
        (quiet >= self.silent_after()).then_some(waiting)
    }

    // Since when `issue`'s worker turn in flight has shown no output, and
    // what it said it waits for, however short the wait. `None` once it
    // has shown output, or when its harness keeps no record to read.
    fn quiet_since(&self, issue: u64) -> Option<Waiting> {
        let flight = self.flights.flying.get(&issue)?;
        flight.deadline?;
        let call = flight.watched.call.as_ref()?;
        let started = flight.watched.started;
        match self.ports.agents.last_active(call) {
            CallActivity::At(at) if at >= started => return None,
            CallActivity::Untracked if flight.told.is_none() => return None,
            _ => {}
        }
        if let Some((since, why)) = flight.told {
            return Some(Waiting { since, why });
        }
        Some(Waiting {
            since: flight.began_at.unwrap_or(started),
            why: Wait::Silent,
        })
    }

    // Records that `issue`'s call waits for `wait`, keeping when it first did.
    pub(super) fn told(&mut self, issue: u64, wait: Wait) {
        let now = self.ports.clock.now();
        if let Some(flight) = self.flights.flying.get_mut(&issue) {
            let since = flight.told.map_or(now, |(since, _)| since);
            flight.told = Some((since, wait));
        }
        self.board_changed();
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
            .filter(|(issue, _)| self.state.item(**issue).is_some_and(|i| !i.pinned))
            .filter_map(|(&issue, _)| self.quiet_since(issue))
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
        let due: Vec<(u64, AgentName)> = (self.flights.flying.iter())
            .filter(|(_, f)| f.first && f.fallback == Fallback::Stays && !f.ending.asked())
            .filter_map(|(&issue, _)| {
                let waiting = self.model_wait(issue)?;
                let waited = now.0.saturating_sub(waiting.since.0);
                let item = self.state.item(issue)?;
                (waited >= u64::from(after.get()) * 60 && !item.pinned)
                    .then(|| (issue, item.agent.clone()))
            })
            .collect();
        for (issue, from) in due {
            let to = self.next_free(issue, &from);
            // Drawn before the call stops, so a move never fails half done.
            let session = match to.as_ref().map(|_| new_session_id()) {
                Some(Err(e)) => {
                    self.notes.push(format!(
                        "#{issue}: cannot draw a session id to move its first turn: {e}"
                    ));
                    continue;
                }
                Some(Ok(session)) => Some(session),
                None => None,
            };
            let Some(flight) = self.flights.flying.get_mut(&issue) else {
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
                        "#{issue}: its first turn on {from} waits for its model, and no \
                         implementer listed after {from} is free to take it, so it waits on"
                    ));
                }
            }
        }
    }

    // The implementer listed after `from` that no other turn waits on, or
    // is moving to.
    fn next_free(&self, issue: u64, from: &AgentName) -> Option<AgentName> {
        let listed = self.agents.implementer_names();
        // An agent no longer listed has no implementers after it.
        let after = listed.iter().position(|n| n == from)? + 1;
        let waited_on = (self.state.work_items.iter())
            .filter(|i| i.issue != issue && self.model_wait(i.issue).is_some())
            .map(|i| &i.agent);
        let moving_to = (self.flights.flying.values()).filter_map(|f| match &f.fallback {
            Fallback::Moving(to, _) => Some(to),
            Fallback::Stays | Fallback::Nowhere => None,
        });
        let busy: Vec<&AgentName> = waited_on.chain(moving_to).collect();
        listed[after..]
            .iter()
            .find(|name| !busy.contains(name))
            .cloned()
    }

    // Moves `issue`'s work item to the implementer its fallback chose, once
    // the turn it stopped has ended, with a new session for its first turn.
    // `None` when it was not moving, or its turn answered after all.
    pub(super) fn fell_back(
        &mut self,
        issue: u64,
        result: &Result<AgentReply, AgentError>,
    ) -> Option<Result<Option<StepReport>, StateError>> {
        let flight = self.flights.flying.get(&issue)?;
        let Fallback::Moving(to, session) = &flight.fallback else {
            return None;
        };
        if matches!(result, Ok(_) | Err(AgentError::Stopped)) {
            return None;
        }
        let (to, session) = (to.clone(), session.clone());
        let waited = self.model_wait(issue).map(|w| w.since);
        let now = self.ports.clock.now();
        // Saved while its flight still runs, so the wait is charged as the
        // worker's time, as any turn's is.
        let mut next = self.state.clone();
        let item = next.item_mut(issue)?;
        let from = std::mem::replace(&mut item.agent, to.clone());
        item.session = session;
        item.turn = Turn::Due;
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

#[cfg(test)]
mod tests;
