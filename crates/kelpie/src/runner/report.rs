//! What each step of the runner did, one JSON line each in its log
//!
//! A step dispatches from the board, runs a worker's turn, moves the work
//! item through the gate (CI, a rebase, a ruling, the merge), or posts a
//! ruling to the maintainer's webhook.

use std::path::PathBuf;

use serde::Serialize;

use crate::board::{Skip, WorkerModel};
use crate::pacer::HoldKind;
use crate::ports::{ClaudeCall, Finding, SessionId, Severity, Timestamp, Usage, Verdict};
use crate::work_item::ReviewerKind;

/// What asked for a rework on the pull request itself
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReworkBy {
    /// The `ready-for-agent` label
    Label,
    /// A review requesting changes
    Review,
}

/// What one step of the runner did, for its log
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "step", rename_all = "kebab-case")]
pub enum StepReport {
    /// The board's oldest free issue became the work item in flight
    Dispatched {
        /// The work item's issue
        issue: u64,
        /// The model and effort its worker runs on
        worker: WorkerModel,
        /// Older ready issues the board passed over, and why
        skipped: Vec<Skip>,
    },
    /// A pull request kelpie opened asked for a rework, which became the
    /// work item in flight
    Reworked {
        /// The work item's issue
        issue: u64,
        /// The pull request
        pull_request: u64,
        /// The model and effort its worker runs on
        worker: WorkerModel,
        /// What asked for it: the `ready-for-agent` label, or a review
        /// requesting changes
        by: ReworkBy,
    },
    /// A pull request asked for a rework that cannot start, and the refusal
    /// went to it as a comment
    ReworkRefused {
        /// The pull request
        pull_request: u64,
        /// Why, as the refusal reads
        reason: String,
        /// Why the comment could not be posted, if it could not
        comment_failed: Option<String>,
    },
    /// Nothing was dispatched: the board could not be read, or the issue it
    /// picked could not be taken
    BoardFailed {
        /// Why
        reason: String,
    },
    /// The pacer found a limit reached, so nothing new started
    Held {
        /// Which limit
        kind: HoldKind,
        /// Why, as `status` shows it
        reason: String,
        /// When the pacer reads usage again at the latest
        until: Timestamp,
    },
    /// A turn ended and its call was recorded
    Ended {
        /// The work item's issue
        issue: u64,
        /// The worker's session
        session: SessionId,
        /// What the call used
        usage: Usage,
        /// What the call cost, in US dollars
        cost_usd: f64,
        /// What the work item has cost so far, in US dollars
        work_item_cost_usd: f64,
        /// The worker's draft pull request, once it has opened one
        pull_request: Option<u64>,
    },
    /// A turn ended on the worker's question, and the worker is parked on it
    Asked {
        /// The work item's issue
        issue: u64,
        /// The worker's session
        session: SessionId,
        /// What the call used
        usage: Usage,
        /// What the call cost, in US dollars
        cost_usd: f64,
        /// What the work item has cost so far, in US dollars
        work_item_cost_usd: f64,
        /// The worker's draft pull request, once it has opened one
        pull_request: Option<u64>,
        /// The ruling's id
        id: u64,
        /// The question, with the trigger that answers it
        question: String,
        /// Why the question could not be posted on the pull request, if it could not
        comment_failed: Option<String>,
    },
    /// A turn could not run
    Failed {
        /// The work item's issue
        issue: u64,
        /// Why
        reason: String,
    },
    /// A turn ran past its ceiling, was stopped, and is parked on a ruling
    TimedOut {
        /// The work item's issue
        issue: u64,
        /// The worker's session, which the ruling's yes resumes
        session: SessionId,
        /// The worker's draft pull request, once it has opened one
        pull_request: Option<u64>,
        /// The ruling's id
        id: u64,
        /// The question, with the triggers that answer it
        question: String,
        /// Why the question could not be posted on the pull request, if it could not
        comment_failed: Option<String>,
    },
    /// CI failed, and the failure is the worker's next turn
    CiFailed {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The head CI ran on
        head: String,
        /// The checks that failed
        checks: Vec<String>,
    },
    /// The branch was rebased onto `main` and pushed, and CI runs again
    Rebased {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The rebased head
        head: String,
    },
    /// A ruling was raised, and the worker is parked on it
    Ruling {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The ruling's id
        id: u64,
        /// The question, with the triggers that answer it
        question: String,
        /// Why the question could not be posted on the pull request, if it could not
        comment_failed: Option<String>,
    },
    /// The draft was marked ready after a yes; the merge waits for CI to settle
    MarkedReady {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
    },
    /// A yes no longer held when kelpie came to merge, so CI runs again
    YesWithdrawn {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// What changed since the question
        reason: String,
    },
    /// The work item is gone: its worktree, branch and build folder removed
    Finished {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: Option<u64>,
        /// Whether the pull request merged
        merged: bool,
    },
    /// A ruling was posted to the maintainer's webhook
    Alerted {
        /// The ruling's id
        id: u64,
    },
    /// A ruling could not be posted to the webhook, and is tried again later
    AlertFailed {
        /// The ruling's id
        id: u64,
        /// Why, never naming the webhook's URL
        reason: String,
        /// When the post is tried again at the earliest
        retry_at: Timestamp,
    },
    /// The forge or git could not be asked, and the step is tried again later
    GateFailed {
        /// The work item's issue
        issue: u64,
        /// Why
        reason: String,
    },
    /// A review round ran and reported its raw findings
    ReviewRound {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// Which reviewer ran it
        reviewer: ReviewerKind,
        /// How many findings it reported
        findings: usize,
    },
    /// The judge ruled on one finding
    FindingJudged {
        /// The work item's issue
        issue: u64,
        /// The round the finding came from
        round: u32,
        /// Whether it held
        holds: bool,
        /// The judge's severity
        severity: Severity,
    },
    /// Kelpie put the `review please` label on, holding the CodeRabbit lease
    Summoned {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The head the summon is for
        head: String,
    },
    /// CodeRabbit refused the summon, and the label came off
    SummonRefused {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// When CodeRabbit said its window opens
        opens: Timestamp,
    },
    /// A CodeRabbit review covered the head, and the label came off
    CodeRabbitReviewed {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// Which round this was
        round: u32,
        /// Its threads still open, which the judge now reads
        open_threads: usize,
    },
    /// The judge ruled on every open CodeRabbit thread
    CodeRabbitJudged {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// Threads the judge held, sent to the worker
        held: usize,
        /// Threads the judge rejected, now resolved
        resolved: usize,
    },
    /// No CodeRabbit thread is open and the judge holds nothing: CI, then
    /// the merge ruling
    CodeRabbitSatisfied {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// Rounds it took
        rounds: u32,
    },
    /// The round's held findings were sent to the worker's next turn
    ReviewFindingsSent {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// How many findings held
        held: usize,
        /// Whether every one was a nit (LOW)
        clean: bool,
    },
    /// The worker's fix turn for a round's held findings ended, and the
    /// round counts
    FixPushed {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The round whose findings it fixed
        round: u32,
        /// The head it pushed; none when an older state file kept no head
        head: Option<String>,
    },
}

impl StepReport {
    /// Whether the runner should wait before its next step, rather than go on
    pub fn waits(&self) -> bool {
        matches!(
            self,
            Self::BoardFailed { .. } | Self::GateFailed { .. } | Self::Held { .. }
        )
    }
}

/// What a step found there was to do, before the outer loop runs it
pub(super) enum Begin {
    Idle,
    Report(StepReport),
    Call(ClaudeCall),
    Review(ReviewCall),
}

/// Something the qwen-review loop needs run outside the runner's lock
pub(super) enum ReviewCall {
    /// One round of the maintainer's script
    Qwen {
        worktree: PathBuf,
        out: PathBuf,
        round: u32,
    },
    /// A fresh Claude review round
    ClaudeRound(ClaudeCall),
    /// The judge's one-shot on a single finding
    Judge(ClaudeCall),
}

/// What a [`ReviewCall`] came back with
pub(super) enum ReviewResult {
    /// A round's raw findings, from qwen or a Claude round
    Findings(Result<Vec<Finding>, String>),
    /// The judge's verdict on one finding
    Verdict(Result<Verdict, String>),
}
