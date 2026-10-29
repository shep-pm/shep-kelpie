//! Writing the board briefing, `board.md`, for the project manager's agent
//!
//! A save that changes the board records its events in the state file and
//! marks the board due, and the end of each pass writes it, atomically. So
//! does the runner's start, each read of the ready queue, a call starting,
//! and a refresh every [`REFRESH`] seconds, which keeps the ages and each
//! session's activity current. Git runs with the runner's lock let go, and
//! nothing here calls a model: the activity is a file's modified time.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::Runner;
use super::trigger::lock;
use super::turn::held_up::HeldUp;
use crate::agents::{Fit, Role, Tier};
use crate::board::briefing::{
    self, AgentLine, Briefing, Files, IDLE_AFTER, Item, Merge, Ready, ReadyLine, RedLine,
    RulingLine, Session, age, elapsed, quiet_for,
};
use crate::board::{ReadyIssue, Skip, priority, rule_order};
use crate::local_paths::Surface;
use crate::ports::{AgentCall, PaddockHolder, Timestamp, Wait};
use crate::settings::{AgentName, GatewayName, ModelHost};
use crate::state::{ProjectState, StateError, write_atomically};
use crate::ticket::TicketId;
use crate::work_item::{Phase, ReviewStage, Seat, Turn, WorkItem};

mod events;
pub(in crate::runner) mod git;
pub(in crate::runner) mod patch;

#[cfg(test)]
mod tests;

use git::{Git, GitAnswers, GitJob};

/// How many events a board shows before the project manager first reads it
const RECENT: usize = 20;

/// How often the board is written again with nothing changed, in seconds
const REFRESH: u64 = 60;

/// How long a call in flight waits for its model before the project manager
/// is woken for it, in seconds
///
/// The board calls a quiet turn idle at the same length. A wait is not the
/// worker being quiet, but a long one is worth a word of its own.
const LEASE_WAIT_AFTER: u64 = IDLE_AFTER;

/// What the board keeps between writes, in memory only
#[derive(Debug, Default)]
pub(super) struct BoardCache {
    // The ready queue as last read, and when
    ready: Option<(Timestamp, Vec<ReadyIssue>)>,
    // The bodies of the issues ready or open, whose named paths git reads
    bodies: Vec<(TicketId, String)>,
    // The paths each issue's body names, as git last matched them
    named: BTreeMap<TicketId, Vec<String>>,
    // The bodies `named` was read from
    named_from: BTreeMap<TicketId, String>,
    // The files each open work item's branch changes, as git last said
    files: BTreeMap<TicketId, Vec<String>>,
    // Each open branch's diff as git last gave it, or why not, for the PM
    patches: BTreeMap<TicketId, Result<Vec<u8>, String>>,
    git: Git,
    due: bool,
    written: Option<Timestamp>,
}

/// A call in flight, as the board watches it
#[derive(Debug)]
pub(super) struct Watched {
    /// When it started
    pub(super) started: Timestamp,
    /// A worker's turn, whose activity the board reads; none for a review call
    pub(super) call: Option<AgentCall>,
    /// Whether the board has said it is idle
    pub(super) idle: bool,
    /// What the call last said it waits for before its model can answer, and
    /// since when. `None` once it has the lease it waited for.
    pub(super) told: Option<(Timestamp, Wait)>,
    /// When the call got its model: the grant of the lease it waited for, or
    /// the start a carried-back call brought with it. `None` while it waits
    /// for a lease, and for a live call whose harness never says it has it.
    pub(super) granted: Option<Timestamp>,
    /// Whether this wait has already woken the project manager
    pub(super) woken: bool,
    /// The gateway and the model the call holds a lease on, `None` when its
    /// harness or round holds none
    pub(super) lease: Option<(GatewayName, String)>,
}

