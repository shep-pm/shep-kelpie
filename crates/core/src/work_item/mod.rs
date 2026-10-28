//! The work item in flight, as the state file keeps it, keyed by its ticket id

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agents::Tier;
use crate::ports::{Cost, Finding, Role, SessionId, Timestamp, Usage};
use crate::random;
use crate::review_bot::Bot;
use crate::worktree::Remote;
use crate::{settings::AgentName, ticket::TicketId};

mod bots;
mod disputes;
mod follow_ups;
mod heads;
mod known;
mod local;
mod review;
mod seat;
mod spend;
mod talked;
mod tally;
mod threads;
mod timings;

pub use bots::BotSkipped;
pub use disputes::Dispute;
pub use follow_ups::FollowUps;
pub use known::{Known, foreign_change};
pub use local::LOCAL_FAILURES_DOWN;
pub use review::{Awaited, Reading, Review, ReviewStage, TestCheck};
pub use seat::Seat;
pub use spend::{Priced, QwenTally, RoleSpend, Spend, raised_cap};
pub use talked::{TALK_ROUNDS, Talked};
pub use tally::{CiFailures, Counts, RoleTally, Tally};
pub use threads::ThreadAnswer;
pub use timings::{CallKind, Seconds, Split, TimingPhase, Timings};

