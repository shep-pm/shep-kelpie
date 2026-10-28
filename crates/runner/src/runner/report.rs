//! What each step of the runner did, one JSON line each in its log
//!
//! A step dispatches from the board, runs a worker's turn, moves the work
//! item through the gate (CI, a rebase, a ruling, the merge), posts a
//! ruling to the maintainer's webhook, or handles a reply on its topic.

use std::path::PathBuf;

use serde::Serialize;

use crate::board::Skip;
use crate::pacer::HoldKind;
use crate::ports::{
    AgentCall, Cost, Finding, Role, RoundEnd, SessionId, Timestamp, Transcript, Usage,
};
use crate::settings::{AgentName, LocalRound};
use crate::ticket::TicketId;
use crate::work_item::{QwenTally, Spend, Split, TestCheck};

fn none_left(n: &usize) -> bool {
    *n == 0
}

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
        /// The work item's ticket
        issue: TicketId,
        /// The implementer its worker runs on
        agent: AgentName,
        /// Older ready issues the board passed over, and why
        skipped: Vec<Skip>,
    },
    /// A work item's own implementer would finish past its slack, so it was
    /// handed off to the agent board's next pick before its due turn
    HandedOff {
        /// The work item's ticket
        issue: TicketId,
        /// The implementer it left
        from: AgentName,
        /// The implementer its next turn runs on, on a session of its own
        to: AgentName,
        /// Seconds to the finish the board gave `from`, None when it ranked it not
        #[serde(skip_serializing_if = "Option::is_none")]
        from_finish: Option<u64>,
        /// Seconds to the finish the board gave `to`
        to_finish: u64,
    },
    /// A pull request kelpie opened asked for a rework, which opened a work
    /// item
    Reworked {
        /// The work item's ticket
        issue: TicketId,
        /// The pull request
        pull_request: u64,
        /// The implementer its worker runs on
        agent: AgentName,
        /// What asked for it: the `ready-for-agent` label, or a review
        /// requesting changes
        by: ReworkBy,
    },
    /// An adopted pull request opened a work item
    Adopted {
        /// The work item's ticket: the one its pull request closes
        issue: TicketId,
        /// The pull request
        pull_request: u64,
        /// The implementer its worker runs on
        agent: AgentName,
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
        /// The ticket
        issue: TicketId,
    },
    /// An issue whose sub-issues are all closed could not be closed this step
    ParentCloseFailed {
        /// The ticket
        issue: TicketId,
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
    /// A work item's worktree had its packages installed before its turn, in the worker's
    /// sandbox. A failed install does not stop the turn: the worker's prompt says why.
    PackagesInstalled {
        /// The work item's ticket
        issue: TicketId,
        /// The folders it installed, relative to the worktree
        folders: Vec<String>,
        /// Why the install failed, if it did
        failed: Option<String>,
    },
    /// A turn ended and its call was recorded
    Ended {
        /// The work item's ticket
        issue: TicketId,
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
        /// The work item's ticket
        issue: TicketId,
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
        id: TicketId,
        /// The question, with the trigger that answers it
        question: String,
        /// Why the question could not be posted on the pull request, if it could not
        comment_failed: Option<String>,
    },
    /// A turn could not run, and is parked on a ruling
    Failed {
        /// The work item's ticket
        issue: TicketId,
        /// The worker's draft pull request, once it has opened one
        pull_request: Option<u64>,
        /// The ruling's id
        id: TicketId,
        /// The question, carrying why the turn failed and the triggers that answer it
        question: String,
        /// Why the question could not be posted on the pull request, if it could not
        comment_failed: Option<String>,
    },
    /// A turn ran past its ceiling, was stopped, and is parked on a ruling
    TimedOut {
        /// The work item's ticket
        issue: TicketId,
        /// The worker's session, which the ruling's yes resumes
        session: SessionId,
        /// The worker's draft pull request, once it has opened one
        pull_request: Option<u64>,
        /// The ruling's id
        id: TicketId,
        /// The question, with the triggers that answer it
        question: String,
        /// Why the question could not be posted on the pull request, if it could not
        comment_failed: Option<String>,
    },
    /// A turn pushed its branch, but kelpie could not open the pull request
    /// from the worker's `pull-request.md`, and the refusal is the worker's
    /// next turn
    PullRequestNotOpened {
        /// The work item's ticket
        issue: TicketId,
        /// Why, as the forge, the guard or a project's hook refused it
        reason: String,
    },
    /// CI failed, and the failure is the worker's next turn
    CiFailed {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The head CI ran on
        head: String,
        /// The checks that failed
        checks: Vec<String>,
    },
    /// CI failed, its logs named no file the branch changed, and kelpie
    /// reran the failed jobs instead of sending the worker a fix turn; or a
    /// check stayed queued past `ci.queued_minutes` and kelpie reran its run,
    /// when `checks` are the queued ones
    CiRerun {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The head CI ran on
        head: String,
        /// The checks that failed
        checks: Vec<String>,
    },
    /// The branch was rebased onto `main` and pushed, and CI runs again
    Rebased {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The rebased head
        head: String,
    },
    /// The branch conflicts with `main`, and the conflict is the worker's next turn
    Conflicted {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The head that conflicts
        head: String,
        /// The files that conflict
        files: Vec<String>,
    },
    /// A ruling was raised, and the worker is parked on it
    Ruling {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The ruling's id
        id: TicketId,
        /// The question, with the triggers that answer it
        question: String,
        /// Why the question could not be posted on the pull request, if it could not
        comment_failed: Option<String>,
    },
    /// The draft was marked ready, before a review bot's summon or after a yes
    MarkedReady {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
    },
    /// A yes no longer held when kelpie came to merge, so CI runs again
    YesWithdrawn {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// What changed since the question
        reason: String,
    },
    /// A merge under `auto` no longer held, or the forge refused it, so CI
    /// runs again
    MergeWithdrawn {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// What changed since the gate passed, or why the forge refused
        reason: String,
    },
    /// Under `auto`, a head no gate saw goes back through the review before
    /// any merge
    Regated {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The head adopted
        head: String,
    },
    /// The work item is gone: its worktree, branch and build folder removed
    Finished {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: Option<u64>,
        /// Whether the pull request merged
        merged: bool,
        /// Whether its issue was closed with no change, so it ended with no
        /// pull request
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        closed: bool,
        /// What its Claude calls cost, by role
        spend: Box<Spend>,
        /// Its qwen rounds
        qwen: QwenTally,
        /// Where its time went
        timings: Split,
    },
    /// The runner was finishing and its last open work item ended, so it
    /// stops its own sheep
    RunFinished,
    /// Findings a merged pull request left unfixed were filed
    FollowUpsFiled {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The tickets opened, one per finding
        opened: Vec<TicketId>,
        /// The open tickets that already held a finding, and got a comment
        commented: Vec<TicketId>,
        /// How many findings were left out because the text names a folder on this machine
        skipped: usize,
    },
    /// A ruling was posted to the maintainer's webhook
    Alerted {
        /// The ruling's id
        id: TicketId,
    },
    /// A ruling could not be sent, and is tried again later
    AlertFailed {
        /// The ruling's id
        id: TicketId,
        /// Why, never naming the webhook's URL
        reason: String,
        /// When the post is tried again at the earliest
        retry_at: Timestamp,
    },
    /// The notice of an automatic merge was sent to the webhook, or only logged where there is none
    Noticed {
        /// The work item's ticket
        issue: TicketId,
        /// The pull request merged
        pull_request: u64,
    },
    /// A notice could not be posted to the webhook, and is tried again later
    NoticeFailed {
        /// The work item's ticket
        issue: TicketId,
        /// The pull request merged
        pull_request: u64,
        /// Why, never naming the webhook's URL
        reason: String,
        /// When the post is tried again at the earliest
        retry_at: Timestamp,
    },
    /// The notice of a workflow on `main` that stayed red, or went green, was
    /// sent to the webhook, or only logged where there is none
    MainNoticed {
        /// The workflow's name
        workflow: String,
        /// Whether it went green
        green: bool,
    },
    /// A notice of a workflow on `main` could not be posted to the webhook,
    /// and is tried again later
    MainNoticeFailed {
        /// The workflow's name
        workflow: String,
        /// Whether it went green
        green: bool,
        /// Why, never naming the webhook's URL
        reason: String,
        /// When the post is tried again at the earliest
        retry_at: Timestamp,
    },
    /// A reply on the webhook's topic, carrying the authenticator code, answered it
    ReplyAnswered {
        /// The ruling's id
        id: TicketId,
    },
    /// A reply carrying the authenticator code was refused, by `rule` or
    /// because its code could not be recorded, and the topic was told why
    ReplyRefused {
        /// The ruling's id
        id: TicketId,
        /// Why, as `rule` refused it
        reason: String,
        /// Why the line for the topic could not be posted, if it could not
        line_failed: Option<String>,
    },
    /// A reply with the authenticator code named a ruling already settled:
    /// it ran nothing, and the topic was told so
    ReplyToSettled {
        /// The ruling's id
        id: TicketId,
        /// Why the line for the topic could not be posted, if it could not
        line_failed: Option<String>,
    },
    /// A reply carried a right code that already answered a reply: it ran
    /// nothing, and the topic was told so
    ReplyCodeUsed {
        /// The ruling's id
        id: TicketId,
        /// Why the line for the topic could not be posted, if it could not
        line_failed: Option<String>,
    },
    /// A reply carried a right code but named no ruling kelpie could read:
    /// it answered nothing, its code is spent, and the topic was told so
    ReplyUnreadable {
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
        /// The work item's ticket
        issue: TicketId,
        /// Why
        reason: String,
    },
    /// A review round ran and reported its raw findings
    ReviewRound {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// Which reviewer ran it
        reviewer: AgentName,
        /// How many findings it reported
        findings: usize,
        /// The files it could not review, which are no findings
        #[serde(skip_serializing_if = "Vec::is_empty")]
        unreviewed: Vec<String>,
    },
    /// A reviewer's call failed three times in a row, or a review bot's
    /// window opens more than an hour on or it never answered its summon, so
    /// the review went on to the next reviewer without it
    ReviewerSkipped {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// Which reviewer it was
        reviewer: AgentName,
        /// Why its last call failed, or why the bot was passed over
        reason: String,
    },
    /// A local round reviewed no file at all, so the review went on to the
    /// next reviewer
    LocalRoundFailed {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// Which reviewer ran it
        reviewer: AgentName,
        /// The files it could not review
        unreviewed: Vec<String>,
    },
    /// Kelpie summoned a review bot, by its label or its comment, holding its lease
    Summoned {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The head the summon is for
        head: String,
    },
    /// The bot gave no sign of the summon in fifteen minutes, so kelpie
    /// sent it once more, in the same round
    SummonedAgain {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The head the summon is for
        head: String,
    },
    /// The bot refused the summon, and its label came off
    SummonRefused {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// When the bot said its window opens, or its window's span from now
        opens: Timestamp,
    },
    /// A review bot's review covered the head, and its label came off. Its
    /// open threads are the round's findings.
    BotReviewed {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// Which reviewer it was
        reviewer: AgentName,
        /// Its threads still open
        open_threads: usize,
    },
    /// The fix moved the head, but the forge kept refusing to resolve these
    /// threads, so the review went on with them open
    ThreadsLeftOpen {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The forge's ids of the threads still open
        threads: Vec<String>,
        /// Why the last try failed
        reason: String,
    },
    /// The round's findings were sent to the worker's next turn, or, with
    /// none, the review moved on
    ReviewFindingsSent {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// How many findings were sent
        held: usize,
    },
    /// A reviewer with a second look read once, and reads again next,
    /// shown what it found
    FirstLook {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The round
        round: u32,
        /// Who reviews it
        reviewer: AgentName,
        /// How many findings the first look reported
        findings: usize,
    },
    /// A review pass ended with no reviewer having read the pull request,
    /// as a listed one was down or kept failing, so the work item is marked
    /// unreviewed and goes to CI
    Unreviewed {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// Which reviewers did not read it, and why
        reason: String,
    },
    /// The worker's fix turn pushed nothing, having deferred every finding
    /// sent as out of scope, so the next reviewer reads the pull request as
    /// it stands
    FindingsDeferred {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The round whose findings it deferred
        round: u32,
        /// How many findings it deferred
        deferred: usize,
    },
    /// The worker's fix turn pushed nothing, having shown every finding sent
    /// that was not a nit to be wrong or deferred it, so the next reviewer
    /// is shown its reasons and reads the pull request as it stands
    FindingsDisputed {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The round whose findings it disputed
        round: u32,
        /// How many findings it disputed, each with a reason
        disputed: usize,
        /// How many it deferred as out of scope
        deferred: usize,
    },
    /// The worker's fix turn was sent only nits and pushed nothing, which
    /// declines them, so the next reviewer reads the pull request as it stands
    NitsDeclined {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The round whose nits it declined
        round: u32,
        /// How many nits it was sent
        nits: usize,
    },
    /// The worktree was not the head on `origin` as a review round was
    /// about to run, so the worker's next turn pushes or discards first
    Unpushed {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The round that waits
        round: u32,
        /// The files the worktree holds uncommitted
        files: Vec<String>,
        /// The commit the worktree has checked out
        head: String,
        /// The branch's head on `origin`
        pushed: String,
    },
    /// The worker's fix turn for a round's held findings ended, and the
    /// round counts
    FixPushed {
        /// The work item's ticket
        issue: TicketId,
        /// Its pull request
        pull_request: u64,
        /// The round whose findings it fixed
        round: u32,
        /// The head it pushed; none when an older state file kept no head
        head: Option<String>,
        /// What the round's attached tests did, before the fix and after it
        #[serde(skip_serializing_if = "Vec::is_empty")]
        checks: Vec<TestCheck>,
        /// How many attached tests were not run after the fix, because the
        /// head it pushed is not the worker's own
        #[serde(skip_serializing_if = "none_left")]
        unchecked: usize,
    },
    /// The project manager answered a wake, and kelpie acted on what the
    /// board allowed
    PmAnswered {
        /// Its session
        session: SessionId,
        /// What woke it
        woke_for: Vec<String>,
        /// What kelpie did with its answer
        acted: Vec<String>,
        /// What kelpie dropped, each with why
        #[serde(skip_serializing_if = "Vec::is_empty")]
        dropped: Vec<String>,
        /// Its message to the maintainer
        #[serde(skip_serializing_if = "Option::is_none")]
        reply: Option<String>,
        /// Why, in its words
        #[serde(skip_serializing_if = "Option::is_none")]
        why: Option<String>,
        /// What its session has cost so far, in US dollars
        session_cost_usd: Option<f64>,
        /// The tokens its context held at the call's end
        context: Option<u64>,
    },
    /// The project manager's pick was dropped before dispatch took it, so the
    /// board rule picks
    PickDropped {
        /// The issue it picked
        issue: TicketId,
        /// Why it was dropped
        why: String,
    },
    /// The project manager's call failed or its reply held no answer, so
    /// the board rule picks and stuck items keep their rulings for now
    PmFailed {
        /// Why
        reason: String,
    },
    /// The project manager's session was compacted, its context past the mark
    PmCompacted {
        /// Its session
        session: SessionId,
    },
    /// A call started in a session, before it ends: what `shep kelpie watch`
    /// reads to tell whose a transcript is and where to find it
    SessionStarted {
        /// The work item's ticket, or none for the project manager
        #[serde(skip_serializing_if = "Option::is_none")]
        issue: Option<TicketId>,
        /// The role the call is made for
        role: Role,
        /// The agent it runs on
        agent: String,
        /// The harness it runs on, as settings name it
        harness: &'static str,
        /// The session it runs in, new or resumed
        session: SessionId,
        /// Where the harness writes the session's transcript, if kelpie can
        /// read it
        #[serde(skip_serializing_if = "Option::is_none")]
        transcript: Option<Transcript>,
    },
    /// A worker's turn resumed a session its harness could not find, so the
    /// turn starts over in a session of the same id
    ResumeLost {
        /// The work item's ticket
        issue: TicketId,
        /// The harness it runs on, as settings name it
        harness: &'static str,
        /// The session it tried to resume
        session: SessionId,
        /// Whether the session's transcript file was there, as kelpie looks
        /// for it, when the harness said it was not
        transcript: bool,
    },
    /// A worker's session was compacted before its next turn, its context
    /// past the mark
    WorkerCompacted {
        /// The work item's ticket
        issue: TicketId,
        /// Its session
        session: SessionId,
        /// The tokens its context held before
        context: Option<u64>,
    },
    /// A worker's session could not be compacted, so its next turn goes on
    /// in the session as it was
    WorkerCompactFailed {
        /// The work item's ticket
        issue: TicketId,
        /// Why
        reason: String,
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

/// What a step found there was to do, before a call it names is started
pub(super) enum Begin {
    Idle,
    Report(StepReport),
    /// A worker's turn, ended at `deadline` if it runs that long. `first`
    /// is its work item's first, which has no session to keep. `compact`
    /// is a `/compact` of its session, sent before the turn that waits.
    Call {
        call: AgentCall,
        deadline: Timestamp,
        first: bool,
        compact: bool,
    },
    Review(ReviewCall),
    /// A call on a contribution's head, its security review or the pass over
    /// it, which takes a slot while it runs
    Secure(super::contributions::Job),
    /// The triager's research of an untouched issue, which takes a slot while it runs
    Triage(Box<super::flight::triage::Job>),
    /// A read the step needs, which the rotation makes with the runner's lock let go before it
    /// steps the work item again
    Reads(Box<super::turn::reads::Job>),
}

/// A review call, which runs in flight like a turn
#[expect(
    clippy::large_enum_variant,
    reason = "one is made a review round; boxing either variant leaves the other the large one"
)]
pub(super) enum ReviewCall {
    /// One local round, of the project's kind
    Local {
        local: LocalRound,
        repo: PathBuf,
        worktree: PathBuf,
        base: String,
        out: PathBuf,
        round: u32,
        criteria: String,
    },
    /// A fresh session of a reviewer, its first look or its second
    Session(AgentCall),
}

