//! What wakes the project manager besides a pick and a `tell`: a work item
//! parked on a stuck item's ruling, a pair of open branches newly in
//! conflict in their own changes rather than `main`'s, and, once it is back,
//! whatever it missed while it was not
//!
//! A work item the project manager answered `none` for is not queued again
//! while it is in the state it was left alone in, which is what `pm_due` checks
//! for every source of a stuck wake.

use std::collections::BTreeMap;

use crate::board::briefing::{age, elapsed, ticket_ref};

use super::{StuckState, Wake};
use crate::ports::Timestamp;
use crate::runner::Runner;
use crate::runner::voice::brief;
use crate::state::{ProjectState, Ruling, RulingKind, Stuck};
use crate::ticket::TicketId;
use crate::work_item::Phase;

/// How long an idle call left alone is dropped for after its first `none`, in
/// seconds
///
/// Measured on the maintainer's day of 134 wakes: 89 held nothing but a stuck
/// item, 76 of those answered `none`, and one item woke it 15 times about every
/// 10 minutes, which is the board's own idle length.
const QUIET_FIRST: u64 = 30 * 60;

/// How long an idle call left alone is dropped for after a second `none` in a
/// row on the same turn, in seconds
const QUIET_SECOND: u64 = 60 * 60;

/// The most an idle call left alone is dropped for, in seconds
const QUIET_MOST: u64 = 2 * 60 * 60;

/// What a wake says for a call in flight the board reads idle, when nothing
/// measured its quiet length
const IDLE_WHAT: &str = "its worker has shown no tool call or output for 10 minutes or more";

/// What the project manager left a work item alone in, when it said so, how
/// many `none`s in a row on that state, when its hold ends, and whether the
/// wake its hold ended for has gone out
#[derive(Debug, Clone)]
pub(super) struct LeftAlone {
    state: StuckState,
    at: Timestamp,
    /// When its wake may go out again. `None` for an item parked on a ruling,
    /// which waits on the maintainer's answer and not on a clock
    until: Option<Timestamp>,
    nones: u32,
    /// Its hold ended and the wake was queued, so the same hold does not wake
    /// the project manager on every pass. A new `none` for the item clears it
    asked: bool,
}

// How long `nones` in a row on one idle call hold its wake back.
fn quiet_backoff(nones: u32) -> u64 {
    match nones {
        0 | 1 => QUIET_FIRST,
        2 => QUIET_SECOND,
        _ => QUIET_MOST,
    }
}

impl Runner {
    // Queues a wake for each item the board still reads as stuck or waiting
    // and each conflict still open, once it is back after giving some up.
    pub(super) fn still_due(&mut self) {
        let mut wakes = Vec::new();
        for item in &self.state.work_items {
            let ticket = item.issue;
            match self.stuck_now(ticket) {
                Some(wake) => wakes.push(wake),
                None => {
                    if let Some(wake) = self.lease_wake(ticket) {
                        wakes.push(wake);
                    }
                }
            }
        }
        for (&(a, b), files) in &self.pm.conflicts {
            wakes.push(Wake::Conflict(a, b, files.clone()));
        }
        wakes.extend(self.unsettled_reds());
        for wake in wakes {
            self.pm_due(wake);
        }
    }

    // The stuck wake `ticket`'s work item deserves as the board stands: the
    // ruling it is parked on, its idle call, or nothing.
    fn stuck_now(&self, ticket: TicketId) -> Option<Wake> {
        let item = self.state.item(ticket)?;
        let parked = match item.phase {
            Phase::Ruling { id } => self.state.rulings.iter().find(|r| r.id == id),
            _ => None,
        };
        if let Some(wake) = parked.and_then(|r| stuck_wake(ticket, r)) {
            return Some(wake);
        }
        let watched = self.flights.watched(ticket).filter(|w| w.idle)?;
        Some(Wake::Stuck {
            ticket,
            what: IDLE_WHAT.to_owned(),
            state: StuckState::Idle(watched.started),
        })
    }

