//! What each step of the runner did, one JSON line each in its log
//!
//! A step dispatches from the board, runs a worker's turn, moves the work
//! item through the gate (CI, a rebase, a ruling, the merge), posts a
//! ruling to the maintainer's webhook, or handles a reply on its topic.

use std::path::PathBuf;

use serde::Serialize;

use crate::board::{Skip, WorkerModel};
use crate::pacer::HoldKind;
use crate::ports::{
    AgentCall, Cost, Finding, Role, SessionId, Severity, Timestamp, Usage, Verdict,
};
use crate::settings::{LocalRound, ReviewerName};
use crate::shots::ShotsJob;
use crate::work_item::{QwenTally, Spend, Split};

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
    /// The board's oldest free issue opened a work item
    Dispatched {
        /// The work item's issue
        issue: u64,
        /// The model and effort its worker runs on
        worker: WorkerModel,
        /// Older ready issues the board passed over, and why
        skipped: Vec<Skip>,
    },
    /// A pull request kelpie opened asked for a rework, which opened a work
    /// item
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
    /// An adopted pull request opened a work item
    Adopted {
        /// The work item's issue: the one the pull request closes
        issue: u64,
        /// The pull request
        pull_request: u64,
        /// The model and effort its worker runs on
        worker: WorkerModel,
    },
    /// An adopted pull request cannot start, and the refusal went to it as a
    /// comment
    AdoptRefused {
        /// The pull request
        pull_request: u64,
        /// Why, as the refusal reads
        reason: String,
        /// Why the comment could not be posted, if it could not
        comment_failed: Option<String>,
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
    /// An issue whose sub-issues are all closed was closed too
    ParentClosed {
        /// The issue
        issue: u64,
    },
    /// An issue whose sub-issues are all closed could not be closed this step
    ParentCloseFailed {
        /// The issue
        issue: u64,
        /// Why
        reason: String,
        /// Whether the forge refused too often in a row, so this runner
        /// passes the issue over until it restarts
        given_up: bool,
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
        /// What the call cost, in US dollars, or null when its harness
        /// reports no cost
        cost_usd: Option<f64>,
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
        /// What the call cost, in US dollars, or null when its harness
        /// reports no cost
        cost_usd: Option<f64>,
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
    /// A turn could not run, and is parked on a ruling
    Failed {
        /// The work item's issue
        issue: u64,
        /// The worker's draft pull request, once it has opened one
        pull_request: Option<u64>,
        /// The ruling's id
        id: u64,
        /// The question, carrying why the turn failed and the triggers that answer it
        question: String,
        /// Why the question could not be posted on the pull request, if it could not
        comment_failed: Option<String>,
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
    /// The branch conflicts with `main`, and the conflict is the worker's next turn
    Conflicted {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The head that conflicts
        head: String,
        /// The files that conflict
        files: Vec<String>,
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
    /// The draft was marked ready, before a CodeRabbit round or after a yes
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
    /// A merge under `auto` no longer held, or the forge refused it, so CI
    /// runs again
    MergeWithdrawn {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// What changed since the gate passed, or why the forge refused
        reason: String,
    },
    /// Under `auto`, a head no gate saw goes back through the qwen-review
    /// loop and CodeRabbit before any merge
    Regated {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The head adopted
        head: String,
    },
    /// The work item is gone: its worktree, branch and build folder removed
    Finished {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: Option<u64>,
        /// Whether the pull request merged
        merged: bool,
        /// What its Claude calls cost, by role
        spend: Box<Spend>,
        /// Its qwen rounds
        qwen: QwenTally,
        /// Where its time went
        timings: Split,
    },
    /// Findings a merged pull request left unfixed were filed
    FollowUpsFiled {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The issues opened, one per finding
        opened: Vec<u64>,
        /// The open issues that already held a finding, and got a comment
        commented: Vec<u64>,
        /// How many findings were left out because the text names a folder on this machine
        skipped: usize,
    },
    /// A ruling was posted to the maintainer's webhook
    Alerted {
        /// The ruling's id
        id: u64,
    },
    /// A ruling could not be sent, and is tried again later
    AlertFailed {
        /// The ruling's id
        id: u64,
        /// Why, never naming the webhook's URL
        reason: String,
        /// When the post is tried again at the earliest
        retry_at: Timestamp,
    },
    /// The notice of an automatic merge was sent to the webhook, or only logged where there is none
    Noticed {
        /// The work item's issue
        issue: u64,
        /// The pull request merged
        pull_request: u64,
    },
    /// A notice could not be posted to the webhook, and is tried again later
    NoticeFailed {
        /// The work item's issue
        issue: u64,
        /// The pull request merged
        pull_request: u64,
        /// Why, never naming the webhook's URL
        reason: String,
        /// When the post is tried again at the earliest
        retry_at: Timestamp,
    },
    /// A reply on the webhook's topic, carrying the authenticator code, answered it
    ReplyAnswered {
        /// The ruling's id
        id: u64,
    },
    /// A reply carrying the authenticator code was refused, by `rule` or
    /// because its code could not be recorded, and the topic was told why
    ReplyRefused {
        /// The ruling's id
        id: u64,
        /// Why, as `rule` refused it
        reason: String,
        /// Why the line for the topic could not be posted, if it could not
        line_failed: Option<String>,
    },
    /// A reply with the authenticator code named a ruling already settled:
    /// it ran nothing, and the topic was told so
    ReplyToSettled {
        /// The ruling's id
        id: u64,
        /// Why the line for the topic could not be posted, if it could not
        line_failed: Option<String>,
    },
    /// A reply carried a right code that already answered a reply: it ran
    /// nothing, and the topic was told so
    ReplyCodeUsed {
        /// The ruling's id
        id: u64,
        /// Why the line for the topic could not be posted, if it could not
        line_failed: Option<String>,
    },
    /// A message on the webhook's topic carried no right code, and was
    /// ignored. Its text is not logged, since anyone holding the topic can
    /// write it.
    ReplyIgnored,
    /// A post on the topic held text kelpie could not read in full: every
    /// step a code in it could name was spent, it answered nothing, and the
    /// topic was told to send a shorter reply
    ReplyTooLong {
        /// Why the line for the topic could not be posted, if it could not
        line_failed: Option<String>,
    },
    /// A wrong code turned answers from the topic off, for every project,
    /// until the maintainer turns them back on, and the topic was told so
    RepliesLocked {
        /// Why the line for the topic could not be posted, if it could not
        line_failed: Option<String>,
    },
    /// The webhook's topic could not be read, and is read again later
    RepliesFailed {
        /// Why, never naming the webhook's URL
        reason: String,
        /// When it is read again at the earliest
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
        reviewer: ReviewerName,
        /// How many findings it reported
        findings: usize,
        /// The files it could not review, which are no findings
        #[serde(skip_serializing_if = "Vec::is_empty")]
        unreviewed: Vec<String>,
    },
    /// A local round reviewed no file at all, so it counts for nothing
    LocalRoundFailed {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// Which reviewer ran it
        reviewer: ReviewerName,
        /// The files it could not review
        unreviewed: Vec<String>,
        /// Whether the round runs again; if not, the loop goes on without
        /// this reviewer
        retrying: bool,
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
    /// CodeRabbit gave no sign of the summon in fifteen minutes, so kelpie
    /// sent it once more, in the same round
    SummonedAgain {
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
    /// Kelpie took shots of a head, before a Claude review round or the merge ruling
    Shots {
        /// The work item's issue
        issue: u64,
        /// The head the worktree held
        head: String,
        /// How many screenshots it took
        shots: usize,
        /// What went wrong, each naming its page, the whole run's failure included
        problems: Vec<String>,
    },
    /// The shots of the head about to be ruled on are on its pull request's comment
    ShotsPosted {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The head they show
        head: String,
        /// The comment's id, edited in place by every later run
        comment: u64,
    },
    /// The shots could not all reach the pull request; the merge ruling goes on
    ShotsNotPosted {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// Why
        reason: String,
    },
    /// A reader of the deep round reported
    DeepRead {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// Which reader: 1 for the first, 2 for the one shown its findings
        reader: u8,
        /// How many findings it reported
        findings: usize,
    },
    /// A session that may run commands tried to confirm a HIGH with a failing test
    DeepConfirmed {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// Where the finding is, as `file:line`
        finding: String,
        /// Whether it wrote a test that fails; if not, the HIGH is sent unconfirmed
        backed: bool,
    },
    /// The re-check of the deep round's fix ran
    DeepRechecked {
        /// The work item's issue
        issue: u64,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// How many findings the fix fixed
        fixed: usize,
        /// How many the worker left for an issue of their own, which are not re-checked
        deferred: usize,
        /// How many it did not fix
        unfixed: usize,
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
            Self::BoardFailed { .. }
                | Self::ParentCloseFailed { .. }
                | Self::GateFailed { .. }
                | Self::Held { .. }
                | Self::RepliesFailed { .. }
        )
    }
}

/// What a step found there was to do, before the outer loop runs it
pub(super) enum Begin {
    Idle,
    Report(StepReport),
    Call(AgentCall),
    Review(ReviewCall),
    /// A shots run of this head
    Shots(Box<ShotsJob>, String),
}

/// Something the review loop needs run outside the runner's lock
pub(super) enum ReviewCall {
    /// One local round, of the project's kind
    Local {
        local: LocalRound,
        worktree: PathBuf,
        base: String,
        out: PathBuf,
        round: u32,
        criteria: String,
    },
    /// A fresh Claude review round
    ClaudeRound(AgentCall),
    /// The judge's one-shot on a single finding
    Judge(AgentCall),
    /// A session of the deep round: a reader, a confirmation or a re-check
    Deep(AgentCall),
}

/// What a [`ReviewCall`] cost, for the work item's record
pub(super) enum Spent {
    /// A Claude call that came back, in `session`
    Claude {
        role: Role,
        session: SessionId,
        usage: Usage,
        session_cost: Option<Cost>,
    },
    /// A local round that ran, however it ended
    Local,
}

/// What a [`ReviewCall`] came back with, and what it spent
pub(super) struct Reviewed {
    pub result: ReviewResult,
    /// `None` when nothing ran to the end: a stopped call, or a Claude call
    /// that failed and so reported no cost
    pub spent: Option<Spent>,
}

/// What a [`ReviewCall`] came back with
pub(super) enum ReviewResult {
    /// A round's raw findings, from the local round or a Claude round
    Findings(Result<Vec<Finding>, String>),
    /// The judge's verdict on one finding
    Verdict(Result<Verdict, String>),
    /// The local model sat partly or wholly on the CPU, so the round did not
    /// run, and why
    Spilled(String),
    /// What a deep round's session said, as text, or why its call failed
    Deep(Result<String, String>),
    /// The call was ended because the runner is stopping, before it came
    /// back with anything
    Stopped,
}