/// What a [`ReviewCall`] cost, for the work item's record
pub(super) enum Spent {
    /// A session's call that came back, in `session`
    Claude {
        role: Role,
        session: SessionId,
        usage: Usage,
        session_cost: Option<Cost>,
    },
    /// A local round that ran, however it ended
    Local,
    /// A session's call that reached its model and ended, as given, with
    /// no usage reported
    Unanswered(crate::usage::Ended),
}

/// What a [`ReviewCall`] came back with, and what it spent
pub(super) struct Reviewed {
    pub result: ReviewResult,
    /// `None` when nothing ran to the end: a stopped call, or a Claude call
    /// that failed and so reported no cost
    pub spent: Option<Spent>,
    /// How a local round ended, where the reviewer said; `None` for any other call
    pub ended: Option<RoundEnd>,
}

impl Reviewed {
    /// A call that came back with `reason` and ran no round
    pub fn failed(reason: String) -> Self {
        Self {
            result: ReviewResult::Findings(Err(reason)),
            spent: None,
            ended: None,
        }
    }
}

/// What a [`ReviewCall`] came back with
pub(super) enum ReviewResult {
    /// A round's raw findings, from a local round or a reviewer's session
    Findings(Result<Vec<Finding>, String>),
    /// The call was ended because the runner is stopping, before it came
    /// back with anything
    Stopped,
}
