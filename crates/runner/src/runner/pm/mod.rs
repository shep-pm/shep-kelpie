//! The project manager's agent: woken by the runner on the board's events,
//! briefed from the board, and checked against it before kelpie acts
//!
//! A wake comes from a free slot with two or more ready issues, two open
//! branches that conflict, a stuck work item, a worker's question, or the
//! maintainer's `tell`. A question's ruling is held back for at most one
//! wake, and so is a follow-up ruling, which it can only pass on. The reasons
//! since the last wake go in one call, which the loop never waits on. Its
//! session is the project's one, resumed and compacted past [`COMPACT_PAST`].
//! While it is down, the board rule picks and stuck items keep their rulings.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use super::Runner;
use super::report::StepReport;
use super::ruling::PostJob;
use super::trigger::lock;
use crate::board::ReadyIssue;
use crate::board::briefing::ticket_ref;
use crate::pacer::Scope;
use crate::ports::{
    AgentError, AgentReply, COMPACT, Finding, PaddockHolder, Session, SessionId, Timestamp, Wait,
};
use crate::settings::PmAgent;
use crate::state::StateError;
use crate::ticket::TicketId;
use crate::usage::CallKind;
use crate::work_item::new_session_id;

mod act;
mod answer;
mod asked;
mod call;
mod ci_logs;
mod diffs;
mod maintainer;
mod notes;
mod picked;
mod prompt;
mod question;
mod red;
mod requests;
mod rulings;
#[cfg(test)]
mod tests;
mod wakes;

pub(super) use asked::{PmReply, pm_reply};
pub(super) use ci_logs::wake_off_lock;
pub use maintainer::{Held, PmAttaching, PmError, PmStatus, Settled};
pub(super) use picked::Picked;

use answer::Action;
use ci_logs::Woke;
use wakes::{LeftAlone, stuck_on};

/// The context, in tokens, past which the session is compacted after a wake
///
/// Measured, one session compacted as it goes was the cheapest shape a day
/// of wakes ran in, and lost no probe of what it knew.
pub(super) const COMPACT_PAST: u64 = 100_000;

/// The context, in tokens, past which a worker's session is compacted
/// before the next turn the runner sends it
///
/// Every turn after the build resumes the session and rereads all of it. On
/// four work items it ended at 284k to 486k tokens, and its turns after the
/// build cost as much as the build or more, three quarters of it cache reads.
/// Set above [`COMPACT_PAST`]: a build alone reads a good deal of the repo.
pub(super) const WORKER_COMPACT_PAST: u64 = 150_000;

/// How long one call may run before it is ended, in seconds. A wake ran
/// 12 to 26 seconds when measured.
pub(super) const CEILING: u64 = 10 * 60;

/// How long the project manager is passed over after a call fails, in seconds
const DOWN_FOR: u64 = 10 * 60;

/// How long a pick of none holds the board with no work item open, in
/// seconds, before the board rule picks
pub(super) const NOTHING_FOR: u64 = 30 * 60;

/// How long a wake reason nobody waits on is held before it goes, in seconds
///
/// Measured on the maintainer's day of 134 wakes: 71 of them began less than 5
/// minutes after the project's previous wake, and each wake pays for its whole
/// starting context (median 54k tokens). A stuck item, a conflict and a lease
/// wait are reasons no one is waiting on, so each waits this long, and the ones
/// that arrive inside the window go in the same call.
pub(super) const HOLD_FOR: u64 = 3 * 60;

/// What a work item is stuck on, which a new turn, phase or ruling is a
/// different one of
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StuckState {
    /// Parked on this ruling, raised then. A ruling raised again on the same
    /// item is a different one, whatever its number.
    Ruling {
        /// The ruling's number
        id: TicketId,
        /// When it was raised, `None` only in a state file from before
        raised: Option<Timestamp>,
    },
    /// Its call in flight has shown nothing, on the turn that started then
    Idle(Timestamp),
}