impl Watched {
    /// When the call's quiet counts from: the grant of the lease it waited
    /// for, or its start when it waited for none
    ///
    /// `None` while the call still waits for its model, when nothing it shows
    /// is late: the quiet is the gateway's turn, not the worker's.
    pub(super) fn quiet_from(&self) -> Option<Timestamp> {
        match self.told {
            Some(_) => None,
            None => Some(self.granted.unwrap_or(self.started)),
        }
    }
}

// Whether `after` holds an entry `before` did not, or holds one differently
fn learned<T: PartialEq>(before: &BTreeMap<TicketId, T>, after: &BTreeMap<TicketId, T>) -> bool {
    (after.iter()).any(|(ticket, now)| before.get(ticket) != Some(now))
}

/// Writes `runner`'s board if it is due, asking git with the lock let go
pub(super) fn brief(runner: &Mutex<Runner>) {
    let job = lock(runner).board_job();
    if let Some(job) = job {
        let answers = job.run();
        lock(runner).write_board(answers);
    }
}

impl Runner {
    // Records the events between the saved state and `next` in `next`.
    pub(super) fn note_changes(&mut self, next: &mut ProjectState) {
        let now = self.ports.clock.now();
        let found = events::changes(&self.state, next, &|text| self.shown(text));
        if !found.is_empty() {
            self.brief.due = true;
        }
        self.pm_noticed(next);
        for what in found {
            next.record_event(now, what);
        }
    }

    // Saves `found` as board events.
    pub(in crate::runner) fn record(&mut self, found: Vec<String>) -> Result<(), StateError> {
        if found.is_empty() {
            return Ok(());
        }
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        for what in found {
            next.record_event(now, what);
        }
        self.brief.due = true;
        self.save(next)
    }

    /// Takes the ready queue just read, and records the issues it gained or lost
    ///
    /// # Errors
    ///
    /// [`StateError::Write`] when the events cannot be saved.
    pub(super) fn ready_read(&mut self, ready: &[ReadyIssue]) -> Result<(), StateError> {
        let now = self.ports.clock.now();
        let mut found = Vec::new();
        if let Some((_, before)) = &self.brief.ready {
            let had = |t: TicketId| before.iter().any(|i| i.ticket == t);
            let has = |t: TicketId| ready.iter().any(|i| i.ticket == t);
            for issue in ready.iter().filter(|i| !had(i.ticket)) {
                found.push(format!("#{} joined the ready queue", issue.ticket));
            }
            for issue in before.iter().filter(|i| !has(i.ticket)) {
                found.push(format!("#{} left the ready queue", issue.ticket));
            }
        }
        let open = self.state.open_tickets();
        self.brief
            .bodies
            .retain(|(ticket, _)| open.contains(ticket));
        let fresh = ready.iter().filter(|i| !open.contains(&i.ticket));
        (self.brief.bodies).extend(fresh.map(|i| (i.ticket, i.body.clone())));
        self.brief.ready = Some((now, ready.to_vec()));
        self.brief.due = true;
        self.record(found)
    }

    /// The ready issues the board shows, as last read, none of them open
    pub(super) fn board_ready(&self) -> Vec<TicketId> {
        let Some((_, ready)) = &self.brief.ready else {
            return Vec::new();
        };
        (ready.iter())
            .filter(|i| self.state.item(i.ticket).is_none())
            .map(|i| i.ticket)
            .collect()
    }

    /// Writes the board now if it is due, git and all, as the runner's
    /// start does before any lock exists
    pub(super) fn brief_now(&mut self) {
        if let Some(job) = self.board_job() {
            let answers = job.run();
            self.write_board(answers);
        }
    }

