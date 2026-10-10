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
use crate::board::briefing::{
    self, Briefing, Files, IDLE_AFTER, Item, Merge, Ready, ReadyLine, RulingLine, Session, age,
    quiet_for,
};
use crate::board::{ReadyIssue, Skip, priority, rule_order};
use crate::local_paths::Surface;
use crate::ports::{AgentCall, Timestamp};
use crate::state::{ProjectState, StateError, write_atomically};
use crate::work_item::{Phase, ReviewStage, Seat, Turn, WorkItem};

mod events;
mod git;

#[cfg(test)]
mod tests;

use git::{Git, GitAnswers, GitJob};

/// How many events a board shows before the project manager first reads it
const RECENT: usize = 20;

/// How often the board is written again with nothing changed, in seconds
const REFRESH: u64 = 60;

/// What the board keeps between writes, in memory only
#[derive(Debug, Default)]
pub(super) struct BoardCache {
    // The ready queue as last read, and when
    ready: Option<(Timestamp, Vec<ReadyIssue>)>,
    // The bodies of the issues ready or open, whose named paths git reads
    bodies: Vec<(u64, String)>,
    // The paths each issue's body names, as git last matched them
    named: BTreeMap<u64, Vec<String>>,
    // The bodies `named` was read from
    named_from: BTreeMap<u64, String>,
    // The files each open work item's branch changes, as git last said
    files: BTreeMap<u64, Vec<String>>,
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
    fn record(&mut self, found: Vec<String>) -> Result<(), StateError> {
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
            let had = |n: u64| before.iter().any(|i| i.number == n);
            let has = |n: u64| ready.iter().any(|i| i.number == n);
            for issue in ready.iter().filter(|i| !had(i.number)) {
                found.push(format!("#{} joined the ready queue", issue.number));
            }
            for issue in before.iter().filter(|i| !has(i.number)) {
                found.push(format!("#{} left the ready queue", issue.number));
            }
        }
        let open = self.state.open_issues();
        self.brief.bodies.retain(|(n, _)| open.contains(n));
        let fresh = ready.iter().filter(|i| !open.contains(&i.number));
        (self.brief.bodies).extend(fresh.map(|i| (i.number, i.body.clone())));
        self.brief.ready = Some((now, ready.to_vec()));
        self.brief.due = true;
        self.record(found)
    }

    /// The ready issues the board shows, as last read, none of them open
    pub(super) fn board_ready(&self) -> Vec<u64> {
        let Some((_, ready)) = &self.brief.ready else {
            return Vec::new();
        };
        (ready.iter())
            .filter(|i| self.state.item(i.number).is_none())
            .map(|i| i.number)
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
            git: std::mem::take(&mut self.brief.git),
            branches: (self.state.work_items.iter())
                .map(|item| (item.issue, item.branch.clone()))
                .collect(),
            bodies: self.brief.bodies.clone(),
        })
    }

    /// Marks the board due to be written at the end of this pass
    pub(super) fn board_changed(&mut self) {
        self.brief.due = true;
    }

    fn watch_idle(&mut self, now: Timestamp) -> Vec<String> {
        let agents = std::sync::Arc::clone(&self.ports.agents);
        let mut found = Vec::new();
        let mut stuck = Vec::new();
        for (issue, watched) in self.flights.watched_mut() {
            let Some(call) = &watched.call else { continue };
            let Some(quiet) = quiet_for(watched.started, agents.last_active(call), now) else {
                continue;
            };
            let idle = quiet >= IDLE_AFTER;
            if idle && !watched.idle {
                let quiet = age(now, Timestamp(now.0.saturating_sub(quiet)));
                found.push(format!(
                    "#{issue}: worker idle, no tool call or output for {quiet}"
                ));
                stuck.push((issue, quiet));
            } else if !idle && watched.idle {
                found.push(format!("#{issue}: worker active again"));
            }
            watched.idle = idle;
        }
        for (issue, quiet) in stuck {
            let what = format!("its worker has shown no tool call or output for {quiet}");
            self.pm_due(super::pm::Wake::Stuck(issue, what));
        }
        found
    }

    fn write_board(&mut self, mut answers: GitAnswers) {
        let now = self.ports.clock.now();
        self.brief.git = std::mem::take(&mut answers.git);
        self.brief.named = std::mem::take(&mut answers.named);
        self.brief.named_from = std::mem::take(&mut answers.bodies).into_iter().collect();
        // A branch git could not read this time keeps the files last read.
        let open = self.state.open_issues();
        self.brief.files.retain(|issue, _| open.contains(issue));
        self.brief.files.extend(std::mem::take(&mut answers.files));
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
            .map(|item| self.item_line(item, self.item_files(item.issue), now))
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
            ready: self.ready_lines(),
            events,
            woken_before: self.state.pm_seen.is_some(),
            dropped,
            conflicts: answers.merges,
        }
    }

    /// The files the work item for `issue` touches, as the board last read
    /// them: its branch's diff, or the paths its issue names while that is empty
    pub(super) fn item_files(&self, issue: u64) -> Files {
        let diff = self.brief.files.get(&issue);
        let named = self.brief.named.get(&issue);
        match (diff, named) {
            (Some(diff), Some(named)) if diff.is_empty() && !named.is_empty() => {
                Files::Named(named.clone())
            }
            (Some(diff), _) => Files::Diff(diff.clone()),
            (None, Some(named)) => Files::Named(named.clone()),
            (None, None) => Files::Unknown,
        }
    }

    /// The paths `issue`'s body names, once the board has read them from
    /// the body as it was last read from the forge
    pub(super) fn brief_named(&self, issue: u64) -> Option<&[String]> {
        let body = self.brief.bodies.iter().find(|(n, _)| *n == issue);
        let read = self.brief.named_from.get(&issue);
        match (body, read) {
            (Some((_, body)), Some(read)) if body == read => {}
            _ => return None,
        }
        self.brief.named.get(&issue).map(Vec::as_slice)
    }

    fn item_line(&self, item: &WorkItem, files: Files, now: Timestamp) -> Item {
        let session = match self.flights.watched(item.issue) {
            Some(Watched {
                started,
                call: Some(call),
                ..
            }) => Session::Turn {
                since: *started,
                last: self.ports.agents.last_active(call),
            },
            Some(Watched { started, .. }) => Session::Review { since: *started },
            None => Session::Still(still(&item.turn, now, &|text| self.shown(text))),
        };
        Item {
            waiting: self.model_wait(item.issue).map(|w| (w.since, w.why)),
            issue: item.issue,
            title: events::quote(&item.title),
            phase: phase_text(&item.phase, now),
            pull_request: item.pull_request,
            opened: item.timings.as_ref().map(|t| t.created),
            agent: item.agent.to_string(),
            branch: item.branch.clone(),
            session,
            summary: item.summary.clone().map(|s| self.shown(s)),
            files,
        }
    }

    fn ready_lines(&self) -> Option<Ready> {
        let (read, ready) = self.brief.ready.as_ref()?;
        let mut ready: Vec<ReadyIssue> = (ready.iter())
            .filter(|i| self.state.item(i.number).is_none())
            .cloned()
            .collect();
        rule_order(&mut ready);
        let issues = ready
            .into_iter()
            .map(|issue| {
                let skip = self.skipped.iter().find(|s| s.issue() == issue.number);
                let (mut waits, quoted) = skip.map_or((None, true), |s| {
                    let (text, quoted) = passed_over(s, &|text| self.shown(text));
                    (Some(text), quoted)
                });
                if waits.is_none() && self.pm_held().contains(&issue.number) {
                    waits = Some("held by you until an open work item closes".to_owned());
                }
                let named = self.brief.named.get(&issue.number);
                ReadyLine {
                    number: issue.number,
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

// Why the board passes over an issue, and whether the board may still take
// it later, so its body is worth quoting.
fn passed_over(skip: &Skip, shown: &dyn Fn(String) -> String) -> (String, bool) {
    match skip {
        Skip::PullRequest { pull_request, .. } => {
            (format!("PR #{pull_request} is open for it"), false)
        }
        Skip::Finished { .. } => (
            "kelpie finished it, and the forge has not closed it".into(),
            false,
        ),
        Skip::Assigned { .. } => ("someone is assigned to it".into(), false),
        Skip::Split { open, .. } => (
            format!("its {open} open sub-issues are worked instead"),
            false,
        ),
        Skip::Blocked { by, unlisted, .. } => {
            let mut named: Vec<String> = by.iter().map(|n| format!("#{n}")).collect();
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
        Skip::Unlabelled { ruling: None, .. } => (
            "waits for the issue writer to pick its implementer".into(),
            true,
        ),
        Skip::Unlabelled {
            ruling: Some(id), ..
        } => (
            format!("has no `agent:` label, and waits on ruling {id}"),
            true,
        ),
        Skip::Rework { error, .. } | Skip::Adopt { error, .. } => {
            (shown(events::quote(error)), true)
        }
    }
}

// Where a work item with no call in flight stands, in words
fn still(turn: &Turn, now: Timestamp, shown: &dyn Fn(String) -> String) -> String {
    match turn {
        Turn::Due => "no call in flight; its first turn is due".into(),
        Turn::Running { since } => format!(
            "no call in flight; a turn saved as running {} ago resumes on the next pass",
            age(now, *since)
        ),
        Turn::Next { .. } => "no call in flight; its next turn is due".into(),
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
