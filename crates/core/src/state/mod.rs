//! The project's state file
//!
//! What a runner must remember across a restart: its open work items,
//! pending rulings and leases held. Whether the project runs is the
//! runner's sheep's own state, which shep keeps. Each save goes
//! to a temporary file that is synced and then renamed over the old one, so
//! a runner killed mid-write leaves the previous state whole.

mod bumps;
mod checks;
mod contributions;
mod events;
mod lease;
mod notices;
mod numbered;
mod projects;
mod red;
mod removed;
mod ruling;
mod triage;

pub use bumps::{Bump, E2eResult, E2eState, Red, RedStep};
pub use checks::{CheckTriage, IssueCheck};
pub use contributions::{
    Adopting, Asked, BotTurn, Ci, CiState, Contribution, Deciding, Held, How, KEPT, Pass, Released,
    Ruled, Said, Security, SecurityVerdict, Spent, Verdict, Walking,
};
pub use events::{BoardEvent, EVENTS_KEPT};
pub use lease::{LeaseHeld, Resource};
pub use notices::{MainNotice, MergeNotice, Notice};
pub use projects::Projects;
pub use red::{Handled, RedWorkflow};
pub use removed::OldWorker;
pub use ruling::{PmAsked, Ruling};
pub use triage::{
    TRIAGE_CALLS_KEPT, TRIAGED_KEPT, TriageAsked, TriageHeld, TriageLocal, TriageQuestions,
    TriageState, TriageStep, Triaged,
};

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pacer::DayStart;
use crate::ports::{Cost, Finding, SessionId, Timestamp};
use crate::settings::Account;
use crate::ticket::TicketId;
use crate::work_item::{Attached, Known, Phase, Priced, Review, Seconds, Tally, Turn, WorkItem};

/// The name of a project's state file, in its folder
pub const STATE_FILE: &str = "state.json";

/// The state file's format version, bumped when the file gains a field or value an older kelpie refuses
pub const VERSION: u32 = 89;

/// The format before a project could have more than one work item open,
/// which this kelpie still reads
const ONE_ITEM: u32 = 1;

/// The format before reviewers were agent files, whose `deep` and `claude`
/// reviewers this kelpie reads as `defect-hunter`
const BUILT_IN_REVIEWERS: u32 = 2;

/// The format before review bots ran in the review pass, whose bot round
/// this kelpie reads into a pass of the listed bots
const BOT_ROUNDS: u32 = 3;

/// The last format before a work item remembered the comments it had read
const BEFORE_TALKED: u32 = 38;

/// The last format before a contribution recorded what it had answered
const BEFORE_ANSWERED: u32 = 57;

/// The format that named rulings by their work item's number, so a file
/// saved before it has its rulings renamed as it loads
const NUMBERED: u32 = 38;

/// How many finished work items the state file keeps a record of
///
/// The file is written whole at every save, a heartbeat included, and a
/// record with its spend is about 1.2 KB, so the records stay under about
/// 120 KB.
pub const HISTORY_CAP: usize = 100;