    // Says which calls in flight went idle or came back, then hands out
    // what git must answer if the board is due: changed, or REFRESH old.
    fn board_job(&mut self) -> Option<GitJob> {
        let now = self.ports.clock.now();
        let found = self.watch_idle(now);
        if let Err(e) = self.record(found) {
            eprintln!("cannot save the board's events: {e}");
        }
        let stale = (self.brief.written).is_none_or(|at| now.0.saturating_sub(at.0) >= REFRESH);
        if !self.brief.due && !stale {
            return None;
        }
        // A change saved while git runs marks the board due again.
        self.brief.due = false;
        Some(GitJob {
            repo: self.settings.git.checkout.clone(),
            base: self.settings.git.base().to_owned(),
            git: std::mem::take(&mut self.brief.git),
            branches: (self.state.work_items.iter())
                .map(|item| (item.issue, item.branch.clone()))
                .collect(),
            bodies: self.brief.bodies.clone(),
            patches: self.agents.pm.is_some().then(|| {
                (self.state.work_items.iter())
                    .map(|item| (item.issue, self.paths.worktree(item.issue)))
                    .collect()
            }),
        })
    }

    /// Marks the board due to be written at the end of this pass
    pub(super) fn board_changed(&mut self) {
        self.brief.due = true;
    }

    fn watch_idle(&mut self, now: Timestamp) -> Vec<String> {
        let agents = std::sync::Arc::clone(&self.ports.agents);
        let mut found = Vec::new();
        let mut stuck: Vec<(TicketId, String, Timestamp)> = Vec::new();
        let mut waiting: Vec<TicketId> = Vec::new();
        for (ticket, watched) in self.flights.watched_mut() {
            // A call that has not been granted its model's lease is never idle:
            // what it has shown nothing for is the gateway's turn, not the
            // worker's. A long wait is a wake of its own.
            if let Some((since, why)) = watched.told {
                watched.idle = false;
                let waited = now.0.saturating_sub(since.0);
                if !watched.woken && waited >= LEASE_WAIT_AFTER {
                    watched.woken = true;
                    let who = if watched.call.is_some() {
                        "worker"
                    } else {
                        "review call"
                    };
                    // The board names what the call waits for, as the wake
                    // does. A gateway that answered busy has no lease in the
                    // way, and a line that says its model's lease is waiting
                    // points the reader at a holder to release.
                    let for_what = match why {
                        Wait::Lease => "its model's lease",
                        _ => why.as_str(),
                    };
                    found.push(format!(
                        "#{ticket}: {who} waits {} for {for_what}",
                        elapsed(waited)
                    ));
                    waiting.push(ticket);
                }
                continue;
            }
            let Some(call) = &watched.call else { continue };
            // Its quiet runs from the lease it was granted, or from its start
            // for a harness that waits on none.
            let from = watched.granted.unwrap_or(watched.started);
            let Some(quiet) = quiet_for(from, agents.last_active(call), now) else {
                continue;
            };
            let idle = quiet >= IDLE_AFTER;
            if idle && !watched.idle {
                let quiet = age(now, Timestamp(now.0.saturating_sub(quiet)));
                found.push(format!(
                    "#{ticket}: worker idle, no tool call or output for {quiet}"
                ));
                stuck.push((ticket, quiet, watched.started));
            } else if !idle && watched.idle {
                found.push(format!("#{ticket}: worker active again"));
            }
            watched.idle = idle;
        }
        for (ticket, quiet, started) in stuck {
            let what = format!("its worker has shown no tool call or output for {quiet}");
            self.pm_due(super::pm::Wake::Stuck {
                ticket,
                what,
                state: super::pm::StuckState::Idle(started),
            });
        }
        for ticket in waiting {
            if let Some(wake) = self.lease_wake(ticket) {
                self.pm_due(wake);
            }
        }
        found
    }

