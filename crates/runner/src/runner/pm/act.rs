//! Checking the project manager's answer against the board, and acting on
//! what holds
//!
//! A pick or a hold must name a ready issue the board shows, a pick's agent
//! an implementer, and an unstick a work item that is stuck now: parked on a
//! ruling a stuck item raises, or with a call the board reads as idle.
//! Anything else is dropped, and named in the log. Each unstick in the answer
//! runs on its own, so one wake that carried several stuck items can settle
//! each, and one naming the same item twice acts once.

use super::answer::{Action, Answer, Unstick};
use super::maintainer::Held;
use super::red::Settled;
use super::{Pick, Picked, StuckState, stuck_on};
use crate::board::briefing::ticket_ref;
use crate::runner::thread_replies::Settler;
use crate::runner::{Answer as Ruled, RuleError, Runner};
use crate::state::{RulingKind, StateError};
use crate::ticket::TicketId;
use crate::work_item::Phase;

/// The most of the project manager's words a ruling or a note carries, in
/// characters
pub(super) const WORDS: usize = 300;

impl Runner {
    /// Acts on `answer` as far as the board allows, and returns what it
    /// did and what it dropped, each in words. `picking` is how many issues
    /// and then how many slots the wake offered, when it was for a pick, and
    /// so how many picks it takes.
    ///
    /// # Errors
    ///
    /// [`StateError`] when what it acted on cannot be saved.
    pub(super) fn act(
        &mut self,
        answer: &mut Answer,
        picking: Option<(usize, usize)>,
    ) -> Result<(Vec<String>, Vec<String>), StateError> {
        let ready = self.board_ready();
        // An answer with no `hold` leaves the holds standing; one with a list replaces them.
        let mut held: Vec<Held> = match &answer.hold {
            Some(_) => Vec::new(),
            None => self.pm.held.clone(),
        };
        let mut dropped = answer.unread.clone();
        let open = self.state.open_tickets();
        for call in answer.hold.iter().flatten() {
            let named = ticket_ref(call.issue);
            let waits = call.until.filter(|until| !open.contains(until));
            if !ready.contains(&call.issue) {
                dropped.push(format!("hold {named}: not a ready issue on the board"));
            } else if let Some(until) = waits {
                let until = ticket_ref(until);
                dropped.push(format!("hold {named}: {until} is no open work item"));
            } else if !held.iter().any(|h| h.issue == call.issue) {
                let text = self.ready_text(call.issue).unwrap_or_default();
                held.push(Held {
                    issue: call.issue,
                    until: call.until,
                    text,
                });
            }
        }
        let mut acted = Vec::new();
        // A workflow red on `main` is settled first: a fix work item it opens
        // is first on the board, so the rule starts it when no pick is named.
        for red in &answer.red {
            match self.settle_red(red)? {
                Ok((done, settled)) => {
                    acted.push(done);
                    if let Settled::Asked(message) = settled {
                        let reply = answer.reply.take();
                        answer.reply = Some(match reply {
                            Some(reply) => format!("{reply}\n\n{message}"),
                            None => message,
                        });
                    }
                }
                Err(why) => dropped.push(why),
            }
        }
        // A wake for free slots takes a pick for each; any other takes one.
        let (offered, slots) = picking.unwrap_or((1, 1));
        let implementers = self.implementer_files();
        let mut picks: Vec<Picked> = Vec::new();
        for picked in &answer.picks {
            let ticket = picked.issue;
            let named = ticket_ref(ticket);
            let unlisted = (picked.agent.as_ref()).filter(|a| !implementers.contains(a));
            if picks.len() >= slots {
                dropped.push(format!(
                    "pick {named}: only {slots} can start from this wake"
                ));
            } else if held.iter().any(|h| h.issue == ticket) {
                dropped.push(format!("pick {named}: it holds {named} too"));
            } else if !ready.contains(&ticket) {
                dropped.push(format!("pick {named}: not a ready issue on the board"));
            } else if let Some(agent) = unlisted {
                dropped.push(format!(
                    "pick {named}: `{agent}` is no implementer agent file `agents.allow` holds"
                ));
            } else {
                picks.push(picked.clone());
            }
        }
        if !picks.is_empty() {
            let named: Vec<String> = picks.iter().map(|p| ticket_ref(p.issue)).collect();
            acted.push(format!("picked {}", named.join(", ")));
            self.pm.pick = Some(Pick::Issues(picks));
        } else if picking.is_some() {
            // A pick of null, or none the board could take, leaves the issues
            // the wake offered to the rule.
            self.pm.pick = Some(Pick::Rule(slots.min(offered)));
        }
        if picking.is_some() {
            self.pm.asked_pick = false;
        }
        if answer.hold.is_some() && !held.is_empty() {
            let named: Vec<String> = held.iter().map(Held::to_string).collect();
            acted.push(format!("holds {}", named.join(", ")));
        }
        self.pm.held = held;
        let mut unstuck: Vec<TicketId> = Vec::new();
        for unstick in &answer.unstick {
            if unstuck.contains(&unstick.item) {
                dropped.push(format!(
                    "{} {}: the answer names it twice",
                    unstick.action.as_str(),
                    ticket_ref(unstick.item)
                ));
                continue;
            }
            unstuck.push(unstick.item);
            match self.unstick(unstick)? {
                Ok(done) => acted.push(done),
                Err(why) => dropped.push(why),
            }
        }
        if let Some(settle) = &answer.answer {
            match self.settle_question(settle)? {
                Ok(done) => acted.push(done),
                Err(why) => dropped.push(why),
            }
        }
        if let Some(decide) = &answer.follow_up {
            match self.settle_follow_up(decide)? {
                Ok(done) => acted.push(done),
                Err(why) => dropped.push(why),
            }
        }
        if let Some(reply) = &answer.reply {
            self.pm.reply = Some(reply.clone());
        }
        Ok((acted, dropped))
    }