/// The work item in flight
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkItem {
    /// The ticket it resolves
    pub issue: TicketId,
    /// The ticket's title when it was added
    pub title: String,
    /// Its branch, cut from `origin/<base>` unless it is a rework
    pub branch: String,
    /// The project's base branch when it opened, which it is cut from, opened
    /// against and merged into whatever `git.base` says later. None in a file
    /// from before it was recorded, which the runner that loads it settles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Whether it reworks a pull request kelpie opened before, from its
    /// latest review. Its worktree starts at the branch's head on `origin`.
    #[serde(default)]
    pub rework: bool,
    /// Whether it adopts an open pull request kelpie didn't open. Its
    /// worktree starts at the branch's head on `origin`, and kelpie never
    /// rewrites the branch's commits.
    #[serde(default)]
    pub adopted: bool,
    /// Where its branch lives: `origin`, or the contributor's fork for an
    /// adopted contribution. Then `branch` is the local branch, and the
    /// fork's own name for it is in the remote.
    #[serde(default, skip_serializing_if = "Remote::is_origin")]
    pub remote: Remote,
    /// The login of the contributor whose released pull request this adopted
    /// work item is, left out for any other. Kelpie tells them when it fixes
    /// red CI on their branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contributor: Option<String>,
    /// An adopted pull request's head as it arrived, which its review diffs
    /// against instead of `origin/main`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrived: Option<String>,
    /// The listed review bots that owe an adopted pull request a review
    /// kelpie summoned. Until a bot's lands, a review from before the
    /// adoption does not stand for that bot's read.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub summons_owed: BTreeSet<Bot>,
    /// An older file's mark that the listed review bots owe an adopted pull
    /// request a summon, which the runner's start reads into `summons_owed`
    #[serde(default, skip_serializing)]
    pub summon_owed: bool,
    /// Whether a work item from a file older than review bots in the pass,
    /// past its review and not yet read by a bot, owes the listed bots a
    /// pass before its merge, which CI's next green run starts
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bots_after_ci: bool,
    /// Whether kelpie caught the branch up with `main` since a review bot
    /// last answered a summon. A bot finds nothing new in a caught-up branch,
    /// so the next summon asks it for a full review.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rebased: bool,
    /// Whether it holds one of the slots `concurrency.active_items` sets, which a phase
    /// that calls a model needs
    #[serde(default, skip_serializing_if = "Seat::is_held")]
    pub seat: Seat,
    /// Rounds in a row that left files unreviewed, by the local reviewer that
    /// ran them: ones that reviewed nothing, and ones that left the same
    /// files unreviewed again. Whatever the cause, the script's own
    /// `not reviewed:` lines are all kelpie sees. The first failure of a
    /// At [`LOCAL_FAILURES_DOWN`] the review goes on without the reviewer
    /// for the rest of the work item. A round that left no file, or only new
    /// ones, unreviewed clears its reviewer's count.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub local_failures: BTreeMap<AgentName, u32>,
    /// The files the last local round that reviewed anything left
    /// unreviewed. The next local round replaces the list with what it leaves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_unreviewed: Vec<String>,
    /// The local reviewer whose round left `local_unreviewed`. Only that
    /// reviewer leaving the same files again counts against it: another
    /// reviewer's first miss on them is its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_unreviewed_by: Option<AgentName>,
    /// Its worktree
    pub worktree: PathBuf,
    /// Its worker's build folder
    pub build: PathBuf,
    /// The implementer its worker runs on, chosen when it opened and kept
    /// until it ends. Each turn runs that agent's file as it then stands.
    pub agent: AgentName,
    /// Whether its issue's `agent:` label ends in `!`, or the project
    /// manager's pick named `agent`, which keeps it on `agent`: its first
    /// turn never falls back to another implementer
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// The kind its issue's one `kind:` label named when it opened, as `fix`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The least tier the project manager's pick said it needs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    /// The minutes past its quickest finish the project manager's pick allows it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack: Option<u32>,
    /// The worker's session, chosen before its first turn
    pub session: SessionId,
    /// Where the worker's turn stands
    pub turn: Turn,
    /// The draft pull request its worker opened, once kelpie has seen it
    pub pull_request: Option<u64>,
    /// Where it stands between the worker's turns and the merge
    #[serde(default)]
    pub phase: Phase,
    /// The head whose red CI run last went to the worker
    #[serde(default)]
    pub red_head: Option<String>,
    /// The head whose failed CI jobs kelpie reran, because their logs named no
    /// file the branch changed. A head is rerun once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reran: Option<String>,
    /// The head a yes to a `no-checks` ruling passed as skipped. While it is
    /// the head, no check on it reads as `Checks::Skipped`; a push is another
    /// head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unchecked: Option<String>,
    /// The head whose failed CI kelpie has already counted in
    /// `kelpie_ci_failures_total`. A head is counted once whatever the gate did
    /// with it after: a fix turn, a rerun, or a still-red ruling a `yes` sent
    /// back to CI at the same head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counted_head: Option<String>,
    /// The conflict with `main` that last went to the worker
    #[serde(default)]
    pub conflict: Option<Conflict>,
    /// What phase to force once the turn now running ends, overriding the
    /// ordinary rule that a known pull request goes straight to CI. Set by
    /// a ruling's answer that needs the review to run again, or by a rework;
    /// cleared once applied.
    #[serde(default)]
    pub resume: Option<Phase>,
    /// Whether a review call is in flight
    #[serde(default)]
    pub review_call: ReviewCallState,
    /// The reads each review bot has made of its pull request, which a
    /// bot's `rounds` limits
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bot_reads: BTreeMap<Bot, u32>,
    /// The listed review bots this pass went on without, and why
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bots_skipped: Vec<BotSkipped>,
    /// The forge's ids of the review bot threads sent to the worker, in the
    /// order its findings file lists them, which kelpie replies on once the
    /// fix turn ends, and resolves when the worker fixed them
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads_sent: Vec<String>,
    /// What the worker's reply said of `threads_sent`, which kelpie replies
    /// with: a thread it did not mention has none
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thread_answers: Vec<ThreadAnswer>,
    /// Steps in a row that could not answer `threads_sent`
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resolve_failures: u32,
    /// The forge's ids of the review bot threads the maintainer merged past
    /// with a yes, which hold no later merge of this work item
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads_accepted: Vec<String>,
    /// The reviewers whose calls failed so often in a row that a pass went
    /// on without them
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewers_skipped: Vec<AgentName>,
    /// Why the last review pass ended with no reviewer having read the pull
    /// request, as a listed reviewer was down or failed. None once a later
    /// pass has a reviewer read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreviewed: Option<String>,
    /// The pull request's labels, ready state and head, as kelpie and its
    /// worker leave them. A mismatch at the gate is a change kelpie did not make.
    #[serde(default)]
    pub known: Known,
    /// The heads a review round read, oldest first
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewed_heads: Vec<String>,
    /// The heads kelpie sends to CI with no round's read, on purpose, oldest
    /// first: a fix turn's push, a pass the project's own list gives nobody
    /// to read, and kelpie's own catch-up of a head it vouches for
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sent_unread: Vec<String>,
    /// The heads a fix turn sent only nits pushed, oldest first. A bot that
    /// read the pull request before has its nits on one of these left open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nit_fix_heads: Vec<String>,
    /// The head a merge ruling's `no` was about, while the fix it asked for
    /// goes to CI with no reviewer's read. The next merge ruling, on another
    /// head no round read, says that head is the fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub noted_from: Option<String>,
    /// The head a review bot's late round read once the pass had ended, while
    /// its fix goes to CI with no reviewer's read. The next merge ruling, on
    /// another head no round read, says that head is the late round's fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub late_from: Option<String>,
    /// Which of the people's comments on the pull request kelpie has handled
    #[serde(default, skip_serializing_if = "Talked::is_empty")]
    pub talked: Talked,
    /// The head at which the maintainer accepted a change to `.claude` or
    /// `.mcp.json`. Later heads that leave those files as this one has them
    /// pass the gate, and so does a worktree holding this head's copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_files_accepted: Option<String>,
    /// Its qwen rounds so far
    #[serde(default)]
    pub qwen: QwenTally,
    /// How many times a ruling for going over its budget has doubled its cap
    #[serde(default, skip_serializing_if = "is_zero")]
    pub budget_raises: u32,
    /// How many times a turn that passed its ceiling while still touching its
    /// worktree was resumed with no ruling. A fourth ceiling raises one.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ceiling_resumes: u32,
    /// Whether the forge refused a merge under `auto` since the last
    /// ruling on one. A second refusal raises a `stuck` ruling for `merge-refused`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub merge_refused: bool,
    /// Whether a worker turn already ended with no pull request and no
    /// question, and kelpie sent the worker back once. The next such turn
    /// parks it on a ruling. Cleared when a question the worker asked is
    /// answered.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sent_back: bool,
    /// Whether a worker turn ended with uncommitted files and nothing
    /// pushed, and kelpie sent the worker back once to commit them. Cleared
    /// when the next turn ends, so a later turn gets its own chance.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub asked_to_commit: bool,
    /// The head the project's `handover` hooks last passed, so a turn that
    /// ends on the same head runs them no more
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed_over: Option<String>,
    /// Whether a `handover` hook refused the branch and kelpie sent the
    /// worker back once to fix it. The next refusal parks the work item on
    /// a ruling. Cleared when a hook passes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub handover_refused: bool,
    /// Why the forge refused the pull request kelpie tried to open from the
    /// worker's `pull-request.md` (the guard, a `publish` hook or the forge
    /// itself), with how to fix the file where the worker can, until the next
    /// step tells the worker. Kept in the state, so a restart still names the
    /// refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_refused: Option<String>,
    /// The head of the last merge under `auto` that answered an error. A
    /// pull request later found merged at it is kelpie's merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_tried: Option<String>,
    /// The merge queue is holding this pull request, from the pass that
    /// queued it until it merges or the queue removes it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_queued: Option<MergeQueued>,
    /// Every finding above a nit kelpie has sent the worker to fix, across
    /// rounds. Only one of these is filed when the worker defers it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<Finding>,
    /// The findings a fix turn disputed in this pass, with the worker's
    /// reasons. A finding in here that is raised again cannot be disputed
    /// again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disputes: Vec<Dispute>,
    /// The comment on the pull request that carries the disputes no
    /// reviewer followed, by the forge's id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disputes_comment: Option<u64>,
    /// What kelpie ran of the tests reviewers attached to findings, before
    /// and after the fix, oldest first
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_checks: Vec<TestCheck>,
    /// The findings the worker left unfixed, read once the pull request
    /// merges. None until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_ups: Option<FollowUps>,
    /// The worker's last turn's closing message, on one line and cut short
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The tokens the worker's session held at its last turn's last model
    /// request, where its harness reports it. Unknown after a compaction,
    /// until the next turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    /// Where its wall time went, from the runner that first loaded it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<Timings>,
    /// The maintainer's `attach`, which holds it: no call starts for it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached: Option<Attached>,
    /// Its review rounds, fix turns and rulings so far
    #[serde(default, skip_serializing_if = "Counts::is_zero")]
    pub counts: Counts,
    /// Every Claude call made for it, oldest first
    pub calls: Vec<CallRecord>,
}