    /// Whether the board still shows `wake` as a reason, by the same checks
    /// `still_due` uses to queue one: the item still stuck on the same thing,
    /// still waiting for its model, or the pair still in conflict. A reason the
    /// window never asks about is one it never drops.
    pub(super) fn wake_holds(&self, wake: &Wake) -> bool {
        match wake {
            Wake::Stuck { ticket, state, .. } => matches!(
                self.stuck_now(*ticket),
                Some(Wake::Stuck { state: now, .. }) if now == *state
            ),
            Wake::LeaseWait { ticket, .. } => self.lease_wake(*ticket).is_some(),
            Wake::Conflict(a, b, _) => self.pm.conflicts.contains_key(&(*a, *b)),
            _ => true,
        }
    }

    // Whether `ticket`'s work item is still held by the state it was left
    // alone in, naming each drop in the log
    pub(super) fn left_alone_holds(&mut self, ticket: TicketId, state: StuckState) -> bool {
        let Some(entry) = self.pm.left_alone.get(&ticket).cloned() else {
            return false;
        };
        if entry.state != state {
            return false;
        }
        let now = self.ports.clock.now();
        let named = ticket_ref(ticket);
        match state {
            // The ruling already asks the maintainer: there is nothing to wait
            // for but its answer.
            StuckState::Ruling { id, .. } => {
                self.notes.push(format!(
                    "{named} is parked on ruling {id}, which the project manager left alone, \
                     so it is not woken for it again"
                ));
                true
            }
            StuckState::Idle(..) => {
                let Some(until) = entry.until else {
                    return false;
                };
                let held = now < until;
                if held {
                    self.notes.push(format!(
                        "{named} is idle on the turn the project manager left alone {}, so it \
                         is not woken for it again for {}",
                        age(now, entry.at),
                        elapsed(until.0.saturating_sub(now.0))
                    ));
                }
                held
            }
        }
    }

    /// Records that the project manager left `ticket`'s work item alone in
    /// `state`, once more in a row on the same state or new to it
    pub(super) fn pm_left_alone(&mut self, ticket: TicketId, state: StuckState, at: Timestamp) {
        let nones = match self.pm.left_alone.get(&ticket) {
            Some(entry) if entry.state == state => entry.nones.saturating_add(1),
            _ => 1,
        };
        let until = match state {
            StuckState::Ruling { .. } => None,
            StuckState::Idle(..) => Some(Timestamp(at.0.saturating_add(quiet_backoff(nones)))),
        };
        self.pm.left_alone.insert(
            ticket,
            LeftAlone {
                state,
                at,
                until,
                nones,
                asked: false,
            },
        );
    }

    /// Queues each stuck wake whose back-off has ended
    ///
    /// This is what wakes the project manager again for a worker that shows
    /// nothing more: the board flips an idle call only after new output, and
    /// `still_due` runs only after a lapse. Called from `wake_pm`, where its
    /// agent is set up. Each hold is queued once, so an item the project
    /// manager says nothing about is not woken for on every pass.
    pub(super) fn pm_holds_over(&mut self) {
        let now = self.ports.clock.now();
        let over: Vec<TicketId> = self
            .pm
            .left_alone
            .iter()
            .filter(|(_, entry)| !entry.asked && entry.until.is_some_and(|until| now >= until))
            .map(|(ticket, _)| *ticket)
            .collect();
        for ticket in over {
            let Some(entry) = self.pm.left_alone.remove(&ticket) else {
                continue;
            };
            match self.stuck_now(ticket) {
                Some(wake @ Wake::Stuck { state, .. }) if state == entry.state => {
                    self.pm.left_alone.insert(
                        ticket,
                        LeftAlone {
                            asked: true,
                            ..entry
                        },
                    );
                    self.pm_due(wake);
                }
                Some(wake) => self.pm_due(wake),
                None => {}
            }
        }
    }

    /// Notes what saving `now` changes that wakes the project manager: a
    /// work item parked on a stuck ruling, or one closing, which lets its
    /// holds go. A pick of an issue stands whatever opens or closes, since
    /// dispatch checks the board for it.
    pub(in crate::runner) fn pm_noticed(&mut self, now: &ProjectState) {
        if self.agents.pm.is_none() {
            return;
        }
        let was = &self.state;
        let raised = (now.rulings.iter()).filter(|r| !was.rulings.iter().any(|w| w.id == r.id));
        let stuck: Vec<Wake> = raised
            .filter_map(|ruling| {
                let ticket = ruling.issue?;
                let ended = self.pm.after_end.contains_key(&ticket);
                stuck_wake(ticket, ruling).filter(|_| !ended)
            })
            .collect();
        let closed: Vec<TicketId> = (was.work_items.iter())
            .map(|i| i.issue)
            .filter(|&issue| now.item(issue).is_none())
            .collect();
        self.pm_asked(now);
        for wake in stuck {
            self.pm_due(wake);
        }
        if !closed.is_empty() {
            // A hold waits on the work item it names, or on any when it names none.
            (self.pm.held).retain(|held| held.until.is_some_and(|until| !closed.contains(&until)));
            let open = now.open_tickets();
            self.pm.after_end.retain(|ticket, _| open.contains(ticket));
            (self.pm.left_alone).retain(|ticket, _| open.contains(ticket));
            (self.pm.conflicts).retain(|(a, b), _| open.contains(a) && open.contains(b));
        }
    }