    // What one unstick in the answer did, or why it was dropped.
    fn unstick(&mut self, unstick: &Unstick) -> Result<Result<String, String>, StateError> {
        let Unstick { item, action, why } = unstick;
        let ticket = *item;
        let named = ticket_ref(ticket);
        let Some(stuck) = self.stuck(ticket) else {
            return Ok(Err(format!(
                "{} {named}: not a stuck work item on the board",
                action.as_str()
            )));
        };
        let now = self.ports.clock.now();
        let done = match (stuck, action) {
            (_, Action::Leave) => {
                self.pm_left_alone(ticket, stuck, now);
                format!("left {named} to resolve itself")
            }
            (StuckState::Idle(..), action) => {
                self.pm.left_alone.remove(&ticket);
                // Its end parks it on the ceiling's ruling, which this then settles.
                self.flights.end(ticket);
                self.pm.after_end.insert(ticket, (*action, why.clone()));
                format!("ended {named}'s idle call, to {}", action.as_str())
            }
            (StuckState::Ruling { id, .. }, action) => {
                self.pm.left_alone.remove(&ticket);
                if let Err(e) = self.settle(ticket, id, *action, why)? {
                    return Ok(Err(e));
                }
                match action {
                    Action::Retry => format!("retried {named}"),
                    _ => format!("put {named} to the maintainer on ruling {id}"),
                }
            }
        };
        Ok(Ok(done))
    }

    /// Settles a work item whose idle call the project manager had ended,
    /// once its end has parked it
    ///
    /// # Errors
    ///
    /// [`StateError`] when what it did cannot be saved.
    pub(in crate::runner) fn pm_after_end(&mut self, ticket: TicketId) -> Result<(), StateError> {
        let Some((action, why)) = self.pm.after_end.remove(&ticket) else {
            return Ok(());
        };
        let Some(StuckState::Ruling { id, .. }) = self.stuck(ticket) else {
            return Ok(());
        };
        if let Err(e) = self.settle(ticket, id, action, &why)? {
            self.notes.push(format!(
                "the project manager could not act on {}: {e}",
                ticket_ref(ticket)
            ));
        }
        Ok(())
    }

    // Retries the item parked on ruling `id` as a yes would, or for CI
    // still red sends the worker back; or puts it to the maintainer with
    // the project manager's words added to the ruling, posted again.
    fn settle(
        &mut self,
        ticket: TicketId,
        id: TicketId,
        action: Action,
        why: &str,
    ) -> Result<Result<(), String>, StateError> {
        let named = ticket_ref(ticket);
        let Some(ruling) = self.state.rulings.iter().find(|r| r.id == id) else {
            return Ok(Err(format!("{named}: ruling {id} is answered")));
        };
        let why = plain(&self.shown(why.to_owned()), WORDS);
        if action == Action::Retry {
            let (answer, said) = match ruling.kind {
                RulingKind::Stuck(crate::state::Stuck::StillRed { .. }) => (
                    Ruled::No(format!(
                        "(from the project manager) CI is still red: fix it and push. PM says: {why}"
                    )),
                    "no, CI is still red, so the worker fixes it and pushes",
                ),
                _ => (Ruled::Yes, "yes"),
            };
            let by = Settler::Pm {
                answer: said.to_owned(),
                why: why.clone(),
                unread: false,
            };
            return match self.rule_by(id, answer, by) {
                Ok(()) => Ok(Ok(())),
                Err(RuleError::State(e)) => Err(e),
                Err(e) => Ok(Err(format!("retry {named}: {e}"))),
            };
        }
        let lead = match action {
            Action::ReScope => "The project manager proposes re-scoping it",
            _ => "The project manager asks you to decide",
        };
        let mut next = self.state.clone();
        let ruling = (next.rulings.iter_mut().find(|r| r.id == id)).expect("found above");
        ruling.question = match why.is_empty() {
            true => format!("{}\n\n{lead}.", ruling.question),
            false => format!("{}\n\n{lead}. PM says: {why}", ruling.question),
        };
        ruling.alerted = false;
        self.save(next)?;
        Ok(Ok(()))
    }

    // How `ticket`'s work item is stuck now, if it is.
    fn stuck(&self, ticket: TicketId) -> Option<StuckState> {
        let item = self.state.item(ticket)?;
        if let Phase::Ruling { id } = item.phase {
            let ruling = self.state.rulings.iter().find(|r| r.id == id)?;
            return stuck_on(&ruling.kind).map(|_| StuckState::Ruling {
                id,
                raised: ruling.raised,
            });
        }
        let watched = self.flights.watched(ticket)?;
        watched.idle.then_some(StuckState::Idle(watched.started))
    }
}

// `text` on one line with no control characters, cut past `most` characters,
// since it reaches the maintainer's webhook and the state file as it is.
pub(super) fn plain(text: &str, most: usize) -> String {
    let spaced: String = (text.chars())
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(most) {
        Some((end, _)) => format!("{} …", &line[..end]),
        None => line,
    }
}