/// Everything a project's runner keeps across a restart
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectState {
    version: u32,
    /// The open work items, oldest first
    #[serde(default)]
    pub work_items: Vec<WorkItem>,
    /// The work item a version 1 file kept, which loads into `work_items`
    #[serde(default, skip_serializing)]
    work_item: Option<WorkItem>,
    /// Rulings waiting on the maintainer, oldest first
    pub rulings: Vec<Ruling>,
    /// The counter ids this file's rulings were saved under, by the number
    /// each is named by now. Only a file saved before version 38 has any, and
    /// they are not saved: the runner that loaded it reads the answers left
    /// under them once.
    #[serde(skip)]
    renamed: numbered::Renamed,
    /// Tickets whose work items kelpie finished, which the board never takes again
    #[serde(default)]
    pub finished: Vec<TicketId>,
    /// Tickets whose work items ended with no merged pull request, `drop`ped or
    /// stopped by a `no`, which the board leaves be until the forge stops
    /// listing them as ready, so a label put back returns one, or their text
    /// changes
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<TicketId>,
    /// What each ticket in `dropped` read as when the board first saw it after,
    /// as [`crate::board::ReadyIssue::fingerprint`], so an edit returns it. A ticket the board
    /// has not yet seen has none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dropped_text: BTreeMap<TicketId, String>,
    /// The ready tickets in the order the board takes them, first to go
    /// first. Each read of the board updates it. Empty until one has.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<TicketId>,
    /// The priority rank each ticket in `order` had when the board last read it,
    /// so a changed priority label moves it. Empty until a read has.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ranked: crate::board::Ranked,
    /// The last [`HISTORY_CAP`] work items kelpie finished, oldest first
    #[serde(default)]
    pub history: Vec<Finished>,
    /// The forge's ids of the reviews that started a rework or were refused
    /// one. None of them starts another.
    #[serde(default)]
    pub reworked: Vec<String>,
    /// Pull requests adopted and waiting for a free slot, oldest first
    #[serde(default)]
    pub adopted: Vec<Waiting>,
    /// The pull requests from other people the board follows, oldest first
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributions: Vec<Contribution>,
    /// The pull requests the maintainer chose to leave open, which kelpie
    /// follows no further, oldest first. One leaves when its pull request is
    /// no longer open.
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left_open: Vec<u64>,
    /// What the calls on pull requests no longer followed cost, so a
    /// contribution that comes back (a draft marked ready again) keeps its
    /// spend and its cap, oldest first, the last [`KEPT`]
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spent: Vec<Spent>,
    /// The pull requests from listed bots the dependencies setting follows,
    /// oldest first
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bumps: Vec<Bump>,
    /// When `triage.pick` went past `off`, once it has, and the issues
    /// triaged since
    ///
    /// Left out while unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<TriageState>,
    /// What the issue check last said of each open GitHub issue, by number
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<IssueCheck>,
    /// Leases this project holds
    pub leases: Vec<LeaseHeld>,
    /// What the Claude account's week had spent when today began, once
    /// usage has been read
    #[serde(default)]
    pub pacing: Option<DayStart>,
    /// The same for the Codex account, once a Codex agent's usage has been read
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_pacing: Option<DayStart>,
    /// Automatic merges not yet sent, oldest first
    #[serde(default)]
    pub notices: Vec<Notice>,
    /// Where reading the webhook's replies has got to
    #[serde(default)]
    pub replies: Replies,
    /// The board's newest events, oldest first
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<BoardEvent>,
    /// The id of the last board event recorded
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_event: u64,
    /// The last board event the project manager has read, once it has read one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pm_seen: Option<u64>,
    /// The project manager's session, which each wake resumes, once it has one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pm_session: Option<SessionId>,
    /// What the maintainer told the project manager since its last
    /// answered wake, oldest first
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pm_told: Vec<String>,
    /// Whether `pm-archive.md` holds notes the project manager has not read
    /// into `pm-standing.md` yet. A roll sets it, and a `standing` answer
    /// clears it, so a wake that rolled and then failed still owes the ask.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pm_unmined: bool,
    /// The maintainer's terminal holding the project manager's session,
    /// which pauses its wakes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pm_attached: Option<Attached>,
    /// Whether `finish` holds the board back until the open work items
    /// end, when the runner stops its own sheep
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub finishing: bool,
    /// Each pending ruling kelpie's App posted on a thread that mentions
    /// someone, by ruling id: who may answer it there, and how far the
    /// comments on its thread have been read for an answer. A ruling is
    /// entered when its comment is posted, and leaves when it is answered.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub heard: BTreeMap<TicketId, Heard>,
    /// Each pending ruling kelpie's App posted on a thread with no mention,
    /// since no login could be named, by ruling id: the id of its comment.
    /// Nothing is read for an answer there, but the comment is edited when
    /// the ruling is settled.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unmentioned: BTreeMap<TicketId, u64>,
    /// The ids of the last [`POSTED_KEPT`] comments kelpie posted as the
    /// login `gh` acts as, oldest first. A reply in a ruling's thread is
    /// from that login too, so these are never read as answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub posted: Vec<u64>,
    /// The workflows whose latest completed run on `main` failed, in the order
    /// they went red
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub red: Vec<RedWorkflow>,
}