    // Wakes the project manager for each question, follow-up and ruling it
    // answers for the maintainer that `now` raises. The ruling is held back
    // from the maintainer for its answer, unless the manager is down or the
    // ruling is held by the maintainer's terminal.
    fn pm_asked(&mut self, now: &ProjectState) {
        let at = self.ports.clock.now();
        let passed = (self.pm.down_until).is_some_and(|until| at < until);
        if passed || now.pm_attached.is_some() {
            return;
        }
        for ruling in &now.rulings {
            let new = !self.state.rulings.iter().any(|r| r.id == ruling.id);
            let id = ruling.id;
            let wake = match (&ruling.kind, ruling.issue, ruling.pull_request, new) {
                (RulingKind::Question { asked, .. }, Some(ticket), _, true) => Wake::Question {
                    ticket,
                    id,
                    asked: asked.clone(),
                },
                // One the forge refused is no call on the diff.
                (
                    RulingKind::FollowUp {
                        findings,
                        refused: None,
                    },
                    Some(ticket),
                    Some(pull_request),
                    true,
                ) => Wake::FollowUp {
                    ticket,
                    id,
                    pull_request,
                    findings: findings.clone(),
                },
                (_, _, _, true) if self.pm_answers_ruling(ruling) => Wake::Answer {
                    id,
                    asked: brief(ruling).0,
                    request: None,
                },
                _ => continue,
            };
            self.pm.asking.insert(id, at);
            self.pm_due(wake);
        }
    }

    /// Wakes the project manager for each pair of open branches newly in
    /// conflict, given every pair in conflict now
    ///
    /// The files a branch conflicts with `main` in are in no pair: the board
    /// reports it behind `main`, and kelpie catches it up without the project
    /// manager.
    pub(in crate::runner) fn pm_conflicts(
        &mut self,
        now: BTreeMap<(TicketId, TicketId), Vec<String>>,
    ) {
        if self.agents.pm.is_none() {
            return;
        }
        for (&(a, b), files) in &now {
            if !self.pm.conflicts.contains_key(&(a, b)) {
                self.pm_due(Wake::Conflict(a, b, files.clone()));
            }
        }
        self.pm.conflicts = now;
    }
}

// The wake for `ticket`'s work item, parked on `ruling`, if that is a stuck
// item's.
fn stuck_wake(ticket: TicketId, ruling: &Ruling) -> Option<Wake> {
    let what = stuck_on(&ruling.kind)?;
    Some(Wake::Stuck {
        ticket,
        what: format!("{what}, and ruling {} asks the maintainer", ruling.id),
        state: StuckState::Ruling {
            id: ruling.id,
            raised: ruling.raised,
        },
    })
}

/// What a ruling of `kind` says of a stuck work item, or none for one that
/// is not about a stuck item
pub(in crate::runner) fn stuck_on(kind: &RulingKind) -> Option<&'static str> {
    match kind {
        RulingKind::Stuck(Stuck::TurnFailed { .. }) => Some(
            "its worker's turn parked: the call failed once, the model host stayed unreachable for 30 minutes, or the turn stopped short twice",
        ),
        RulingKind::Stuck(Stuck::TurnTimeout { .. }) => {
            Some("its worker's turn ran past its ceiling")
        }
        RulingKind::Stuck(Stuck::StillRed {
            fix_turns: None, ..
        }) => Some("CI failed again and the worker pushed no fix"),
        RulingKind::Stuck(Stuck::StillRed { .. }) => {
            Some("CI failed and the worker's fix turns reached the cap `ci.fix_attempts` sets")
        }
        _ => None,
    }
}