/// The `attach` holding a work item, which holds while either of its
/// processes runs
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attached {
    /// The `attach` command itself
    pub by: Holder,
    /// The session it runs in the terminal, once it has started
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Holder>,
    /// When it asked
    pub since: Timestamp,
}

/// A process, named so that another given its pid later is not it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Holder {
    /// Its pid
    pub pid: u32,
    /// When it started, as `ps -o lstart=` prints it
    pub started: String,
}

/// A pull request kelpie put in the merge queue
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeQueued {
    /// How many removals the queue had made of it before kelpie queued it
    pub removals: u32,
    /// When kelpie queued it
    pub since: Timestamp,
}

/// A conflict with `main` that went to the worker as its next turn
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conflict {
    /// The branch's head when it conflicted
    pub head: String,
    /// The `origin/main` commit it conflicted with
    pub main: String,
    /// How many conflict turns this work item has had, this one included
    pub turns: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Where a work item stands between the worker's turns and the merge
///
/// A work item saved before phases existed reads as [`Phase::Implement`],
/// so a restart never puts its pull request through the gate unasked.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Phase {
    /// The worker's turns. One that ends with a pull request open starts the
    /// review.
    #[default]
    Implement,
    /// The review, between the draft pull request and CI
    Review(Review),
    /// Waiting for CI on the pull request's head
    Ci {
        /// The head kelpie last saw, once it has looked
        head: Option<String>,
        /// When kelpie first saw that head, or entered CI
        since: Timestamp,
    },
    /// Parked on a ruling
    Ruling {
        /// The ruling's id: its pull request's number, or the ticket's id
        /// before there is one
        id: TicketId,
    },
    /// The maintainer said yes, or every gate passed under `auto`: merging this head
    Merge {
        /// The head the ruling, or the gate, was about
        head: String,
        /// When kelpie marked the draft ready, which can start a fresh CI run
        #[serde(default)]
        readied: Option<Timestamp>,
        /// Whether the gate started it under `auto`, with no ruling asked
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        auto: bool,
    },
    /// Removing the worktree, branch and build folder
    Done {
        /// Whether the pull request merged, so its branch on the forge goes too
        merged: bool,
        /// Whether its issue was closed while its branch held no commit, so
        /// it ends with no pull request and nothing to keep
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        closed: bool,
    },
}

