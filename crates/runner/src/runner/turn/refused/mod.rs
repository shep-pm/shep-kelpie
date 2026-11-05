//! A worker's turn the API refuses for spent credit or a spent limit
//!
//! A turn on an API agent that ends with [`AgentError::Failed`] whose text
//! [`refusal`] reads tells the agent board, which holds every API agent. An
//! unpinned work item then goes to the first other agent the board ranks and
//! the pacer lets start, on a session of its own, with a note of why and no
//! ruling. A pinned item, one with no such agent, and any other failure park
//! as a failed turn does, with the API's message. A refusal on a plan agent
//! is no API refusal and parks the same.

use super::super::Runner;
use super::super::pace::Walked;
use super::super::report::StepReport;
use crate::api_refusal::refusal;
use crate::lease::door::PickAsk;
use crate::pacer::Scope;
use crate::ports::{AgentError, AgentReply};
use crate::settings::{AgentName, Limit};
use crate::state::StateError;
use crate::ticket::TicketId;

/// How many characters of the API's message a handoff note quotes
const QUOTED: usize = 300;

impl Runner {
    /// Tells the board the API refused `ticket`'s worker turn that just ended
    /// with `result`, and hands the work item to the next pick when it can
    ///
    /// Returns the handoff's report, or None when the turn was no refusal, or
    /// its item stays and parks as a failed turn. Call it while the turn's
    /// flight still stands, for the agent it ran.
    ///
    /// # Errors
    ///
    /// [`StateError`] when the handoff cannot be saved.
    pub(in crate::runner) fn hand_off_refused(
        &mut self,
        ticket: TicketId,
        result: &Result<AgentReply, AgentError>,
    ) -> Result<Option<StepReport>, StateError> {
        let Err(AgentError::Failed(_, text)) = result else {
            return Ok(None);
        };
        let Some(refusal) = refusal(text) else {
            return Ok(None);
        };
        let Some((call, ..)) = self.flights.turn_call(ticket) else {
            return Ok(None);
        };
        if !call.harness.bills_api() {
            return Ok(None);
        }
        if self.ports.agent_board.refused(&refusal).is_none() {
            self.notes.push(format!(
                "cannot tell the agent board the API refused its key: {}",
                ticket.shown()
            ));
        }
        let Some((to, finish)) = self.on(Some(ticket)).next_pick()? else {
            return Ok(None);
        };
        let why = format!("the API refused the key it ran on: {}", quoted(text));
        let handed = self.move_item(to, None, finish, &why, true)?;
        if let Some(StepReport::HandedOff {
            issue, from, to, ..
        }) = &handed
        {
            self.builds_turn_ended();
            self.dev_server_ended();
            self.notes.push(format!(
                "{} is handed from {from} to {to}: the API refused its key",
                issue.shown()
            ));
        }
        Ok(handed)
    }

    // The agent the current work item goes to and the seconds it takes to
    // finish, or None when it stays: pinned, or no other agent the board
    // ranks that the pacer lets start. Every API agent shares the key the API
    // refused, so none is a pick.
    fn next_pick(&mut self) -> Result<Option<(AgentName, u64)>, StateError> {
        let Some(item) = self.current().filter(|item| !item.pinned) else {
            return Ok(None);
        };
        let ask = PickAsk {
            kind: item.kind.clone(),
            tier: item.tier,
            pin: None,
            slack: None,
            agents: self.board_agents(),
            leave_out: item.left.clone(),
        };
        let Ok(picks) = self.board_picks(&ask) else {
            return Ok(None);
        };
        let others: Vec<_> = (picks.iter())
            .filter(|pick| self.agent_limit(&pick.agent) != Some(Limit::Api))
            .collect();
        // Each agent is walked alone, so a read the pacer asks for names the
        // agent it is for, and the agents it held ahead of it stay passed over.
        for pick in others {
            match self.first_unpaced([&pick.agent], Scope::Turn)? {
                Walked::Opens(to) => return Ok(Some((to, pick.finish))),
                // The turn that begins on it reads its account before it starts.
                Walked::Reads(_) => return Ok(Some((pick.agent.clone(), pick.finish))),
                Walked::Held { .. } => {}
            }
        }
        Ok(None)
    }
}

// The first `QUOTED` characters of the API's message, on one line.
fn quoted(text: &str) -> String {
    let line: Vec<&str> = text.split_whitespace().collect();
    line.join(" ").chars().take(QUOTED).collect()
}

#[cfg(test)]
mod tests;
