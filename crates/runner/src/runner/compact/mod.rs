//! A worker's `/compact`, sent before a turn once its session has grown
//!
//! A worker keeps one session for its whole work item, and every turn the
//! runner sends after the build resumes it and rereads all of it. Past
//! [`WORKER_COMPACT_PAST`](super::pm::WORKER_COMPACT_PAST) tokens of context
//! the runner sends `/compact` first, the way it does the project manager's.
//! The turn stays due while the compaction runs, so a compaction cut short
//! by a stop is sent again, and the turn's prompt, once it ends, opens with
//! the issue and the pull request, which the summary may have lost.

use super::Runner;
use super::report::StepReport;
use super::turn::ticket_name;
use crate::ports::{AgentError, AgentReply, Role};
use crate::skills::split_command;
use crate::state::StateError;
use crate::ticket::TicketId;
use crate::work_item::{Turn, WorkItem};

impl Runner {
    /// Records the end of `ticket`'s `/compact`
    ///
    /// A compaction that failed is let go: the turn goes on in the session
    /// as it was, and is not compacted again until its context is read anew.
    ///
    /// # Errors
    ///
    /// [`StateError`] when the work item cannot be saved.
    pub(super) fn compact_ended(
        &mut self,
        result: Result<AgentReply, AgentError>,
    ) -> Result<Option<StepReport>, StateError> {
        let ticket = self.focus.expect("a compaction is a work item's");
        self.flights.landed(ticket);
        // A compaction stopped with the runner is sent again on restart.
        if matches!(result, Err(AgentError::Stopped)) {
            return self.save_time().map(|()| None);
        }
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let Some(item) = self.current_in(&mut next) else {
            return Ok(None);
        };
        let issue = item.issue;
        let context = item.context.take();
        let report = match result {
            Ok(reply) => {
                let session = item.session.clone();
                item.record_metered_call(
                    Role::Worker,
                    now,
                    session.clone(),
                    reply.usage,
                    reply.session_cost,
                    reply.metered.and_then(|m| m.cost),
                );
                let reminder = reminder(item, ticket);
                // A skill's command only runs from the very start of a prompt.
                if let Turn::Next { prompt } = &mut item.turn {
                    let (command, then) = split_command(prompt);
                    *prompt = match command {
                        Some(command) => format!("{command} {reminder}\n\n{then}"),
                        None => format!("{reminder}\n\n{then}"),
                    };
                }
                StepReport::WorkerCompacted {
                    issue,
                    session,
                    context,
                }
            }
            Err(e) => StepReport::WorkerCompactFailed {
                issue,
                reason: e.to_string(),
            },
        };
        self.save(next)?;
        Ok(Some(report))
    }
}

// What the worker is told ahead of its turn once its history is a summary:
// the ticket and the pull request, which nothing but that history held. Only
// a GitHub ticket has an issue for `gh` to read.
fn reminder(item: &WorkItem, ticket: TicketId) -> String {
    let pull_request = item.pull_request.map_or_else(String::new, |number| {
        format!(
            " Your pull request #{number} is on branch `{}`.",
            item.branch
        )
    });
    let read = ticket.github().map_or_else(String::new, |issue| {
        format!(" `gh issue view {issue}` shows the issue in full.")
    });
    format!(
        "Kelpie compacted your session, so what came before is a summary. Your work item is \
         {}: {}.{pull_request}{read}",
        ticket_name(ticket),
        item.title
    )
}

#[cfg(test)]
mod tests;