/// How many ids of kelpie's own comments the state file keeps
pub const POSTED_KEPT: usize = 500;

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Default for ProjectState {
    fn default() -> Self {
        Self::new()
    }
}

impl ProjectState {
    /// Keeps the ids of comments kelpie posted, each once, the last
    /// [`POSTED_KEPT`] of them
    pub fn note_posted(&mut self, ids: &[u64]) {
        for id in ids {
            if !self.posted.contains(id) {
                self.posted.push(*id);
            }
        }
        let extra = self.posted.len().saturating_sub(POSTED_KEPT);
        self.posted.drain(..extra);
    }

    /// A project with nothing in flight
    pub fn new() -> Self {
        Self {
            version: VERSION,
            work_items: Vec::new(),
            work_item: None,
            rulings: Vec::new(),
            renamed: numbered::Renamed::default(),
            finished: Vec::new(),
            dropped: Vec::new(),
            dropped_text: BTreeMap::new(),
            order: Vec::new(),
            ranked: BTreeMap::new(),
            history: Vec::new(),
            reworked: Vec::new(),
            adopted: Vec::new(),
            contributions: Vec::new(),
            left_open: Vec::new(),
            spent: Vec::new(),
            bumps: Vec::new(),
            triage: None,
            checks: Vec::new(),
            leases: Vec::new(),
            pacing: None,
            codex_pacing: None,
            notices: Vec::new(),
            replies: Replies::default(),
            events: Vec::new(),
            last_event: 0,
            pm_seen: None,
            pm_session: None,
            pm_told: Vec::new(),
            pm_unmined: false,
            pm_attached: None,
            finishing: false,
            heard: BTreeMap::new(),
            unmentioned: BTreeMap::new(),
            posted: Vec::new(),
            red: Vec::new(),
        }
    }

    /// What `account`'s week had spent when today began
    pub fn day_start(&self, account: Account) -> Option<DayStart> {
        match account {
            Account::Claude => self.pacing,
            Account::Codex => self.codex_pacing,
        }
    }

    /// Where `account`'s day start is kept
    pub fn day_start_mut(&mut self, account: Account) -> &mut Option<DayStart> {
        match account {
            Account::Claude => &mut self.pacing,
            Account::Codex => &mut self.codex_pacing,
        }
    }

    /// The open work items' tickets, oldest first
    pub fn open_tickets(&self) -> Vec<TicketId> {
        self.work_items.iter().map(|item| item.issue).collect()
    }

    /// The open work item for `ticket`
    pub fn item(&self, ticket: TicketId) -> Option<&WorkItem> {
        self.work_items.iter().find(|item| item.issue == ticket)
    }

    /// The open work item for `ticket`, to change
    pub fn item_mut(&mut self, ticket: TicketId) -> Option<&mut WorkItem> {
        self.work_items.iter_mut().find(|item| item.issue == ticket)
    }

    /// Adds `record` to the history, dropping the oldest past [`HISTORY_CAP`]
    pub fn record_finished(&mut self, record: Finished) {
        self.history.push(record);
        let excess = self.history.len().saturating_sub(HISTORY_CAP);
        self.history.drain(..excess);
    }

    // A version 1 file held one work item, and every ruling in it was that
    // item's.
    fn one_item_moved(mut self) -> Self {
        let Some(item) = self.work_item.take() else {
            return self;
        };
        for ruling in &mut self.rulings {
            ruling.issue.get_or_insert(item.issue);
        }
        self.work_items = vec![item];
        self
    }
}

/// A work item kelpie finished, merged or dropped, and where its time went
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finished {
    /// The ticket it resolved
    pub issue: TicketId,
    /// The ticket's title when it was added
    pub title: String,
    /// Its pull request, if it had one
    pub pull_request: Option<u64>,
    /// Whether the pull request merged
    pub merged: bool,
    /// Whether its issue was closed with no change, so it ended with no
    /// pull request
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
    /// When it finished
    pub at: Timestamp,
    /// Seconds from its creation to `at`
    pub wall: u64,
    /// Every phase, summing to `wall`
    pub seconds: Seconds,
    /// What it spent. None in a record saved before version 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<Tally>,
}

