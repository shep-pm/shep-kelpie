//! Checking the project manager's answer against the board, and acting on
//! what holds
//!
//! A pick or a hold must name a ready issue the board shows, and an unstick
//! a work item that is stuck now: parked on a ruling a stuck item raises,
//! or with a call the board reads as idle. Anything else is dropped, and
//! named in the log.

use super::answer::{Action, Answer, Unstick};
use super::{Pick, stuck_on};
use crate::runner::{Answer as Ruled, RuleError, Runner};
use crate::state::{RulingKind, StateError};
use crate::work_item::Phase;

/// The most of the project manager's words a ruling or a note carries, in
/// characters
const WORDS: usize = 300;

/// How a work item is stuck
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stuck {
    /// Parked on this ruling, which a stuck item raises
    Ruling(u64),
    /// Its call in flight shows nothing, as the board reads it
    Idle,
}

impl Runner {
    /// Acts on `answer` as far as the board allows, and returns what it
    /// did and what it dropped, each in words. `picking` says the wake was
    /// for a free slot.
    ///
    /// # Errors
    ///
    /// [`StateError`] when what it acted on cannot be saved.
    pub(super) fn act(
        &mut self,
        answer: &Answer,
        picking: bool,
    ) -> Result<(Vec<String>, Vec<String>), StateError> {
        let ready = self.board_ready();
        // Starting nothing lasts until its next wake, which says again.
        if matches!(self.pm.pick, Some(Pick::Nothing(_))) {
            self.pm.pick = None;
        }
        let mut held: Vec<u64> = Vec::new();
        let mut dropped = answer.unread.clone();
        for &n in &answer.hold {
            match ready.contains(&n) {
                true if !held.contains(&n) => held.push(n),
                true => {}
                false => dropped.push(format!("hold #{n}: not a ready issue on the board")),
            }
        }
        let mut acted = Vec::new();
        let now = self.ports.clock.now();
        match answer.pick {
            Some(n) if held.contains(&n) => {
                dropped.push(format!("pick #{n}: it holds #{n} too"));
                if picking {
                    self.pm.pick = Some(Pick::Rule);
                }
            }
            Some(n) if ready.contains(&n) => {
                self.pm.pick = Some(Pick::Issue(n));
                acted.push(format!("picked #{n}"));
            }
            Some(n) => {
                dropped.push(format!("pick #{n}: not a ready issue on the board"));
                if picking {
                    self.pm.pick = Some(Pick::Rule);
                }
            }
            None if picking => {
                self.pm.pick = Some(Pick::Nothing(now));
                acted.push("starts nothing now".to_owned());
            }
            None => {}
        }
        if picking {
            self.pm.asked_pick = false;
        }
        if !held.is_empty() {
            let named: Vec<String> = held.iter().map(|n| format!("#{n}")).collect();
            acted.push(format!("holds {}", named.join(", ")));
        }
        self.pm.held = held;
        if let Some(unstick) = &answer.unstick {
            match self.unstick(unstick)? {
                Ok(done) => acted.push(done),
                Err(why) => dropped.push(why),
            }
        }
        if let Some(reply) = &answer.reply {
            self.pm.reply = Some(reply.clone());
        }
        Ok((acted, dropped))
    }

    // What the answer's unstick did, or why it was dropped.
    fn unstick(&mut self, unstick: &Unstick) -> Result<Result<String, String>, StateError> {
        let Unstick { item, action, why } = unstick;
        let n = *item;
        let Some(stuck) = self.stuck(n) else {
            return Ok(Err(format!(
                "{} #{n}: not a stuck work item on the board",
                action.as_str()
            )));
        };
        let done = match (stuck, action) {
            (_, Action::Leave) => format!("left #{n} to resolve itself"),
            (Stuck::Idle, action) => {
                // Its end parks it on the ceiling's ruling, which this then settles.
                self.flights.end(n);
                self.pm.after_end.insert(n, (*action, why.clone()));
                format!("ended #{n}'s idle call, to {}", action.as_str())
            }
            (Stuck::Ruling(id), action) => {
                if let Err(e) = self.settle(n, id, *action, why)? {
                    return Ok(Err(e));
                }
                match action {
                    Action::Retry => format!("retried #{n}"),
                    _ => format!("put #{n} to the maintainer on ruling {id}"),
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
    pub(in crate::runner) fn pm_after_end(&mut self, issue: u64) -> Result<(), StateError> {
        let Some((action, why)) = self.pm.after_end.remove(&issue) else {
            return Ok(());
        };
        let Some(Stuck::Ruling(id)) = self.stuck(issue) else {
            return Ok(());
        };
        if let Err(e) = self.settle(issue, id, action, &why)? {
            self.notes.push(format!(
                "the project manager could not act on #{issue}: {e}"
            ));
        }
        Ok(())
    }

    // Retries the item parked on ruling `id` as a yes would, or for CI
    // still red sends the worker back; or puts it to the maintainer with
    // the project manager's words added to the ruling, posted again.
    fn settle(
        &mut self,
        issue: u64,
        id: u64,
        action: Action,
        why: &str,
    ) -> Result<Result<(), String>, StateError> {
        let Some(ruling) = self.state.rulings.iter().find(|r| r.id == id) else {
            return Ok(Err(format!("#{issue}: ruling {id} is answered")));
        };
        let why = plain(&self.shown(why.to_owned()), WORDS);
        if action == Action::Retry {
            let answer = match ruling.kind {
                RulingKind::StillRed { .. } => Ruled::No(format!(
                    "(from the project manager) CI is still red: fix it and push. PM says: {why}"
                )),
                _ => Ruled::Yes,
            };
            return match self.rule(id, answer) {
                Ok(()) => Ok(Ok(())),
                Err(RuleError::State(e)) => Err(e),
                Err(e) => Ok(Err(format!("retry #{issue}: {e}"))),
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

    // How `issue`'s work item is stuck now, if it is.
    fn stuck(&self, issue: u64) -> Option<Stuck> {
        let item = self.state.item(issue)?;
        if let Phase::Ruling { id } = item.phase {
            let ruling = self.state.rulings.iter().find(|r| r.id == id)?;
            return stuck_on(&ruling.kind).map(|_| Stuck::Ruling(id));
        }
        let idle = self.flights.watched(issue).is_some_and(|w| w.idle);
        idle.then_some(Stuck::Idle)
    }
}

// `text` on one line with no control characters, cut past `most` characters,
// since it reaches the maintainer's webhook and the state file as it is.
fn plain(text: &str, most: usize) -> String {
    let spaced: String = (text.chars())
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(most) {
        Some((end, _)) => format!("{} …", &line[..end]),
        None => line,
    }
}