    /// `ticket`'s wake for a call in flight that has waited past
    /// [`LEASE_WAIT_AFTER`] for its model, naming who holds the lease when the
    /// wait is one for a lease and the gateway will say
    ///
    /// `None` when it waits on nothing or has not waited that long.
    pub(super) fn lease_wake(&self, ticket: TicketId) -> Option<super::pm::Wake> {
        let watched = self.flights.watched(ticket)?;
        let (since, why) = watched.told?;
        let now = self.ports.clock.now();
        let waited = now.0.saturating_sub(since.0);
        if waited < LEASE_WAIT_AFTER {
            return None;
        }
        Some(super::pm::Wake::LeaseWait {
            ticket,
            worker: watched.call.is_some(),
            why,
            waited: elapsed(waited),
            // Only a wait for a lease has a holder to name. `Wait::Busy` is the
            // gateway turned away for another reason, such as a load, and
            // naming a client for it blames one that is not in the way.
            holder: match why {
                Wait::Lease => watched
                    .lease
                    .as_ref()
                    .and_then(|lease| self.lease_holder(lease))
                    .map(|held| self.shown_holder(held)),
                _ => None,
            },
        })
    }

    // `holder`, with each word the gateway gave it checked as the board checks
    // what it shows
    fn shown_holder(&self, holder: PaddockHolder) -> PaddockHolder {
        PaddockHolder {
            lease: self.shown(holder.lease),
            client: self.shown(holder.client),
            what: self.shown(holder.what),
            since: holder.since,
        }
    }

    // Who holds the gateway's lease on the model `lease` names, when the
    // gateway will say
    fn lease_holder(&self, lease: &(GatewayName, String)) -> Option<PaddockHolder> {
        let upstream = self
            .gateways
            .upstream(&ModelHost::Gateway(lease.0.clone()))
            .ok()?;
        self.ports
            .local_leases
            .paddock_holder(&upstream, lease.1.as_str())
    }

    // Saves `what` as a board event, saying so when it cannot be saved
    pub(super) fn board_event(&mut self, what: String) {
        if let Err(e) = self.record(vec![what]) {
            eprintln!("cannot save the board's events: {e}");
        }
    }

    fn write_board(&mut self, mut answers: GitAnswers) {
        let now = self.ports.clock.now();
        let compared = (self.brief.named_from.clone(), self.brief.files.clone());
        self.brief.git = std::mem::take(&mut answers.git);
        self.brief.named = std::mem::take(&mut answers.named);
        self.brief.named_from = std::mem::take(&mut answers.bodies).into_iter().collect();
        // A branch git could not read this time keeps the files last read.
        let open = self.state.open_tickets();
        self.brief.files.retain(|ticket, _| open.contains(ticket));
        self.brief.files.extend(std::mem::take(&mut answers.files));
        // A diff git could not give is not kept, since a stale one would mislead.
        self.brief.patches = std::mem::take(&mut answers.patches);
        // Paths the board could not compare before may let it dispatch now.
        let (named_from, files) = &compared;
        if learned(named_from, &self.brief.named_from) || learned(files, &self.brief.files) {
            self.looks.board_moved();
        }
        let conflicts = (answers.merges.iter())
            .filter_map(|(&pair, merge)| match merge {
                Merge::Conflicts(files) => Some((pair, files.clone())),
                Merge::Unknown => None,
            })
            .collect();
        self.pm_conflicts(conflicts);
        let text = briefing::render(&self.briefing(now, answers));
        let path = &self.paths.board;
        let made = path.parent().map_or(Ok(()), std::fs::create_dir_all);
        match made.and_then(|()| write_atomically(path, text.as_bytes())) {
            Ok(()) => self.brief.written = Some(now),
            Err(e) => {
                self.brief.due = true;
                eprintln!("cannot write {}: {}", path.display(), e.kind());
            }
        }
    }

    // `text`, or a note in its place when it names this machine, as
    // nothing kelpie posts may
    pub(super) fn shown(&self, text: String) -> String {
        match self.local.find(&text, Surface::Prose) {
            Some(leak) => format!("(withheld: it names {leak})"),
            None => text,
        }
    }