/// A pull request adopted and waiting its turn
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Waiting {
    /// The pull request
    pub pull_request: u64,
    /// Whether `ready-for-agent` adopted it, so taking the label off takes it back
    pub by_label: bool,
}

/// Where a ruling is answered in its thread: the one login whose reply
/// answers it, and the last comment read for an answer, which is the
/// ruling's own comment until a reply has been
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heard {
    /// The login the ruling's comment mentions, in the case it was written
    pub mentions: String,
    /// When the comment was written, as the forge says
    pub time: Timestamp,
    /// The forge's id for it
    pub comment: u64,
    /// The forge's id for the ruling's own comment, which a reaction or a
    /// ticked box on it answers, and which `comment` leaves once a reply has
    /// been read. Zero in a file from before, which then reads no reaction.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ruling_comment: u64,
}

impl Heard {
    /// Whether `login` is the one whose reply answers: GitHub reads a login in any case
    pub fn is_mentioned(&self, login: &str) -> bool {
        self.mentions.eq_ignore_ascii_case(login)
    }

    /// Whether a comment written at `time` with this `id` is after this one
    pub fn before(&self, time: Timestamp, id: u64) -> bool {
        (self.time, self.comment) < (time, id)
    }
}

/// Where reading replies on the webhook's topic has got to
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replies {
    /// The last message read, which the next read starts after
    #[serde(default)]
    pub last: Option<LastRead>,
}

/// A message on the webhook's topic, as far as reading it goes
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastRead {
    /// The webhook's id for it
    pub id: String,
    /// When the webhook took it
    pub time: Timestamp,
}
/// What raised a ruling. A no's note, or an answer, always goes to the worker.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RulingKind {
    /// CI is green on this head of a current branch. A yes merges it.
    Merge {
        /// The head the question is about
        head: String,
        /// Why no reviewer read the pull request in its last pass, when none did
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unreviewed: Option<String>,
        /// The listed review bots' threads still open on the head, by bot,
        /// when any are
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open_threads: Option<String>,
        /// The forge's ids of those threads, which a yes accepts
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        held_by: Vec<String>,
        /// Whether no review round read the head, and no fix turn kelpie
        /// sent pushed it
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unread_head: bool,
        /// Whether the head is the worker's fix for the note a merge
        /// ruling's `no` sent, which no reviewer read
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        note_fix: bool,
        /// Whether the head is the worker's fix for a review bot's late
        /// round, which no reviewer read
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        late_fix: bool,
        /// The listed review bots' nits still open on the head, by bot,
        /// when any are. They never hold a merge.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nits: Option<String>,
        /// Where the disputes no reviewer read are, when the worker made any
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disputes: Option<String>,
        /// The other open pull requests that close the same issue, when any do, so the
        /// maintainer chooses which goes ahead
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rivals: Vec<u64>,
    },
    /// The worker ended its turn on a question. The answer is its next turn.
    Question {
        /// The question, verbatim from the worker's question block
        asked: String,
        /// Where the review stood when the question interrupted
        /// it, so the answer resumes the right place instead of the
        /// ordinary rule (a known pull request goes straight to CI)
        resume: Resume,
    },
    /// The work item cannot go on by itself. Its reason decides what a yes does.
    Stuck(Stuck),
    /// The pull request changes agents' own files, which decide what an
    /// agent runs in the worktree. A yes accepts them at this head; a no stops the work item.
    AgentFiles {
        /// The head that changes them
        head: String,
        /// The files it changes
        files: Vec<String>,
        /// The phase the gate was in, which a yes goes back to
        phase: Phase,
    },
    /// The pull request's labels or ready state changed outside kelpie. A
    /// yes accepts the change and kelpie carries on watching it.
    ForeignChange {
        /// What changed, named plainly enough to answer from a phone
        description: String,
        /// The labels and ready state kelpie adopts as its own on a yes
        known: Known,
    },
    /// A merged pull request left confirmed findings unfixed. A yes files
    /// each as an issue on the project, and a no drops them.
    FollowUp {
        /// The findings, at their reviewer's severity
        findings: Vec<Finding>,
        /// Why the forge would not take them, when it has refused for hours
        /// and a yes tries again. None when the ruling comes before filing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refused: Option<String>,
    },
    /// A contribution waits on the maintainer before anything of it runs. It
    /// parks no work item, and it is never posted on the contributor's pull
    /// request nor put to the project manager.
    Contribution(Held),
}

