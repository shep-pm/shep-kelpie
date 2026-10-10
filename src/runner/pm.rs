//! The project manager's agent: woken by the runner on the board's events,
//! briefed from the board, and checked against it before kelpie acts
//!
//! A wake comes from a free slot with two or more ready issues, two open
//! branches that conflict, a stuck work item, or the maintainer's `tell`.
//! The reasons gathered since the last wake go in one call, a lamb like any
//! other, one at a time, which the loop never waits on. Its session is the
//! project's one, resumed each wake and compacted past [`COMPACT_PAST`].
//! While it is not set up, down, over its pace, or its answer is dropped,
//! the board rule picks and stuck items keep the rulings they raised.

use std::collections::BTreeMap;

use super::Runner;
use super::report::StepReport;
use crate::pacer::Scope;
use crate::ports::{AgentError, AgentReply, Session, SessionId, Timestamp};
use crate::settings::PmAgent;
use crate::state::StateError;
use crate::usage::CallKind;
use crate::work_item::new_session_id;

mod act;
mod answer;
mod call;
mod maintainer;
mod prompt;
#[cfg(test)]
mod tests;
mod wakes;

pub use maintainer::{PmAttaching, PmError, PmStatus};

use answer::Action;
use wakes::stuck_on;

/// The context, in tokens, past which the session is compacted after a wake
///
/// Measured, one session compacted as it goes was the cheapest shape a day
/// of wakes ran in, and lost no probe of what it knew.
pub(super) const COMPACT_PAST: u64 = 100_000;

/// How long one call may run before it is ended, in seconds. A wake ran
/// 12 to 26 seconds when measured.
pub(super) const CEILING: u64 = 10 * 60;

/// How long the project manager is passed over after a call fails, in seconds
const DOWN_FOR: u64 = 10 * 60;

/// How long a pick of none holds the board with no work item open, in
/// seconds, before the board rule picks
pub(super) const NOTHING_FOR: u64 = 30 * 60;

/// What `/compact` is sent as
const COMPACT: &str = "/compact";

/// Why the project manager is woken
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Wake {
    /// A slot is free, and this many ready issues could fill it
    Pick(usize),
    /// The branches of two open work items conflict in these files
    Conflict(u64, u64, Vec<String>),
    /// A work item is stuck, as this says
    Stuck(u64, String),
    /// The maintainer told it something, which the state file holds
    Told,
}

impl Wake {
    // Whether `other` is the same reason, whatever it says, so each is queued once.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Pick(_), Self::Pick(_)) | (Self::Told, Self::Told) => true,
            (Self::Conflict(a, b, _), Self::Conflict(c, d, _)) => (a, b) == (c, d),
            (Self::Stuck(a, _), Self::Stuck(b, _)) => a == b,
            _ => false,
        }
    }
}

/// The project manager's last word on what the board starts next
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pick {
    /// This ready issue
    Issue(u64),
    /// Nothing, decided at this time, until its next wake, or for
    /// [`NOTHING_FOR`] while no work item is open
    Nothing(Timestamp),
    /// Whatever the board rule picks, since it gave no answer
    Rule,
}

/// What dispatch does about a pick
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Choice {
    /// Starts this issue, the project manager's pick
    Take(u64),
    /// Starts what the board rule picked
    Rule,
    /// Starts nothing now: the project manager is deciding, or said so
    Wait,
}

/// What the project manager's call is for
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Wake,
    Compact,
}

/// The call in flight, as the runner needs it once it ends
#[derive(Debug)]
struct Flying {
    kind: Kind,
    wakes: Vec<Wake>,
    // The last board event when it was woken, its cursor once it answers
    read_to: u64,
    // How many of the state file's `pm_told` the prompt carried
    told: usize,
    session: SessionId,
    fresh: bool,
}

/// What the runner keeps about its project manager, in memory only
#[derive(Debug, Default)]
pub(super) struct Desk {
    due: Vec<Wake>,
    // A pick wake is due or in flight
    asked_pick: bool,
    pick: Option<Pick>,
    held: Vec<u64>,
    // Each pair of open branches in conflict, with the files
    conflicts: BTreeMap<(u64, u64), Vec<String>>,
    // A stuck item or conflict was given up while it was unavailable
    lapsed: bool,
    down_until: Option<Timestamp>,
    failures: u32,
    // A new session on the next wake, or the full prompt after a compaction
    fresh: bool,
    full_prompt: bool,
    compact: bool,
    reply: Option<String>,
    // What it decided for an idle worker whose call it had ended
    after_end: BTreeMap<u64, (Action, String)>,
    flying: Option<Flying>,
}

impl Runner {
    /// Queues `wake` for the project manager's next call, once each
    pub(super) fn pm_due(&mut self, wake: Wake) {
        if self.agents.pm.is_some() && !self.pm.due.iter().any(|w| w.same(&wake)) {
            self.pm.due.push(wake);
        }
    }