    fn briefing(&self, now: Timestamp, answers: GitAnswers) -> Briefing<'_> {
        let items = (self.state.work_items.iter())
            .map(|item| self.item_line(item, item.issue, self.item_files(item.issue), now))
            .collect();
        let (events, dropped) = self.state.unread_events(RECENT);
        let events = events.to_vec();
        Briefing {
            project: self.project.as_str(),
            now,
            active_items: self.settings.concurrency.active_items.get(),
            pending_rulings: self.settings.concurrency.pending_rulings,
            held: self
                .issues_where(|i| !i.parked() && i.seat == Seat::Held)
                .len(),
            parked: self.parked_count(),
            run: self.run_status(),
            main: answers.main,
            items,
            rulings: (self.state.rulings.iter())
                .map(|r| RulingLine {
                    id: r.id,
                    issue: r.issue,
                    pull_request: r.pull_request,
                    question: self.shown(events::quote(&r.question)),
                })
                .collect(),
            red: (self.red_status(now).into_iter())
                .map(|red| RedLine {
                    workflow: red.workflow,
                    since: red.since,
                    head: red.head,
                    url: red.url,
                    doing: red.doing,
                })
                .collect(),
            ready: self.ready_lines(),
            agents: self.agent_lines(),
            events,
            woken_before: self.state.pm_seen.is_some(),
            dropped,
            behind: answers.behind,
            conflicts: answers.merges,
        }
    }

    /// The files the work item for `ticket` touches, as the board last read
    /// them: its branch's diff, or the paths its issue names while that is empty
    pub(super) fn item_files(&self, ticket: TicketId) -> Files {
        let diff = self.brief.files.get(&ticket);
        let named = self.brief.named.get(&ticket);
        match (diff, named) {
            (Some(diff), Some(named)) if diff.is_empty() && !named.is_empty() => {
                Files::Named(named.clone())
            }
            (Some(diff), _) => Files::Diff(diff.clone()),
            (None, Some(named)) => Files::Named(named.clone()),
            (None, None) => Files::Unknown,
        }
    }

    /// The work item for `ticket`'s diff from `main` as the board's last
    /// write read it, cut to size, or why git could not give it; `None`
    /// before a write has read it
    pub(super) fn brief_patch(&self, ticket: TicketId) -> Option<&Result<Vec<u8>, String>> {
        self.brief.patches.get(&ticket)
    }

    /// The paths `ticket`'s issue body names, once the board has read them
    /// from the body as it was last read from the forge
    pub(super) fn brief_named(&self, ticket: TicketId) -> Option<&[String]> {
        let body = self.brief.bodies.iter().find(|(t, _)| *t == ticket);
        let read = self.brief.named_from.get(&ticket);
        match (body, read) {
            (Some((_, body)), Some(read)) if body == read => {}
            _ => return None,
        }
        self.brief.named.get(&ticket).map(Vec::as_slice)
    }

    fn item_line(&self, item: &WorkItem, ticket: TicketId, files: Files, now: Timestamp) -> Item {
        let session = match self.flights.watched(ticket) {
            Some(watched) => match &watched.call {
                Some(call) => Session::Turn {
                    since: watched.started,
                    active: watched.quiet_from(),
                    last: self.ports.agents.last_active(call),
                },
                None => Session::Review {
                    since: watched.started,
                    waiting: watched.told,
                },
            },
            None => {
                let held = self.held_up.of(ticket);
                Session::Still(still(&item.turn, now, held, &|text| self.shown(text)))
            }
        };
        Item {
            waiting: self.model_wait(ticket).map(|w| (w.since, w.why)),
            ticket,
            title: events::quote(&item.title),
            phase: phase_text(&item.phase, now),
            pull_request: item.pull_request,
            opened: item.timings.as_ref().map(|t| t.created),
            agent: item.agent.to_string(),
            branch: item.branch.clone(),
            session,
            summary: item.summary.clone().map(|s| self.shown(s)),
            spent: item.priced().total(),
            cap: item.budget_cap(self.settings.budget.per_item_usd),
            files,
        }
    }

    /// The digest of `ticket`'s text as the board last read it, if it is ready
    pub(super) fn ready_text(&self, ticket: TicketId) -> Option<String> {
        let (_, ready) = self.brief.ready.as_ref()?;
        Some(ready.iter().find(|i| i.ticket == ticket)?.fingerprint())
    }

    /// The ready issues as last read, none of them open, in the order the
    /// board takes them
    pub(super) fn ready_in_order(&self) -> Vec<ReadyIssue> {
        let Some((_, ready)) = &self.brief.ready else {
            return Vec::new();
        };
        let mut ready: Vec<ReadyIssue> = (ready.iter())
            .filter(|i| self.state.item(i.ticket).is_none())
            .cloned()
            .collect();
        rule_order(&mut ready, &self.state.order);
        ready
    }

    // The implementers the agent board may pick, by name, then the project's
    // reviewers, each with the keys its file holds now.
    fn agent_lines(&self) -> Vec<AgentLine> {
        let line = |name: &AgentName, role: Role| {
            let fit = self.book.get(name).map(|agent| &agent.fit);
            AgentLine {
                name: name.to_string(),
                role,
                tier: fit.map_or_else(Tier::default, Fit::tier),
                kinds: fit
                    .and_then(|fit| fit.kinds.as_ref())
                    .map(|kinds| kinds.iter().map(str::to_owned).collect()),
                description: fit
                    .and_then(|fit| fit.description.as_ref())
                    .map(|text| self.shown(one_line(text.as_str()))),
            }
        };
        let implementers = (self.board_agents().into_iter())
            .map(|name| line(&name, Role::Implementer))
            .collect::<Vec<_>>();
        let reviewers = (self.lineup.iter()).map(|r| line(&r.name, Role::Reviewer));
        implementers.into_iter().chain(reviewers).collect()
    }

    fn ready_lines(&self) -> Option<Ready> {
        let (read, _) = self.brief.ready.as_ref()?;
        let issues = self
            .ready_in_order()
            .into_iter()
            .map(|issue| {
                let skip = self
                    .skipped
                    .iter()
                    .find(|s| s.issue() == Some(issue.ticket));
                let (mut waits, quoted) = skip.map_or((None, true), |s| {
                    let (text, quoted) = passed_over(s, &|text| self.shown(text));
                    (Some(text), quoted)
                });
                let held = self.pm_held().iter().find(|h| h.issue == issue.ticket);
                if let Some(held) = held.filter(|_| waits.is_none()) {
                    waits = Some(format!("held by you {}", held.waits()));
                }
                let named = self.brief.named.get(&issue.ticket);
                ReadyLine {
                    ticket: issue.ticket,
                    priority: priority(&issue.labels),
                    waits,
                    named: named.cloned().unwrap_or_default(),
                    excerpt: quoted.then(|| briefing::excerpt(&issue.body)),
                    title: events::quote(&issue.title),
                }
            })
            .collect();
        Some(Ready {
            read: *read,
            issues,
        })
    }
}