/// Why a work item is stuck, saved in its ruling's kind as `reason`
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Stuck {
    /// Kelpie could not rebase the branch onto `main`. A yes looks again.
    Rebase {
        /// Why
        why: String,
        /// The review round that was waiting on the head's run when the
        /// catch-up began, when one was; a yes goes back to it
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review: Option<Review>,
    },
    /// CI failed again on a head whose red run the worker already had, or
    /// failed once the worker had the fix turns `ci.fix_attempts` allows. A
    /// yes looks again.
    StillRed {
        /// The head
        head: String,
        /// The checks that failed
        checks: Vec<String>,
        /// The fix turns the worker had, when `ci.fix_attempts` is what
        /// raised it; none when the worker pushed no fix
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fix_turns: Option<u32>,
        /// The review round the red run interrupted, when one was running or
        /// due; a yes goes back to it
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review: Option<Review>,
        /// Whether the failed logs name no file the branch changed, so the
        /// ruling offers to rerun the head's failed jobs once more
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        rerun: bool,
    },
    /// A check stayed queued on a head after its workflow run was rerun. A
    /// yes reruns the run once more and goes back to waiting; a no reruns
    /// nothing and waits.
    CheckQueued {
        /// The head
        head: String,
        /// The checks still queued
        checks: Vec<String>,
        /// The workflow run they belong to
        run: u64,
        /// The run's page on the forge
        url: String,
        /// The review round whose CI wait it was, when one; an answer goes
        /// back to it
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review: Option<Review>,
    },
    /// A head with a pull request workflow to run it got no check past
    /// `ci.queued_minutes`. A yes passes that head as skipped and goes on; a
    /// no waits another `ci.queued_minutes`.
    NoChecks {
        /// The head
        head: String,
        /// The review round whose CI wait it was, when one; an answer goes
        /// back to it
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review: Option<Review>,
    },
    /// The forge refused an automatic merge again after a catch-up. A yes
    /// looks again.
    MergeRefused {
        /// The head the second refusal was about
        head: String,
        /// The forge's reason
        why: String,
    },
    /// Someone closed the pull request without merging it. A yes drops the
    /// work item and keeps its branch on the forge.
    Closed,
    /// The worker's fix turn for held findings ended with nothing pushed. A
    /// yes sends it the same prompt again: the findings, or the tests still
    /// failing.
    FixNotPushed {
        /// The round whose findings hold, still fixing
        #[serde(flatten)]
        fix: Fix,
        /// The fix turn a yes starts
        prompt: String,
        /// What the worker disputed and deferred of the findings, when it
        /// did either
        #[serde(default, skip_serializing_if = "Option::is_none")]
        said: Option<String>,
    },
    /// A reviewer's test still failed after the worker's second fix turn for
    /// it. A yes sends the worker one more fix turn on the same tests; a no
    /// ends the check and the pass goes on.
    TestStillFails {
        /// The round whose findings hold, with the tests still failing as its
        /// fix stage's sent findings
        #[serde(flatten)]
        fix: Fix,
        /// The head the tests failed at
        head: String,
        /// Every test of the round still failing
        tests: Vec<FailingTest>,
        /// The fix turn a yes starts
        prompt: String,
    },
    /// The worktree was still not the head on `origin` after the worker's
    /// turn to push or discard, so a review round did not run. A yes gives
    /// the worker another such turn.
    Unpushed {
        /// The files it holds uncommitted
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
        /// The commit it has checked out
        head: String,
        /// The branch's head on `origin`
        pushed: String,
        /// The review, at the round that did not run
        review: Review,
    },
    /// The fork a contributor's branch lives on refused kelpie's push of the
    /// worker's commits (edits turned off since, the branch deleted, or a
    /// branch that does not extend the contributor's). The commits stay in
    /// the worktree. A yes pushes them again.
    PushRefused {
        /// What git said
        why: String,
    },
    /// A worker's turn ran past its ceiling and kelpie stopped it, keeping
    /// its session. A yes resumes it; a no stops the work item.
    TurnTimeout {
        /// The phase the turn ran in, which a yes resumes. None in an
        /// older state file, which resumes under Implement.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<Phase>,
    },
    /// An adopted pull request cannot start, and no work item holds it. Any
    /// answer clears it, and the board tries again every poll.
    AdoptFailed {
        /// Why, as the board pass reads it
        why: String,
    },
    /// A worker's turn could not run, or its call failed. A yes retries the
    /// step that failed; a no stops the work item.
    TurnFailed {
        /// Why it failed
        why: String,
        /// The phase the turn ran in, which a yes goes back to
        phase: Phase,
        /// The turn as it stood before it failed, which a yes puts back
        retry: Turn,
        /// The agent files (`.claude/`, `CLAUDE.md`, `AGENTS.md`) the worktree
        /// holds uncommitted changes to, which the ruling offers to restore
        /// before the turn resumes
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        restore: Vec<String>,
        /// Kelpie could not set up the call, which a retry does not fix, so
        /// the ruling goes to the maintainer, never the project manager
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        setup: bool,
    },
    /// A work item's priced calls cost more than its cap, so its worker's
    /// next turn did not start. A yes doubles the cap, again while it is
    /// below the spend, and goes back to the phase; a no sends the worker its
    /// note ahead of the turn that was due, and raises the cap the same.
    OverBudget {
        /// The phase the next turn was due in, which an answer goes back to
        phase: Phase,
        /// What the work item's priced calls cost
        spent: Cost,
        /// The cap it was over: `budget.per_item_usd`, doubled once for each
        /// ruling like this one that was answered
        cap: Cost,
        /// What each role's priced calls cost
        by_role: Priced,
    },
}