/// Why the project manager is woken
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Wake {
    /// Slots are free, and this many ready issues could fill them, then
    /// this many slots are free
    Pick(usize, usize),
    /// The branches of two open work items conflict in these files
    Conflict(TicketId, TicketId, Vec<String>),
    /// A work item is stuck, as this says, on this
    Stuck {
        /// The work item's ticket
        ticket: TicketId,
        /// What it is stuck on, in words
        what: String,
        /// What it is stuck on, which a new turn, phase or ruling is another of
        state: StuckState,
    },
    /// A call in flight has waited long for a lease on its model, which is not
    /// the worker being stuck
    LeaseWait {
        /// The work item's ticket
        ticket: TicketId,
        /// Whether the call that waits is a worker's turn, not a review call
        worker: bool,
        /// What its call waits for
        why: Wait,
        /// How long it has waited, in words
        waited: String,
        /// Who holds the lease, as the gateway says it, when the gateway says
        holder: Option<PaddockHolder>,
    },
    /// The maintainer told it something, which the state file holds
    Told,
    /// A worker asked a question, which ruling `id` holds for the maintainer
    Question {
        /// The work item's ticket
        ticket: TicketId,
        /// The ruling
        id: TicketId,
        /// The worker's question, verbatim
        asked: String,
    },
    /// A pull request merged with confirmed findings left unfixed, which
    /// ruling `id` holds for the maintainer to file or drop
    FollowUp {
        /// The work item's ticket, whose folder holds the merged diff
        ticket: TicketId,
        /// The ruling
        id: TicketId,
        /// The merged pull request
        pull_request: u64,
        /// The findings
        findings: Vec<Finding>,
    },
    /// A workflow is red on `main` at this head after kelpie's rerun, which the
    /// project manager fixes or asks about
    Red {
        /// The workflow's name
        workflow: String,
        /// The head that stayed red
        head: String,
    },
    /// A ruling waits on the project manager's answer
    Answer {
        /// The ruling
        id: TicketId,
        /// What it asks and the options
        asked: String,
        /// What the maintainer asked of it, or none where the project
        /// leaves the ruling to it
        request: Option<requests::Request>,
    },
}

impl Wake {
    // Whether `other` is the same reason, whatever it says, so each is queued once.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Pick(..), Self::Pick(..)) | (Self::Told, Self::Told) => true,
            (Self::Conflict(a, b, _), Self::Conflict(c, d, _)) => (a, b) == (c, d),
            (Self::Stuck { ticket: a, .. }, Self::Stuck { ticket: b, .. }) => a == b,
            (Self::LeaseWait { ticket: a, .. }, Self::LeaseWait { ticket: b, .. }) => a == b,
            (Self::Question { id: a, .. }, Self::Question { id: b, .. }) => a == b,
            (Self::FollowUp { id: a, .. }, Self::FollowUp { id: b, .. }) => a == b,
            (Self::Answer { id: a, .. }, Self::Answer { id: b, .. }) => a == b,
            (Self::Red { workflow: a, .. }, Self::Red { workflow: b, .. }) => a == b,
            _ => false,
        }
    }
}

/// The project manager's last word on what the board starts next
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Pick {
    /// These ready issues, in the order dispatch starts them, one a step
    Issues(Vec<Picked>),
    /// Nothing, decided at this time, until its next wake, or for
    /// [`NOTHING_FOR`] while no work item is open
    Nothing(Timestamp),
    /// Whatever the board rule picks, since it gave no answer
    Rule,
}