// `text` on one line, its runs of whitespace closed up
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// Why the board passes over an issue, and whether the board may still take
// it later, so its body is worth quoting.
pub(super) fn passed_over(skip: &Skip, shown: &dyn Fn(String) -> String) -> (String, bool) {
    match skip {
        Skip::PullRequest { pull_request, .. } => {
            (format!("PR #{pull_request} is open for it"), false)
        }
        Skip::Finished { .. } => (
            "kelpie finished it, and the forge has not closed it".into(),
            false,
        ),
        Skip::Dropped { issue } => (
            format!(
                "stopped with no merged pull request, and an edit to the issue, a label put back or `shep kelpie add {issue}` takes it back"
            ),
            false,
        ),
        Skip::Assigned { .. } => ("someone is assigned to it".into(), false),
        Skip::InProgress { .. } => (
            "an earlier worker holds it, take `in-progress` off to release it".into(),
            false,
        ),
        Skip::Split { open, .. } => (
            format!("its {open} open sub-issues are worked instead"),
            false,
        ),
        Skip::Blocked { by, unlisted, .. } => {
            let mut named: Vec<String> = by.iter().map(|t| briefing::ticket_ref(*t)).collect();
            if *unlisted > 0 {
                named.push(format!("{unlisted} more"));
            }
            (format!("blocked by {}", named.join(", ")), true)
        }
        Skip::Label { error, .. } => (shown(error.to_string()), true),
        Skip::Failed { error, .. } => (
            format!("cannot be read: {}", shown(events::quote(error))),
            true,
        ),
        Skip::PathsUnread { unknown: None, .. } => (
            "waits a pass for its paths to be read against parked branches".into(),
            true,
        ),
        Skip::PathsUnread {
            unknown: Some(with),
            ..
        } => (
            format!("waits for #{with}'s branch, parked on a ruling, to be read"),
            true,
        ),
        Skip::Overlap { with, files, .. } => (
            format!(
                "shares {} with #{with}, parked on a ruling",
                shown(files.join(", "))
            ),
            true,
        ),
        Skip::Check { failures, .. } => (
            format!("the issue check refused it: {}", shown(failures.join("; "))),
            true,
        ),
        Skip::Paced { hold, .. } => (format!("held by the pacer: {hold}"), true),
        Skip::Unpicked { why, .. } => (why.clone(), true),
        Skip::PmWaits { .. } => ("the project manager's pick waits".into(), true),
        Skip::Rework { error, .. } | Skip::Adopt { error, .. } => {
            (shown(events::quote(error)), true)
        }
    }
}