impl From<Stuck> for RulingKind {
    fn from(reason: Stuck) -> Self {
        Self::Stuck(reason)
    }
}

/// A reviewer's test that failed at the fix's head, and what it wrote
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailingTest {
    /// The test's path in the repo
    pub test: String,
    /// The tail of its output
    pub output: String,
}

/// The round a fix that pushed nothing was for
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Fix {
    /// A review round: the review, still fixing it
    Review(Review),
}

/// Where the review stood when a worker's question interrupted
/// it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Resume {
    /// No pull request existed yet; answering it changes nothing
    Nothing,
    /// A pull request existed, but the review had not run its first round;
    /// once answered, start it
    ReviewFirst,
    /// The review had already reached this round and stage; once answered,
    /// resume exactly there
    Review(Review),
}

/// Why the state file cannot be read or written
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// Reading the file failed
    Read {
        /// The file
        path: PathBuf,
        /// What reading it failed with
        kind: io::ErrorKind,
    },
    /// The file is not a state file this version of kelpie wrote
    Malformed {
        /// The file
        path: PathBuf,
        /// The parser's message
        message: String,
    },
    /// The file carries a format version this kelpie does not read
    Version {
        /// The file
        path: PathBuf,
        /// The version it carries
        found: u32,
    },
    /// A pending ruling is of a kind this kelpie no longer has, so nothing
    /// here could answer it
    RemovedRuling {
        /// The file
        path: PathBuf,
        /// The ruling
        id: u64,
        /// Its kind, as the file names it
        kind: String,
    },
    /// Writing the file failed, and the previous state still stands
    Write {
        /// The file
        path: PathBuf,
        /// What writing it failed with
        kind: io::ErrorKind,
    },
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, kind } => {
                write!(f, "cannot read state file {}: {kind}", path.display())
            }
            Self::Malformed { path, message } => {
                write!(f, "state file {} is malformed: {message}", path.display())
            }
            Self::Version { path, found } => write!(
                f,
                "state file {} is version {found}, and this kelpie reads versions {ONE_ITEM} to {VERSION}",
                path.display()
            ),
            Self::RemovedRuling { path, id, kind } => write!(
                f,
                "state file {} holds ruling {id} of kind `{kind}`, which this kelpie no \
                 longer has: answer or drop it on the old build first",
                path.display()
            ),
            Self::Write { path, kind } => {
                write!(f, "cannot write state file {}: {kind}", path.display())
            }
        }
    }
}