/// What dispatch does about a pick
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Choice {
    /// Starts this issue, the project manager's pick
    Take(Picked),
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

/// Whether nobody waits on `wake`, so it can wait [`HOLD_FOR`] for the rest
///
/// A free slot, a `tell`, a worker's question, a follow-up ruling, a ruling to
/// answer and a pick are each waited on: the board holds a slot, a ruling is
/// held back from the maintainer, or the maintainer is asking. A stuck item, a
/// conflict and a lease wait are each reasons the runner itself noticed, and no
/// one is waiting on them.
fn held(wake: &Wake) -> bool {
    matches!(
        wake,
        Wake::Stuck { .. } | Wake::Conflict(..) | Wake::LeaseWait { .. }
    )
}

/// The call in flight, as the runner needs it once it ends
#[derive(Debug)]
struct Flying {
    kind: Kind,
    wakes: Vec<Wake>,
    // The last board event when it was woken, its cursor once it answers
    read_to: u64,
    // What the state file's `pm_told` the prompt carried, which the standing notes keep
    told: Vec<String>,
    // The dropped picks the prompt told, which the next wake tells again if this one fails
    lost: Vec<String>,
    session: SessionId,
    fresh: bool,
}

/// What the runner keeps about its project manager, in memory only
#[derive(Debug, Default)]
pub(super) struct Desk {
    due: Vec<Wake>,
    // A pick wake is due or in flight
    asked_pick: bool,
    // Each red head it was woken for, so a head wakes it once
    red_woken: BTreeSet<(String, String)>,
    // Since when a window of held reasons has been open
    holding_since: Option<Timestamp>,
    pick: Option<Pick>,
    // Each of its picks kelpie dropped, in words, until a wake tells it
    lost: Vec<String>,
    // Each ready issue it holds back, with what it waits on
    held: Vec<Held>,
    // Each pair of open branches in conflict, with the files
    conflicts: BTreeMap<(TicketId, TicketId), Vec<String>>,
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
    after_end: BTreeMap<TicketId, (Action, String)>,
    // Each work item it left to resolve itself, with the state it was in, when
    // it said so, how many `none`s in a row on that state, and when its hold
    // ends
    left_alone: BTreeMap<TicketId, LeftAlone>,
    // Each question ruling held back from the maintainer for its answer, with
    // when it was raised
    asking: BTreeMap<TicketId, Timestamp>,
    // Each of those whose comment on its thread waits for the hold to end
    unposted: BTreeSet<TicketId>,
    // The last few questions it settled, newest last
    settled: Vec<Settled>,
    flying: Option<Flying>,
    // The CI logs a wake's launch writes, read with the lock let go, while it launches
    logs: Option<ci_logs::Logs>,
}

impl Desk {
    /// Whether a call of its session is in flight
    pub(super) fn waking(&self) -> bool {
        self.flying.is_some()
    }
}

/// Posts the comment of each held ruling whose hold has ended, with the runner's lock let go
pub(super) fn post_released(runner: &Mutex<Runner>) {
    let posts = lock(runner).release_posts();
    for post in posts {
        let id = post.id();
        let sent = post.send();
        let mut held = lock(runner);
        if let Some(why) = held.ruling_posted(sent) {
            held.notes
                .push(format!("cannot post ruling {id} on its thread: {why}"));
        }
    }
}

impl Runner {
    /// Queues `wake` for the project manager's next call, once each
    ///
    /// A stuck item it left alone in the same state is queued again only when
    /// that state changes or its back-off ends. The window a held reason
    /// waits opens with the first one queued, so it runs from what the board
    /// saw. A newer state for the same item replaces the queued reason.
    pub(super) fn pm_due(&mut self, wake: Wake) {
        if self.agents.pm.is_none() {
            return;
        }
        if let Wake::Stuck { ticket, state, .. } = &wake
            && self.left_alone_holds(*ticket, *state)
        {
            return;
        }
        if let Some(existing) = self.pm.due.iter_mut().find(|w| w.same(&wake)) {
            // An item stuck on something new replaces the reason queued for
            // it, so the wake carries what the board shows when its window
            // ends. An item read idle whose turn then parks on a ruling is the
            // case. The window still runs from the first reason queued.
            let changed = matches!(
                (&*existing, &wake),
                (Wake::Stuck { state: was, .. }, Wake::Stuck { state, .. }) if was != state
            );
            if changed {
                *existing = wake;
            }
            return;
        }
        if held(&wake) && self.pm.holding_since.is_none() {
            self.pm.holding_since = Some(self.ports.clock.now());
        }
        self.pm.due.push(wake);
    }

    /// Forgets a pick wake still due, since the board picks nothing while
    /// the runner finishes. A wake in flight still answers, and its pick
    /// waits unused.
    pub(super) fn pm_forget_pick(&mut self) {
        let due = self.pm.due.len();
        self.pm.due.retain(|wake| !matches!(wake, Wake::Pick(..)));
        if self.pm.due.len() < due {
            self.pm.asked_pick = false;
        }
    }

    /// The ready issues the project manager holds back, out of `takeable`,
    /// those the board could start. A hold on an issue whose text changed
    /// since goes, since the project manager held what it read. With no work
    /// item open, a hold over every one of them has nothing to wait for, so
    /// it goes.
    pub(super) fn pm_holds(
        &mut self,
        takeable: &[TicketId],
        ready: &[ReadyIssue],
    ) -> Vec<TicketId> {
        let edited = |held: &Held| {
            let issue = ready.iter().find(|r| r.ticket == held.issue);
            issue.is_some_and(|r| r.fingerprint() != held.text)
        };
        self.pm.held.retain(|held| !edited(held));
        let held = |n: &TicketId| self.pm.held.iter().any(|h| h.issue == *n);
        let all = !takeable.is_empty() && takeable.iter().all(held);
        if all && self.state.work_items.is_empty() {
            self.notes.push(
                "the project manager holds every issue the board could start, with no work \
                 item open to wait for, so its holds go"
                    .to_owned(),
            );
            self.pm.held.clear();
        }
        self.pm.held.iter().map(|h| h.issue).collect()
    }

    /// The ready issues the project manager holds back, as the board shows them
    pub(super) fn pm_held(&self) -> &[Held] {
        &self.pm.held
    }

    /// What dispatch starts out of `takeable`, the ready issues the board
    /// could take now: the project manager's pick, or nothing while it said
    /// so, or it is asked where two or more could start; else the board rule's.
    pub(super) fn pm_pick(&mut self, takeable: &[TicketId]) -> Choice {
        if self.agents.pm.is_none() {
            return Choice::Rule;
        }
        match self.pm.pick.take() {
            Some(Pick::Issues(mut named)) => {
                // The head stays until its work item opens (`pm_started`). One
                // it cannot start falls to the next it named, and when none is
                // left, to the board rule.
                while let Some(head) = named.first().cloned() {
                    if takeable.contains(&head.issue) {
                        self.pm.pick = Some(Pick::Issues(named));
                        return Choice::Take(head);
                    }
                    named.remove(0);
                    let why = "it is not an issue the board can take now".to_owned();
                    self.pick_dropped(head.issue, why);
                }
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
                self.pm_due(Wake::Pick(takeable.len(), self.free_slots()));
                Choice::Wait
            }
        }
    }

    /// Takes `ticket` off the project manager's picks, since its work item
    /// opened
    pub(super) fn pm_started(&mut self, ticket: TicketId) {
        if let Some(Pick::Issues(named)) = &mut self.pm.pick {
            named.retain(|p| p.issue != ticket);
            if named.is_empty() {
                self.pm.pick = None;
            }
        }
    }

    // Logs that the pick of `ticket` was dropped for `why`, and keeps it to
    // tell the project manager at its next wake.
    fn pick_dropped(&mut self, ticket: TicketId, why: String) {
        self.pm.lost.push(format!(
            "kelpie dropped your pick {} and the board rule picked: {why}",
            ticket_ref(ticket)
        ));
        self.started
            .push(StepReport::PickDropped { issue: ticket, why });
    }

    // Set up, not held by the maintainer's terminal, not down, and within
    // its account's pace.
    pub(super) fn pm_available(&mut self) -> bool {
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
        matches!(
            self.pace_now(Scope::Turn, &limit),
            Ok(super::pace::Pace::Clear)
        )
    }

    /// Starts the project manager's call if one is due and it can run,
    /// and says whether it did
    ///
    /// A compaction owed goes first. One that cannot run gives the reasons up:
    /// a pick falls to the board rule, and a stuck item keeps its ruling. What
    /// the maintainer told it waits for the next wake. A stuck item it left
    /// alone is due once its back-off ends.
    pub(super) fn wake_pm(&mut self) -> bool {
        matches!(self.wake(false), Woke::Did(true))
    }

    /// [`Runner::wake_pm`], except that a wake about to launch whose CI logs are not read
    /// hands back the reads, which [`wake_off_lock`] runs with the lock let go
    fn wake_pm_or_reads(&mut self) -> Woke {
        self.wake(true)
    }

    // Everything before the launch is checked again by the try that follows a read, and drops
    // or defers the same, so a try that hands back reads has changed nothing the next does not.
    fn wake(&mut self, reads: bool) -> Woke {
        self.pm_requests_kept();
        let Some(agent) = self.agents.pm.clone() else {
            return Woke::Did(false);
        };
        if self.pm.flying.is_some() || self.draining() {
            return Woke::Did(false);
        }
        // What the maintainer told it outlasts a restart, and wakes it again.
        if !self.state.pm_told.is_empty() {
            self.pm_due(Wake::Told);
        }
        self.pm_reds();
        if self.pm.lapsed && self.pm_available() {
            self.pm.lapsed = false;
            self.still_due();
        }
        self.pm_holds_over();
        if self.pm.due.is_empty() && !self.pm.compact {
            return Woke::Did(false);
        }
        // A reason nobody waits on waits its window, so the ones that arrive
        // inside it go in the same call.
        if !self.pm.compact && self.pm.due.iter().all(held) {
            let now = self.ports.clock.now();
            let since = match self.pm.holding_since {
                Some(since) => since,
                None => {
                    self.pm.holding_since = Some(now);
                    return Woke::Did(false);
                }
            };
            if now.0 < since.0.saturating_add(HOLD_FOR) {
                return Woke::Did(false);
            }
        }
        // The window is over, or a reason someone waits on ended it early.
        // Either way the call carries only reasons the board still shows, and
        // when none is left, nothing goes.
        let due = std::mem::take(&mut self.pm.due);
        self.pm.due = due
            .into_iter()
            .filter(|wake| !held(wake) || self.wake_holds(wake))
            .collect();
        if self.pm.due.is_empty() {
            self.pm.holding_since = None;
            if !self.pm.compact {
                return Woke::Did(false);
            }
        }
        if !self.pm_available() {
            self.give_up();
            return Woke::Did(false);
        }
        // A compaction writes no logs. A wake with none in hand reads them, if it may.
        if reads
            && !self.pm.compact
            && self.pm.logs.is_none()
            && let Some(job) = self.pm_logs_job()
        {
            return Woke::Reads(Box::new(job));
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
        let woke = launched.is_ok();
        // The project manager's call is in flight from here.
        if woke {
            self.post();
        }
        Woke::Did(woke)
    }

    fn give_up(&mut self) {
        self.pm.holding_since = None;
        for wake in std::mem::take(&mut self.pm.due) {
            match wake {
                Wake::Pick(..) => {
                    self.pm.pick = Some(Pick::Rule);
                    self.pm.asked_pick = false;
                }
                Wake::Told => self.pm_due(Wake::Told),
                Wake::Conflict(..) | Wake::Stuck { .. } => self.pm.lapsed = true,
                Wake::LeaseWait { .. } | Wake::Red { .. } => self.pm.lapsed = true,
                // The ruling goes to the maintainer, who need not wait on it again.
                Wake::Question { id, .. } | Wake::FollowUp { id, .. } => {
                    self.pm.asking.remove(&id);
                }
                Wake::Answer { id, request, .. } => {
                    self.pm.asking.remove(&id);
                    if let Some(request) = request {
                        self.pm_request_lost(id, request);
                    }
                }
            }
        }
    }

    /// Whether ruling `id` waits on the project manager's answer, at most
    /// one wake, and so is not posted to the maintainer yet
    pub(super) fn ruling_held(&self, id: TicketId) -> bool {
        !self.draining() && self.pm_has_asked(id)
    }

    // Whether ruling `id` was put to the project manager, at most one wake
    // ago, a drain or not.
    fn pm_has_asked(&self, id: TicketId) -> bool {
        let now = self.ports.clock.now();
        let by = |since: &Timestamp| now.0 < since.0.saturating_add(CEILING);
        self.agents.pm.is_some() && self.pm.asking.get(&id).is_some_and(by)
    }

    /// Holds ruling `id`'s comment on its thread back while the project
    /// manager may answer it, and says whether it did
    pub(super) fn pm_holds_comment(&mut self, id: TicketId) -> bool {
        let held = self.ruling_held(id);
        if held {
            self.pm.unposted.insert(id);
            self.mark_unposted(id, true);
        }
        held
    }

    /// Keeps on ruling `id` whether its comment is still to be posted, so a
    /// runner that restarts mid-hold posts it
    pub(super) fn mark_unposted(&mut self, id: TicketId, unposted: bool) {
        let differs = (self.state.rulings.iter().find(|r| r.id == id))
            .is_some_and(|r| r.unposted != unposted);
        if !differs {
            return;
        }
        let mut next = self.state.clone();
        if let Some(ruling) = next.rulings.iter_mut().find(|r| r.id == id) {
            ruling.unposted = unposted;
        }
        if let Err(e) = self.save(next) {
            self.notes.push(format!(
                "cannot save whether ruling {id}'s comment is posted: {e}"
            ));
        }
    }

    /// Settles the comment of each held ruling whose hold has ended and which still waits on the
    /// maintainer, as the project manager left it, and returns the posts to make
    pub(super) fn release_posts(&mut self) -> Vec<PostJob> {
        // A ruling held when the runner last stopped is still to be posted.
        let kept: Vec<TicketId> = (self.state.rulings.iter())
            .filter(|r| r.unposted)
            .map(|r| r.id)
            .collect();
        self.pm.unposted.extend(kept);
        let released: Vec<TicketId> = (self.pm.unposted.iter().copied())
            .filter(|&id| !self.ruling_held(id))
            .collect();
        let mut posts = Vec::new();
        for id in released {
            self.pm.unposted.remove(&id);
            if self.draining() && self.pm_has_asked(id) {
                self.notes.push(format!(
                    "ruling {id} goes to the maintainer without the project manager's answer, \
                     since the runner drains"
                ));
            }
            let Some(number) =
                (self.state.rulings.iter().find(|r| r.id == id)).map(|ruling| ruling.pull_request)
            else {
                continue;
            };
            let post = self.ruling_post(number, id);
            if post.is_some() {
                self.posting_begins(id);
            }
            posts.extend(post);
        }
        posts
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
        self.pm.holding_since = None;
        let told = self.state.pm_told.clone();
        let lost = std::mem::take(&mut self.pm.lost);
        let notes = self.pm_notes();
        let full = fresh || self.pm.full_prompt;
        // The board the wake carries in its prompt, and writes to `board.md`
        // for a re-read and for the `## Code` paths it names.
        let board = match self.pm_board() {
            Ok(text) => text,
            Err(why) => {
                self.pm.due = wakes;
                self.pm.lost = lost;
                return Err(why);
            }
        };
        let text = prompt::wake(
            self.project.as_str(),
            &wakes,
            &told,
            &lost,
            full,
            &notes,
            &board,
        );
        let call = match self.pm_call(agent, session, text, Some(board)) {
            Ok(call) => call,
            Err(why) => {
                self.pm.due = wakes;
                self.pm.lost = lost;
                return Err(why);
            }
        };
        self.pm.flying = Some(Flying {
            kind: Kind::Wake,
            wakes,
            read_to: self.state.last_event,
            told,
            lost,
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
        let call = self.pm_call(agent, Session::Resume(id.clone()), COMPACT.to_owned(), None)?;
        self.pm.flying = Some(Flying {
            kind: Kind::Compact,
            wakes: Vec::new(),
            read_to: self.state.last_event,
            told: Vec::new(),
            lost: Vec::new(),
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
                // What it carried had already waited its window, so it waits
                // none again: the fresh session starts on this pass, and a
                // wake held from here is not read as this failure.
                let now = self.ports.clock.now();
                self.pm.holding_since = Some(Timestamp(now.0.saturating_sub(HOLD_FOR)));
                self.pm.lost.splice(0..0, flying.lost);
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
        let Some(mut answer) = answer::read(&reply.text) else {
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
        next.pm_told
            .drain(..flying.told.len().min(next.pm_told.len()));
        self.save(next)?;
        let woke_for: Vec<String> = flying.wakes.iter().map(prompt::reason).collect();
        let picking = flying.wakes.iter().find_map(|w| match w {
            Wake::Pick(_, free) => Some((*free).max(1)),
            _ => None,
        });
        // The standing notes and each tell this wake carried are written before
        // the answer acts, so an action that cannot be saved cannot lose a rule
        // the maintainer gave. A saved `standing` was told to carry its own
        // tells, and one kelpie did not save carried them nowhere.
        let stood = answer.standing.as_deref().map(|text| self.pm_stand(text));
        if !matches!(stood, Some(Ok(_))) {
            self.pm_stand_tells(&flying.told);
        }
        let (mut acted, mut dropped) = self.act(&mut answer, picking)?;
        match stood {
            Some(Ok(done)) => acted.push(done),
            Some(Err(why)) => dropped.push(why),
            None => {}
        }
        // Its answers to the rulings it was asked about; one it gave none on
        // goes to the maintainer. They are part of what kelpie did, so they
        // are applied before the note below names the wake's actions.
        let (asked, requested) = requests::split(&flying.wakes);
        if !asked.is_empty() {
            let answers = answer
                .votes
                .iter()
                .map(|(id, fields)| {
                    let options = self.ruling_options(*id);
                    (*id, crate::pm_answer::from_fields(fields, options))
                })
                .collect();
            acted.extend(self.pm_answered(&asked, answers));
        }
        if !requested.is_empty() {
            acted.extend(self.pm_requested(&requested, &answer.votes));
        }
        // One line of what the wake was for, what kelpie did with it, and the
        // answer's own `why`, so a wake that wrote nothing of its own left its
        // record behind. A wake that failed or gave no readable answer wrote
        // nothing above, and adds nothing here.
        self.pm_note_wake(&woke_for, &acted, answer.why.as_deref());
        // A ruling it did not settle goes to the maintainer as it stands.
        for wake in &flying.wakes {
            if let Wake::Question { id, .. } | Wake::FollowUp { id, .. } = wake {
                self.pm.asking.remove(id);
            }
        }
        // Its pick or hold is taken on the board's next read.
        self.looks.board_moved();
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
        self.pm.lost.splice(0..0, flying.lost);
        for wake in flying.wakes {
            self.pm_due(wake);
        }
        self.give_up();
        StepReport::PmFailed { reason }
    }
}
