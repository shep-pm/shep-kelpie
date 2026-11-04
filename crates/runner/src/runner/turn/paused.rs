//! A worker turn that ended at a tool result because a pause was set
//!
//! The turn's hook (Claude Code's `PostToolUse`, pi's guard extension) leaves
//! a marker in the work item's build folder when it ends the turn. Such a turn
//! is not a finished one: the unpushed check, the handover and review wait for
//! the turn it resumes into, nothing is raised for a ruling, and the work item
//! keeps its phase and stage. The next start resumes the session with
//! [`PAUSED`] and a fresh turn ceiling, since a turn queued to send begins at
//! the time it starts.

use crate::guard::Pause;
use crate::ports::{AgentError, AgentReply, Role};
use crate::runner::Runner;
use crate::state::StateError;
use crate::ticket::TicketId;
use crate::work_item::{Turn, WorkItem};

use crate::runner::report::StepReport;

/// The prompt for a turn resumed after a pause ended it at a tool call
pub(in crate::runner) const PAUSED: &str = "Kelpie paused your last turn after its last tool call. \
                                            Carry on with the work item from where you left off.";

impl Runner {
    // Whether the turn of `ticket`'s work item that came back ended at a tool
    // result for a pause
    pub(in crate::runner) fn paused_at_tool(&self, ticket: TicketId) -> bool {
        (self.state.item(ticket)).is_some_and(|item| Pause::in_build(&item.build).ended_turn())
    }

    // Ends the current work item's turn as paused, when it ended that way, and
    // answers `None` otherwise. A turn past its ceiling or stopped is not a
    // pause, whatever the hook left.
    pub(in crate::runner) fn end_paused(
        &mut self,
        result: &Result<AgentReply, AgentError>,
    ) -> Option<Result<Option<StepReport>, StateError>> {
        let ticket = self.current()?.issue;
        if !self.paused_at_tool(ticket) {
            return None;
        }
        let pause = Pause::in_build(&self.current()?.build);
        pause.clear();
        if matches!(result, Err(AgentError::Stopped | AgentError::TimedOut(_))) {
            return None;
        }
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let item = self.current_in(&mut next)?;
        if let Ok(reply) = result {
            let session = item.session.clone();
            item.record_metered_call(
                Role::Worker,
                now,
                session,
                reply.usage,
                reply.session_cost,
                reply.metered.and_then(|m| m.cost),
            );
            item.context = reply.context;
        }
        item.turn = Turn::Next {
            prompt: PAUSED.to_owned(),
        };
        let saved = self.save(next);
        if saved.is_ok() {
            self.notes.push(format!("#{ticket} paused-at-tool"));
        }
        Some(saved.map(|()| None))
    }

    // Says that the turn that just started resumed one a pause ended
    pub(in crate::runner) fn note_resumed(&self, ticket: TicketId) {
        self.notes
            .push(format!("{} resumed-after-pause", ticket.shown()));
    }
}

// Whether the turn about to start resumes one a pause ended
pub(in crate::runner) fn resumes_pause(item: &WorkItem) -> bool {
    matches!(&item.turn, Turn::Next { prompt } if prompt == PAUSED)
}