/// Whether a review call is in flight
///
/// Recorded in state the way a running turn is: `drop` refuses while a
/// call is running, since it runs outside the runner's lock and a dropped
/// work item would leave nothing for the result to land on.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ReviewCallState {
    /// Nothing is running
    #[default]
    Idle,
    /// A review call is running, started at this time
    Running {
        /// When it started
        since: Timestamp,
    },
}

impl WorkItem {
    /// A work item whose first turn is due, with nothing recorded yet
    ///
    /// Everything else is what a work item starts with. The caller sets
    /// `timings`, the one thing a clock gives.
    pub fn new(
        issue: TicketId,
        title: impl Into<String>,
        branch: impl Into<String>,
        worktree: PathBuf,
        build: PathBuf,
        agent: AgentName,
        session: SessionId,
    ) -> Self {
        Self {
            issue,
            title: title.into(),
            branch: branch.into(),
            base: None,
            rework: false,
            adopted: false,
            remote: Remote::Origin,
            contributor: None,
            arrived: None,
            worktree,
            build,
            agent,
            pinned: false,
            kind: None,
            tier: None,
            slack: None,
            session,
            turn: Turn::Due,
            pull_request: None,
            phase: Phase::default(),
            red_head: None,
            reran: None,
            unchecked: None,
            counted_head: None,
            conflict: None,
            resume: None,
            review_call: ReviewCallState::default(),
            bot_reads: BTreeMap::new(),
            bots_skipped: Vec::new(),
            known: Known::default(),
            reviewed_heads: Vec::new(),
            sent_unread: Vec::new(),
            nit_fix_heads: Vec::new(),
            noted_from: None,
            late_from: None,
            talked: Talked::default(),
            claude_files_accepted: None,
            qwen: QwenTally::default(),
            budget_raises: 0,
            ceiling_resumes: 0,
            merge_refused: false,
            sent_back: false,
            asked_to_commit: false,
            handed_over: None,
            handover_refused: false,
            pull_request_refused: None,
            context: None,
            merge_tried: None,
            merge_queued: None,
            summon_owed: false,
            summons_owed: BTreeSet::new(),
            bots_after_ci: false,
            seat: Seat::default(),
            threads_sent: Vec::new(),
            thread_answers: Vec::new(),
            resolve_failures: 0,
            threads_accepted: Vec::new(),
            reviewers_skipped: Vec::new(),
            unreviewed: None,
            local_failures: BTreeMap::new(),
            local_unreviewed: Vec::new(),
            local_unreviewed_by: None,
            rebased: false,
            held: Vec::new(),
            disputes: Vec::new(),
            disputes_comment: None,
            test_checks: Vec::new(),
            follow_ups: None,
            summary: None,
            timings: None,
            attached: None,
            counts: Counts::default(),
            calls: Vec::new(),
        }
    }

