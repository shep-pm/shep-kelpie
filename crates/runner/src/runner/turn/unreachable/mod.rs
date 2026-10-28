//! A worker's turn whose model host cannot be reached
//!
//! A host that is down, restarting or refusing connections passes, so a turn
//! that ends with [`AgentError::Unreachable`] does not park its work item on
//! a ruling. It keeps its session, and the runner tries the same call again
//! after a pause that grows from [`RETRY_FIRST`] to [`RETRY_MAX`]. The item
//! shows as waiting for its model host from the first failure. Past [`LIMIT`]
//! since that failure the turn parks as any failed one does, with the last
//! error. The wait is in memory only, so a restart begins a new one: the
//! turn is still marked running, and the restart resumes its session.

use std::collections::BTreeMap;
use std::time::Duration;

use super::super::Runner;
use super::super::alert::backoff;
use super::super::code_graph::who;
use super::super::flight::Waiting;
use super::super::report::Begin;
use crate::ports::{AgentCall, AgentError, Timestamp, Wait};
use crate::profile;
use crate::ticket::TicketId;

use super::ticket_name;

/// Seconds the wait lasts from the first failure before the turn parks
const LIMIT: u64 = 30 * 60;

/// Seconds before the first retry
const RETRY_FIRST: u64 = 30;

/// The longest pause between two retries, in seconds
const RETRY_MAX: u64 = 5 * 60;

// One work item's turn waiting for its host
#[derive(Debug)]
struct Pause {
    // The call that failed, tried again as it was, session and prompt
    call: AgentCall,
    // Whether the turn is its work item's first
    first: bool,
    // The seconds of the turn's ceiling it had when the call began, which the
    // retry has again, since the wait is not the worker's time
    left: u64,
    // When the host first could not be reached
    since: Timestamp,
    // The failures in a row
    failures: u32,
    // Until when no retry is made
    until: Timestamp,
}

/// The turns waiting for their model host, by ticket, in memory only
#[derive(Debug, Default)]
pub(in crate::runner) struct Unreachable(BTreeMap<TicketId, Pause>);

impl Unreachable {
    /// What `ticket`'s turn waits for, if it waits for its host
    pub(in crate::runner) fn waiting(&self, ticket: TicketId) -> Option<Waiting> {
        self.0.get(&ticket).map(|pause| Waiting {
            since: pause.since,
            why: Wait::Unreachable,
        })
    }

    /// Forgets the waits of work items no longer open
    pub(in crate::runner) fn keep_only(&mut self, open: &[TicketId]) {
        self.0.retain(|ticket, _| open.contains(ticket));
    }
}

impl Runner {
    /// Whether `ticket`'s turn that just ended with `result` waits for its
    /// host and is tried again, rather than parks on a ruling
    ///
    /// Any result but an unreachable host ends the wait. Call it while the
    /// turn's flight still stands, for the call it keeps.
    pub(in crate::runner) fn waits_for_host(
        &mut self,
        ticket: TicketId,
        result: &Result<crate::ports::AgentReply, AgentError>,
    ) -> bool {
        let Err(AgentError::Unreachable(_, last)) = result else {
            if self.unreachable.0.remove(&ticket).is_some() {
                self.board_changed();
            }
            return false;
        };
        let now = self.ports.clock.now();
        let held = self.unreachable.0.remove(&ticket);
        let Some((call, first, left)) = self.flights.turn_call(ticket) else {
            // Not a worker's turn, so no call to keep and no wait to hold.
            if held.is_some() {
                self.board_changed();
            }
            return false;
        };
        // A retry that ran longer than the longest pause reached its host, so
        // the outage it ended is over and this failure begins another.
        let ran = self
            .flights
            .started(ticket)
            .map_or(0, |at| now.0.saturating_sub(at.0));
        let held = held.filter(|_| ran <= RETRY_MAX);
        let since = held.as_ref().map_or(now, |pause| pause.since);
        let failures = held.as_ref().map_or(0, |pause| pause.failures) + 1;
        self.board_changed();
        if now.0.saturating_sub(since.0) >= LIMIT {
            self.notes.push(format!(
                "{}'s model host stayed unreachable for {} minutes: {}",
                ticket_name(ticket),
                LIMIT / 60,
                last.trim()
            ));
            return false;
        }
        let pause = backoff(RETRY_FIRST, failures, RETRY_MAX);
        if held.is_none() {
            self.notes.push(format!(
                "{}'s model host cannot be reached, so its turn is tried \
                 again in {pause} seconds: {}",
                ticket_name(ticket),
                last.trim()
            ));
        }
        self.unreachable.0.insert(
            ticket,
            Pause {
                call,
                first,
                left,
                since,
                failures,
                until: Timestamp(now.0.saturating_add(pause)),
            },
        );
        true
    }

    /// When the soonest turn waiting for its host is tried again
    ///
    /// A turn whose retry is in flight is not waiting for a pause any more,
    /// so its due time does not count.
    pub(in crate::runner) fn next_retry(&self) -> Option<Duration> {
        let now = self.ports.clock.now();
        let soonest = (self.unreachable.0.iter())
            .filter(|(ticket, _)| !self.flights.flying(**ticket))
            .map(|(_, pause)| pause.until)
            .min()?;
        Some(Duration::from_secs(soonest.0.saturating_sub(now.0)))
    }

    /// The call of `ticket`'s turn waiting for its host, once its pause has
    /// passed, or `Begin::Idle` while it holds; `None` for a turn that does
    /// not wait
    pub(super) fn retry_for_host(&self, ticket: TicketId) -> Option<Begin> {
        let pause = self.unreachable.0.get(&ticket)?;
        let now = self.ports.clock.now();
        if now < pause.until || self.draining() || self.token_wait.holds(now) {
            return Some(Begin::Idle);
        }
        // The call is replayed as it was, but its GitHub login was written
        // when the turn began and may have run out during the wait, so a fresh
        // one is written now. A token that cannot be had waits as one does at a
        // turn's start; any other failure goes on with the login already there.
        self.token_wait.begins();
        match self.ports.voice.worker_token(&self.remote) {
            Ok(Some(token)) => {
                if let Some(item) = self.current().filter(|item| item.issue == ticket) {
                    let _ = profile::write_login(&item.build, &token);
                }
                self.token_wait.ends();
            }
            Err(e) if self.token_error_passes(&e) => return Some(Begin::Idle),
            Ok(None) | Err(_) => {}
        }
        let mut call = pause.call.clone();
        if let Err(why) = self.reopen_browser(&mut call) {
            self.notes
                .push(format!("{} retries without its browser: {why}", who(&call)));
        }
        Some(Begin::Call {
            call,
            deadline: Timestamp(now.0.saturating_add(pause.left)),
            first: pause.first,
            compact: false,
        })
    }
}

#[cfg(test)]
mod tests;
