//! Why a worker turn that is due has not started
//!
//! A step that returns [`Begin::Idle`] for a turn about to start, or a
//! fresh pacer hold, says why
//! here, so `status` and the board name the wait. A turn still held up
//! after [`LOG_AFTER`] seconds is logged once for each reason. The memory is
//! in memory only: a restart finds the turn due and asks again.

use std::collections::BTreeMap;
use std::mem::{Discriminant, discriminant};

use serde::Serialize;

use crate::ports::Timestamp;
use crate::runner::report::{Begin, StepReport};
use crate::settings::AgentName;
use crate::ticket::TicketId;

use super::ticket_name;

/// Seconds a due turn waits before the log says what it waits on
const LOG_AFTER: u64 = 5 * 60;

/// What holds a due turn back
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) enum Reason {
    /// The work item has no slot under `concurrency.active_items`
    Unseated,
    /// The adopted branch moved, and the gate reads it before a worker builds on it
    PushedByAnother,
    /// The pacer's hold from an earlier reading stands, with the reading's words
    Paced(String),
    /// GitHub is not giving the worker a token just now
    Token,
    /// The runner is draining
    Draining,
    /// A pause gave up and kept the turn it ended stopped for a while
    PauseHeld,
    /// The maintainer's `attach` holds the work item
    Attached,
    /// Its build folder or the disk is still past a `builds` limit after the
    /// turn's cache was pruned, in the reading's words
    Builds(String),
    /// Its agent is out, and the project manager's next pick names the one after
    AgentOut(AgentName),
}

impl Reason {
    fn text(&self) -> String {
        match self {
            Self::Unseated => "waiting for a slot under `concurrency.active_items`".into(),
            Self::PushedByAnother => {
                "its branch was pushed to by someone else, so the gate reads it first".into()
            }
            Self::Paced(why) => format!("held by the pacer: {why}"),
            Self::Token => "waiting for a GitHub token, which GitHub is not giving just now".into(),
            Self::Draining => "the runner is draining, so no turn starts".into(),
            Self::PauseHeld => {
                "a pause gave up, so the turn it ended stays stopped for a while".into()
            }
            Self::Attached => "attached by the maintainer, so no turn starts".into(),
            Self::Builds(why) => format!("held by the `builds` limits: {why}"),
            Self::AgentOut(agent) => format!(
                "its agent {agent} is out, so the turn waits for the project manager to name the next"
            ),
        }
    }
}

/// A due turn that has not started, as `status` shows it
// wire format: changing this is a breaking change to `status`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeldUp {
    /// When the turn was first held up, unbroken since
    pub since: Timestamp,
    /// What it waits on
    pub because: String,
}

#[derive(Debug)]
struct Held {
    since: Timestamp,
    reason: Reason,
    // The reasons the log has told since the turn was first held up
    told: Vec<Discriminant<Reason>>,
}

/// The work items whose due turn did not start on their last step
#[derive(Debug, Default)]
pub(in crate::runner) struct HeldUps {
    // The reason the step in hand gives, taken when it ends
    pending: Option<Reason>,
    items: BTreeMap<TicketId, Held>,
}

impl HeldUps {
    /// The step in hand returns idle because of `reason`
    pub(in crate::runner) fn because(&mut self, reason: Reason) {
        self.pending = Some(reason);
    }

    /// The step in hand will not end, as its work item changed during a read
    pub(in crate::runner) fn forget_pending(&mut self) {
        self.pending = None;
    }

    /// Records that `ticket`'s step began `begin` at `now`, and returns the
    /// log line when a turn has waited [`LOG_AFTER`] on a reason not yet told
    pub(in crate::runner) fn stepped(
        &mut self,
        ticket: TicketId,
        begin: &Begin,
        now: Timestamp,
    ) -> Option<String> {
        // A fresh pacer hold is a report of its own, and the wait goes on.
        let held = matches!(begin, Begin::Idle | Begin::Report(StepReport::Held { .. }));
        let reason = self.pending.take().filter(|_| held);
        let Some(reason) = reason else {
            self.items.remove(&ticket);
            return None;
        };
        let held = self.items.entry(ticket).or_insert_with(|| Held {
            since: now,
            reason: reason.clone(),
            told: Vec::new(),
        });
        held.reason = reason;
        let waited = now.0.saturating_sub(held.since.0);
        let kind = discriminant(&held.reason);
        if waited < LOG_AFTER || held.told.contains(&kind) {
            return None;
        }
        held.told.push(kind);
        Some(format!(
            "{}'s turn is due and has not started for {} minutes: {}",
            ticket_name(ticket),
            waited / 60,
            held.reason.text()
        ))
    }

    /// What `ticket`'s due turn waits on, if its last step left it idle
    pub(in crate::runner) fn of(&self, ticket: TicketId) -> Option<HeldUp> {
        self.items.get(&ticket).map(|held| HeldUp {
            since: held.since,
            because: held.reason.text(),
        })
    }

    /// Forgets the work items no longer open
    pub(in crate::runner) fn keep_only(&mut self, open: &[TicketId]) {
        self.items.retain(|ticket, _| open.contains(ticket));
    }
}