    /// Forgets the review bot threads sent to the worker, which no fix of
    /// this round will resolve now
    pub fn forget_threads(&mut self) {
        self.threads_sent.clear();
        self.thread_answers.clear();
        self.resolve_failures = 0;
    }

    /// Forgets what the last pass made of the review bots, as a new one
    /// starts, and the head of a late round's fix, which the pass reads
    pub fn new_pass(&mut self) {
        self.forget_threads();
        self.bots_skipped.clear();
        self.late_from = None;
        self.disputes.clear();
        self.disputes_comment = None;
    }

    /// Remembers findings sent to the worker, once each, but for nits: a
    /// nit the worker leaves is never filed as a follow-up
    pub fn record_held(&mut self, held: &[Finding]) {
        for finding in held.iter().filter(|f| !f.is_nit()) {
            if !self.held.iter().any(|known| known.is_same_as(finding)) {
                self.held.push(finding.clone());
            }
        }
    }

    /// Records the head kelpie's own catch-up with `main` pushed, which no
    /// review bot has read
    pub fn caught_up(&mut self, head: Option<String>) {
        self.known.head = head;
        self.rebased = true;
    }

    /// The commit its review diffs against: `origin/` and the project's
    /// `base` branch, or the head an adopted pull request arrived with, so
    /// none of that is reviewed
    pub fn review_base(&self, base: &str) -> String {
        self.arrived
            .clone()
            .unwrap_or_else(|| format!("origin/{base}"))
    }

    /// What its calls have cost so far
    pub fn cost(&self) -> Cost {
        Cost(self.calls.iter().map(|c| c.cost.0).sum())
    }

    /// Moves what `session` had cost as of its last recorded call up to `to`,
    /// for a call in it that the work item does not count, so the cost of the
    /// next call here leaves that one out
    pub fn advance_session_cost(&mut self, session: &SessionId, to: Cost) {
        let last = self.calls.iter_mut().rev().find(|c| &c.session == session);
        if let Some(last) = last.filter(|c| c.session_cost.0 < to.0) {
            last.session_cost = to;
        }
    }

    /// What `session` had cost as of its last recorded call
    pub fn session_cost(&self, session: &SessionId) -> Cost {
        let last = self.calls.iter().rev().find(|c| &c.session == session);
        last.map_or(Cost(0), |c| c.session_cost)
    }
}

/// A random version 4 UUID, which `claude --session-id` takes
pub fn new_session_id() -> io::Result<SessionId> {
    let mut b = [0u8; 16];
    random::fill(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let (a, rest) = hex.split_at(8);
    let (b, rest) = rest.split_at(4);
    let (c, rest) = rest.split_at(4);
    let (d, e) = rest.split_at(4);
    Ok(SessionId(format!("{a}-{b}-{c}-{d}-{e}")))
}

/// Where the worker's turn stands
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase", deny_unknown_fields)]
pub enum Turn {
    /// The first turn waits for the project to run
    Due,
    /// A turn started and has not ended; after a restart, it is resumed
    Running {
        /// When it started
        since: Timestamp,
        /// The prompt a resumed turn was sent, which a turn that failed
        /// before the harness started never saw. A first turn has none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// A later turn waits to resume the session with this prompt
    Next {
        /// What the turn tells the worker
        prompt: String,
    },
    /// The turn ended and the worker waits for kelpie
    Ended {
        /// When it ended
        at: Timestamp,
    },
    /// The turn could not run, and waits for the maintainer
    Failed {
        /// When it failed
        at: Timestamp,
        /// Why
        reason: String,
    },
}

/// One Claude call made for a work item
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallRecord {
    /// The role it was made for
    pub role: Role,
    /// When it ended
    pub at: Timestamp,
    /// The session it ran in
    pub session: SessionId,
    /// What it used
    pub usage: Usage,
    /// What it cost: the change in its session's cost
    pub cost: Cost,
    /// What its session had cost when it ended
    pub session_cost: Cost,
    /// Whether its harness reported no cost, so `cost` is no measure of it
    #[serde(default, skip_serializing_if = "is_false")]
    pub unpriced: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

#[cfg(test)]
mod tests;