    /// Forgets a pick wake still due, since the board picks nothing while
    /// the runner finishes. A wake in flight still answers, and its pick
    /// waits unused.
    pub(super) fn pm_forget_pick(&mut self) {
        let due = self.pm.due.len();
        self.pm.due.retain(|wake| !matches!(wake, Wake::Pick(_)));
        if self.pm.due.len() < due {
            self.pm.asked_pick = false;
        }
    }

    /// The ready issues the project manager holds back, out of `takeable`,
    /// those the board could start. With no work item open, a hold over
    /// every one of them has nothing to wait for, so it goes.
    pub(super) fn pm_holds(&mut self, takeable: &[u64]) -> Vec<u64> {
        let all = !takeable.is_empty() && takeable.iter().all(|n| self.pm.held.contains(n));
        if all && self.state.work_items.is_empty() {
            self.notes.push(
                "the project manager holds every issue the board could start, with no work \
                 item open to wait for, so its holds go"
                    .to_owned(),
            );
            self.pm.held.clear();
        }
        self.pm.held.clone()
    }

    /// The ready issues the project manager holds back, as the board shows them
    pub(super) fn pm_held(&self) -> &[u64] {
        &self.pm.held
    }

    /// What dispatch starts out of `takeable`, the ready issues the board
    /// could take now: the project manager's pick, or nothing while it said
    /// so, or it is asked where two or more could start; else the board rule's
    pub(super) fn pm_pick(&mut self, takeable: &[u64]) -> Choice {
        if self.agents.pm.is_none() {
            return Choice::Rule;
        }
        match self.pm.pick.take() {
            Some(Pick::Issue(n)) if takeable.contains(&n) => Choice::Take(n),
            Some(Pick::Issue(n)) => {
                self.notes.push(format!(
                    "the project manager's pick #{n} is not an issue the board can take \
                     now, so the board rule picks"
                ));
                Choice::Rule
            }
            Some(Pick::Rule) => Choice::Rule,
            Some(Pick::Nothing(at)) => {
                let now = self.ports.clock.now();
                let lapsed = now.0 >= at.0.saturating_add(NOTHING_FOR);
                if lapsed && self.state.work_items.is_empty() {
                    self.notes.push(
                        "the project manager started nothing for 30 minutes with no work item \
                         open, so the board rule picks"
                            .to_owned(),
                    );
                    return Choice::Rule;
                }
                self.pm.pick = Some(Pick::Nothing(at));
                Choice::Wait
            }
            None if takeable.len() < 2 || !self.pm_available() => Choice::Rule,
            None if self.pm.asked_pick => Choice::Wait,
            None => {
                self.pm.asked_pick = true;
                self.pm_due(Wake::Pick(takeable.len()));
                Choice::Wait
            }
        }
    }

    // Set up, not held by the maintainer's terminal, not down, and within
    // its account's pace.
    fn pm_available(&mut self) -> bool {
        let Some(limit) = self.agents.pm.as_ref().map(|pm| pm.limit.clone()) else {
            return false;
        };
        if self.pm_held_by_terminal() {
            return false;
        }
        let now = self.ports.clock.now();
        if self.pm.down_until.is_some_and(|until| now < until) {
            return false;
        }
        matches!(self.pace(Scope::Turn, &limit), Ok(super::pace::Pace::Clear))
    }

    /// Starts the project manager's call if one is due and it can run,
    /// and says whether it did
    ///
    /// A compaction owed goes first. One that cannot run gives the
    /// reasons up: a pick falls to the board rule, and a stuck item keeps
    /// its ruling. What the maintainer told it waits for the next wake.
    pub(super) fn wake_pm(&mut self) -> bool {
        let Some(agent) = self.agents.pm.clone() else {
            return false;
        };
        if self.pm.flying.is_some() || self.draining {
            return false;
        }
        // What the maintainer told it outlasts a restart, and wakes it again.
        if !self.state.pm_told.is_empty() {
            self.pm_due(Wake::Told);
        }
        if self.pm.lapsed && self.pm_available() {
            self.pm.lapsed = false;
            self.still_due();
        }
        if self.pm.due.is_empty() && !self.pm.compact {
            return false;
        }
        if !self.pm_available() {
            self.give_up();
            return false;
        }
        let launched = match self.pm.compact {
            true => self.launch_compact(&agent),
            false => self.launch_wake(&agent),
        };
        if let Err(why) = &launched {
            self.notes
                .push(format!("cannot wake the project manager: {why}"));
            self.give_up();
        }
        launched.is_ok()
    }

    fn give_up(&mut self) {
        for wake in std::mem::take(&mut self.pm.due) {
            match wake {
                Wake::Pick(_) => {
                    self.pm.pick = Some(Pick::Rule);
                    self.pm.asked_pick = false;
                }
                Wake::Told => self.pm_due(Wake::Told),
                Wake::Conflict(..) | Wake::Stuck(..) => self.pm.lapsed = true,
            }
        }
    }

