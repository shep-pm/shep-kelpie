//! A worker's turn that its account refuses for a spent limit
//!
//! A turn on an API agent that ends with [`AgentError::Failed`] whose text
//! [`refusal`] reads tells the agent board, which holds every API agent. A
//! turn on a Codex agent whose text [`codex_limit`] reads holds Codex until
//! the time it names. An unpinned work item then goes to the first other agent
//! the board ranks and the pacer lets start, on a session of its own, with a
//! note and no ruling. A pinned item or one with no such agent parks on an
//! API refusal, and on Codex's waits on the hold. Any other failure parks.

use super::super::Runner;
use super::super::pace::Walked;
use super::super::report::StepReport;
use super::CONTINUE;
use crate::api_refusal::refusal;
use crate::lease::door::PickAsk;
use crate::pacer::Scope;
use crate::plan_refusal::codex_limit;
use crate::ports::{AgentError, AgentReply, Session, Timestamp};
use crate::settings::{Account, AgentName, Harness, Limit};
use crate::state::StateError;
use crate::ticket::TicketId;
use crate::work_item::Turn;

/// How many characters of the API's message a handoff note quotes
const QUOTED: usize = 300;

/// What came of a worker turn that ended in a failure
pub(in crate::runner) enum Handled {
    /// The turn was no refusal this module handles
    No,
    /// The work item went to another agent, with this report
    Moved(StepReport),
    /// The work item stays and waits on a hold, with no ruling
    Waits,
}

impl Runner {
    /// Tells the board the API refused `ticket`'s worker turn that just ended
    /// with `result`, or holds Codex when its plan did, and hands the work
    /// item to the next pick when it can
    ///
    /// Call it while the turn's flight still stands, for the agent it ran.
    ///
    /// # Errors
    ///
    /// [`StateError`] when the handoff cannot be saved.
    pub(in crate::runner) fn hand_off_refused(
        &mut self,
        ticket: TicketId,
        result: &Result<AgentReply, AgentError>,
    ) -> Result<Handled, StateError> {
        let Err(AgentError::Failed(_, text)) = result else {
            return Ok(Handled::No);
        };
        let Some((call, ..)) = self.flights.turn_call(ticket) else {
            return Ok(Handled::No);
        };
        // parity: harness.plan-limit-refusal
        if call.harness.harness() == Harness::Codex {
            return self.hand_off_codex(ticket, text, &call.session);
        }
        let Some(refusal) = refusal(text) else {
            return Ok(Handled::No);
        };
        if !call.harness.bills_api() {
            return Ok(Handled::No);
        }
        if self.ports.agent_board.refused(&refusal).is_none() {
            self.notes.push(format!(
                "cannot tell the agent board the API refused its key: {}",
                ticket.shown()
            ));
        }
        let Some((to, finish)) = self.on(Some(ticket)).next_pick(&Limit::Api)? else {
            return Ok(Handled::No);
        };
        let why = format!("the API refused the key it ran on: {}", quoted(text));
        let handed = self.move_item(to, None, finish, &why, true)?;
        let Some(StepReport::HandedOff {
            issue, from, to, ..
        }) = &handed
        else {
            return Ok(Handled::No);
        };
        self.builds_turn_ended();
        self.dev_server_ended();
        self.notes.push(format!(
            "{} is handed from {from} to {to}: the API refused its key",
            issue.shown()
        ));
        Ok(handed.map_or(Handled::No, Handled::Moved))
    }

    // A Codex turn that failed on its plan's usage limit holds Codex until the
    // time the text names, then goes to the next pick or waits on the hold.
    fn hand_off_codex(
        &mut self,
        ticket: TicketId,
        text: &str,
        session: &Session,
    ) -> Result<Handled, StateError> {
        let Some(until) = codex_limit(text, self.ports.clock.now(), &self.zone) else {
            return Ok(Handled::No);
        };
        let when = shown(until);
        let reason = format!(
            "Codex refused a turn for its plan's usage limit, so no turn starts on it until {when}"
        );
        self.hold_account(Account::Codex, until, reason);
        self.builds_turn_ended();
        self.dev_server_ended();
        let held = Limit::Account(Account::Codex);
        if let Some((to, finish)) = self.on(Some(ticket)).next_pick(&held)? {
            let why = format!(
                "Codex refused a turn for its plan's usage limit, which holds Codex until {when}: {}",
                quoted(text)
            );
            if let Some(handed) = self.move_item(to, None, finish, &why, true)? {
                if let StepReport::HandedOff {
                    issue, from, to, ..
                } = &handed
                {
                    self.notes.push(format!(
                        "{} is handed from {from} to {to}: Codex's usage limit holds Codex until {when}",
                        issue.shown()
                    ));
                }
                return Ok(Handled::Moved(handed));
            }
        }
        self.save_time()?;
        self.turn_waits(ticket, session)?;
        self.notes.push(format!(
            "{}'s turn waits for Codex's usage limit, which holds until {when}",
            ticket.shown()
        ));
        Ok(Handled::Waits)
    }

    // Puts `ticket`'s turn that just failed back to a turn that has not begun,
    // which the pacer holds until the hold ends.
    fn turn_waits(&mut self, ticket: TicketId, session: &Session) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let Some(item) = next.item_mut(ticket) else {
            return Ok(());
        };
        item.turn = match std::mem::replace(&mut item.turn, Turn::Due) {
            Turn::Running {
                prompt: Some(prompt),
                ..
            } => Turn::Next { prompt },
            Turn::Running { prompt: None, .. } if matches!(session, Session::Resume(_)) => {
                Turn::Next {
                    prompt: CONTINUE.to_owned(),
                }
            }
            Turn::Running { prompt: None, .. } => Turn::Due,
            other => other,
        };
        self.save(next)
    }

    // The agent the current work item goes to and the seconds it takes to
    // finish, or None when it stays: pinned, or no other agent the board
    // ranks that the pacer lets start. Every agent on `spent` shares the key
    // or login that was refused, so none is a pick.
    fn next_pick(&mut self, spent: &Limit) -> Result<Option<(AgentName, u64)>, StateError> {
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
            .filter(|pick| self.agent_limit(&pick.agent).as_ref() != Some(spent))
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

// `at` as the date and time in UTC a hold's note names.
fn shown(at: Timestamp) -> String {
    let seconds = i64::try_from(at.0).unwrap_or(i64::MAX);
    jiff::Timestamp::from_second(seconds)
        .unwrap_or(jiff::Timestamp::MAX)
        .to_string()
}

// The first `QUOTED` characters of the API's message, on one line.
fn quoted(text: &str) -> String {
    let line: Vec<&str> = text.split_whitespace().collect();
    line.join(" ").chars().take(QUOTED).collect()
}

#[cfg(test)]
mod tests;