impl core::error::Error for StateError {}

/// Where a project's state lives on disk
#[derive(Debug, Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    /// A store for the state file at `path`
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The workers of the work items a file saved before agent files holds,
    /// as it holds them; none once it is saved again, or when it cannot be read
    pub fn old_workers(&self) -> Vec<OldWorker> {
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        serde_json::from_str(&text).map_or_else(|_| Vec::new(), |v| removed::old_workers(&v))
    }

    /// Reads the state, or `None` when no state has been saved yet
    ///
    /// # Errors
    ///
    /// [`StateError`] when the file exists and cannot be read or understood.
    pub fn load(&self) -> Result<Option<ProjectState>, StateError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(self.error_read(e)),
        };

        // Only the version first, so a newer format reports as one.
        #[derive(Deserialize)]
        struct Versioned {
            version: u32,
        }
        let malformed = |e: serde_json::Error| StateError::Malformed {
            path: self.path.clone(),
            message: e.to_string(),
        };
        let found = serde_json::from_str::<Versioned>(&text)
            .map_err(malformed)?
            .version;
        if !(ONE_ITEM..=VERSION).contains(&found) {
            return Err(StateError::Version {
                path: self.path.clone(),
                found,
            });
        }
        let mut value: serde_json::Value = serde_json::from_str(&text).map_err(malformed)?;
        if let Some((id, kind)) = removed::removed_ruling(&value) {
            let path = self.path.clone();
            return Err(StateError::RemovedRuling { path, id, kind });
        }
        removed::drop_removed_fields(&mut value);
        removed::withdraw_unlabelled(&mut value);
        if found <= BUILT_IN_REVIEWERS {
            removed::name_reviewers_as_files(&mut value);
        }
        if found <= BOT_ROUNDS {
            removed::fold_bot_rounds(&mut value);
        }
        removed::fold_ruling_kinds(&mut value);
        let mut state: ProjectState = serde_json::from_value(value).map_err(malformed)?;
        if found <= BEFORE_TALKED {
            // The worker has seen what is on a pull request already open.
            for item in state
                .work_items
                .iter_mut()
                .filter(|i| i.pull_request.is_some())
            {
                item.talked.quiet = true;
            }
        }
        if found <= BEFORE_ANSWERED {
            // What the contributor said before kelpie answered anyone is not
            // answered now: the maintainer has seen it, and a reply to it
            // would be stale.
            for held in &mut state.contributions {
                held.answered = held.unanswered();
            }
        }
        state.triage.iter_mut().for_each(|t| t.migrate(found));
        let state = state.one_item_moved();
        let state = if found < NUMBERED {
            state.rulings_numbered()
        } else {
            state
        };
        Ok(Some(ProjectState {
            version: VERSION,
            ..state
        }))
    }

    /// Replaces the saved state with `state`, atomically
    ///
    /// # Errors
    ///
    /// [`StateError::Write`] when any step fails. The previous state stands.
    pub fn save(&self, state: &ProjectState) -> Result<(), StateError> {
        let mut bytes = serde_json::to_vec_pretty(state).expect("state serializes to JSON");
        bytes.push(b'\n');
        // A project `shep kelpie add` set up has no folder of its own yet.
        let made = self.path.parent().map_or(Ok(()), std::fs::create_dir_all);
        made.and_then(|()| write_atomically(&self.path, &bytes))
            .map_err(|e| StateError::Write {
                path: self.path.clone(),
                kind: e.kind(),
            })
    }

    fn error_read(&self, e: io::Error) -> StateError {
        StateError::Read {
            path: self.path.clone(),
            kind: e.kind(),
        }
    }
}

pub(crate) fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    path.with_file_name(name)
}

/// The rename is the commit point. Syncing the file first means the name
/// never points at unwritten bytes; syncing the folder keeps the rename.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = temporary_path(path);
    let mut file = File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    let folder = path.parent().filter(|p| !p.as_os_str().is_empty());
    File::open(folder.unwrap_or(Path::new(".")))?.sync_all()
}

#[cfg(test)]
mod tests;