// Where a work item with no call in flight stands, in words
fn still(
    turn: &Turn,
    now: Timestamp,
    held: Option<HeldUp>,
    shown: &dyn Fn(String) -> String,
) -> String {
    // A due turn that has not started says what it waits on.
    let due = |which: &str| match &held {
        Some(held) => format!(
            "no call in flight; {which} turn has waited {}: {}",
            age(now, held.since),
            held.because
        ),
        None => format!("no call in flight; {which} turn is due"),
    };
    match turn {
        Turn::Due => due("its first"),
        Turn::Running { since, .. } => format!(
            "no call in flight; a turn saved as running {} ago resumes on the next pass",
            age(now, *since)
        ),
        Turn::Next { .. } => due("its next"),
        Turn::Ended { at } => format!(
            "no call in flight; the worker's last turn ended {} ago",
            age(now, *at)
        ),
        Turn::Failed { at, reason } => format!(
            "no call in flight; the worker's last turn failed {} ago: {}",
            age(now, *at),
            shown(events::quote(reason))
        ),
    }
}

// A phase as the board's line for a work item says it
fn phase_text(phase: &Phase, now: Timestamp) -> String {
    match phase {
        Phase::Implement => "implementing".into(),
        Phase::Review(review) => {
            let by = review
                .reviewer
                .as_ref()
                .map_or(String::new(), |r| format!(" by {r}"));
            let stage = match &review.stage {
                ReviewStage::Round => String::new(),
                ReviewStage::Found { .. } => ", findings in".into(),
                ReviewStage::Fixing { .. } => ", the worker fixing its findings".into(),
                ReviewStage::Pushing => ", the worker pushing its work first".into(),
                ReviewStage::SecondLook { .. } => ", a second look".into(),
                ReviewStage::Summon { bot, .. } => format!(", waiting to summon {bot}"),
                ReviewStage::Summoned { bot, at, .. } => {
                    format!(", {bot} summoned {} ago", age(now, *at))
                }
                ReviewStage::Settling { bot, .. } => {
                    format!(", {bot} reviewed, reading its threads")
                }
            };
            format!("review round {}{by}{stage}", review.round)
        }
        Phase::Ci { since, .. } => format!("waiting on CI for {}", age(now, *since)),
        Phase::Ruling { id } => format!("parked on ruling {id}"),
        Phase::Merge { .. } => "merging".into(),
        Phase::Done { merged: true, .. } => "merged, cleaning up".into(),
        Phase::Done { closed: true, .. } => "issue closed with no change, cleaning up".into(),
        Phase::Done { .. } => "ending, cleaning up".into(),
    }
}