    fn launch_wake(&mut self, agent: &PmAgent) -> Result<(), String> {
        let fresh = self.pm.fresh || self.state.pm_session.is_none();
        let id = match (&self.state.pm_session, fresh) {
            (Some(id), false) => id.clone(),
            _ => new_session_id().map_err(|e| format!("cannot draw a session id: {e}"))?,
        };
        let session = match fresh {
            true => Session::New(id.clone()),
            false => Session::Resume(id.clone()),
        };
        let wakes = std::mem::take(&mut self.pm.due);
        let told = self.state.pm_told.clone();
        let full = fresh || self.pm.full_prompt;
        let text = prompt::wake(self.project.as_str(), &wakes, &told, full);
        let call = match self.pm_call(agent, session, text) {
            Ok(call) => call,
            Err(why) => {
                self.pm.due = wakes;
                return Err(why);
            }
        };
        self.pm.flying = Some(Flying {
            kind: Kind::Wake,
            wakes,
            read_to: self.state.last_event,
            told: told.len(),
            session: id,
            fresh,
        });
        self.launch_pm(call, CEILING, agent.name.as_str(), CallKind::Wake);
        Ok(())
    }

    fn launch_compact(&mut self, agent: &PmAgent) -> Result<(), String> {
        self.pm.compact = false;
        let Some(id) = self.state.pm_session.clone() else {
            return Ok(());
        };
        let call = self.pm_call(agent, Session::Resume(id.clone()), COMPACT.to_owned())?;
        self.pm.flying = Some(Flying {
            kind: Kind::Compact,
            wakes: Vec::new(),
            read_to: self.state.last_event,
            told: 0,
            session: id,
            fresh: false,
        });
        self.launch_pm(call, CEILING, agent.name.as_str(), CallKind::Compact);
        Ok(())
    }

    /// Records the end of the project manager's call, and acts on its answer
    ///
    /// # Errors
    ///
    /// [`StateError`] when its session, cursor or what it acted on cannot
    /// be saved.
    pub(super) fn pm_ended(
        &mut self,
        result: Result<AgentReply, AgentError>,
    ) -> Result<Option<StepReport>, StateError> {
        let Some(flying) = self.pm.flying.take() else {
            return Ok(None);
        };
        if flying.kind == Kind::Compact {
            // Even a compaction whose result went unread may have dropped the answer's shape.
            self.pm.full_prompt = true;
            return Ok(Some(match result {
                Ok(_) => StepReport::PmCompacted {
                    session: flying.session,
                },
                Err(e) => StepReport::PmFailed {
                    reason: format!("cannot compact its session: {e}"),
                },
            }));
        }
        let reply = match result {
            Ok(reply) => reply,
            // Its session cannot be resumed, so a new one starts from the board.
            Err(AgentError::NoSession(..)) if !flying.fresh => {
                let later = std::mem::take(&mut self.pm.due);
                for wake in flying.wakes.into_iter().chain(later) {
                    self.pm_due(wake);
                }
                self.pm.fresh = true;
                let started = self.wake_pm();
                return Ok((!started).then(|| StepReport::PmFailed {
                    reason: "its session could not be resumed, nor a new one started".into(),
                }));
            }
            Err(AgentError::Stopped) => return Ok(None),
            Err(e) => return Ok(Some(self.pm_failed(flying, e.to_string()))),
        };
        let Some(answer) = answer::read(&reply.text) else {
            let why = "its reply held no answer kelpie reads";
            return Ok(Some(self.pm_failed(flying, why.into())));
        };
        self.pm.failures = 0;
        self.pm.fresh = false;
        self.pm.full_prompt = false;
        self.pm.down_until = None;
        self.pm.compact = reply.context.is_some_and(|c| c > COMPACT_PAST);
        let mut next = self.state.clone();
        next.pm_session = Some(flying.session.clone());
        next.pm_seen = Some(flying.read_to);
        next.pm_told.drain(..flying.told.min(next.pm_told.len()));
        self.save(next)?;
        let woke_for = flying.wakes.iter().map(prompt::reason).collect();
        let picking = flying.wakes.iter().any(|w| matches!(w, Wake::Pick(_)));
        let (acted, dropped) = self.act(&answer, picking)?;
        Ok(Some(StepReport::PmAnswered {
            session: flying.session,
            woke_for,
            acted,
            dropped,
            reply: answer.reply,
            why: answer.why,
            session_cost_usd: reply.session_cost.map(crate::ports::Cost::usd),
            context: reply.context,
        }))
    }

    // A wake that failed: it is passed over for a while, its pick falls to
    // the rule, and two in a row start a new session.
    fn pm_failed(&mut self, flying: Flying, reason: String) -> StepReport {
        self.pm.failures = self.pm.failures.saturating_add(1);
        self.pm.fresh = self.pm.failures >= 2;
        let now = self.ports.clock.now();
        self.pm.down_until = Some(Timestamp(now.0.saturating_add(DOWN_FOR)));
        for wake in flying.wakes {
            self.pm_due(wake);
        }
        self.give_up();
        StepReport::PmFailed { reason }
    }
}
