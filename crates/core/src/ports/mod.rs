//! The runner's ports: agents, the forge, the account's usage, the
//! maintainer's webhook and the clock
//!
//! The work-item loop reaches the outside world only through these traits.
//! The adapters crate holds the real ones and the test rig holds stand-ins,
//! so a test sees exactly the calls the runner makes.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::board::OpenPullRequest;
use crate::lease::LeaseKind;
use crate::lease::wire::{AskerCarry, WindowFact};
use crate::review_bot::{Activity, Login, Profile, ReviewWindow};
use crate::settings::{Account, ForgeSlug};
use crate::ticket::TicketId;
use crate::webhook::Webhook;

mod agent;
mod attachment;
mod bridge;
mod classifier;
mod dev_server;
mod disk;
mod e2e_runner;
mod findings;
mod forge_error;
mod gateway;
mod git;
mod gpu;
mod harness_mcp;
mod installation_token;
mod issue;
mod keychain;
mod leased;
pub mod local_paths;
pub mod mcp;
mod metrics;
mod pace;
pub mod paced_voice;
mod pages;
pub mod rate_limit;
pub mod recorded_voice;
mod revalidated;
mod reviewed;
mod reviewer;
mod routed;
mod run;
mod sandbox;
mod system_clock;
mod talk;
mod test_runner;
mod thread;
mod tickets;
mod voice;

pub use agent::{
    Adopted, AgentCall, AgentError, AgentReply, Agents, Browser, COMPACT, CallActivity, CodeGraph,
    Cost, Ending, Fence, Flying, Guard, Kept, KeptServer, NotAdopted, PM_NOTES, Reach, Role,
    Session, SessionId, Tools, Transcript, Usage, Wait, written_at,
};
pub use attachment::is_github_attachment;
pub use bridge::{Bridges, OpenBridge, Playwright};
pub use classifier::{Classifier, ClassifierError, Probability, SpamState};
pub use dev_server::{DevPort, DevRun, DevServers, READY_WITHIN, Started};
pub use disk::Disk;
pub use e2e_runner::{E2eOutcome, E2eRun, E2eRunner};
pub use findings::{CLEAN, Finding, Severity, is_test_path, parse_findings, read_review};
pub use forge_error::{Cooldown, ForgeError};
pub use gateway::{KeyCheck, ModelGateway, Stand, Taking, Unheld};
pub use git::{Git, GitError};
pub use gpu::{Gpu, GpuError, GpuMetrics};
pub use harness_mcp::HarnessMcp;
pub use installation_token::InstallationToken;
pub use issue::{
    AuthorAssociation, FinishedParent, Issue, IssueCloseReason, OpenIssue, OpenPull, OpenTicket,
    SubIssue, SubIssueState,
};
pub use keychain::KeychainBlocked;
pub use leased::{LocalLeases, ModelLoad, PaddockHolder};
pub use local_paths::{Guarded, GuardedVoice, with_comments_in_body};
pub use metrics::{Sample, SeriesKind, Sink};
pub use pace::{AT_ONCE, CallPacer, LocalPace, PER_MINUTE, Pace, Pass};
pub use paced_voice::PacedVoice;
pub use pages::{ConsoleError, PageRequest, PageSession, Pages, Visit};
pub use rate_limit::{
    AppForges, AppReads, ForgeHold, RateHeld, Reading, Say, Warnings, says_used_up,
};
pub use recorded_voice::RecordedVoice;
pub use revalidated::Revalidated;
pub use reviewed::{MaintainerReview, ReviewComment, Reviewed};
pub use reviewer::{Reviewer, ReviewerError, RoundEnd, RoundStage};
pub use routed::Routed;
pub use run::{BranchRun, FailedLogs, Workflow, WorkflowState};
pub use sandbox::{Forward, Policy, Sandbox, SandboxError, Unreadable};
pub use system_clock::SystemClock;
pub use talk::{Remark, Standing, Talk, TalkThread};
pub use test_runner::{CEILING, OUTPUT_KEPT, TestOutcome, TestRun, TestRunner, tail};
pub use thread::{CommentMarks, LineComment, PostedComment, ThreadComment, Thumb};
pub use tickets::Tickets;
pub use voice::{UNREAD_ID, Voice};

/// Seconds since the Unix epoch
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub u64);

/// Tells the time
pub trait Clock: Send + Sync {
    /// The current time
    fn now(&self) -> Timestamp;

    /// Lets `seconds` pass
    fn wait(&self, seconds: u64) {
        std::thread::sleep(std::time::Duration::from_secs(seconds));
    }
}

/// Who a repo is visible to on the forge
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Anyone
    Public,
    /// Only people given access
    Private,
    /// Members of the owning organisation
    Internal,
}

/// The forge: the project's pull requests, their CI, and the repo's facts
///
/// A project's tickets are not here. The runner reads a ticket and writes to
/// it through [`Tickets`] (ADR 0014). What kelpie posts stays here: only
/// [`Forge::comment`] records the id of a comment, on an issue or a pull
/// request alike.
pub trait Forge: Send + Sync {
    /// Who can see `repo`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn visibility(&self, repo: &ForgeSlug) -> Result<Visibility, ForgeError>;

    /// The branch `repo`'s pull requests merge into by default
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn default_branch(&self, repo: &ForgeSlug) -> Result<String, ForgeError>;

    /// Whether `repo`'s rules require a pull request's branch to be up to date
    /// with `base` before it may merge
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn requires_up_to_date(&self, repo: &ForgeSlug, base: &str) -> Result<bool, ForgeError>;

    /// Whether `repo` has a branch named `branch`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn branch_exists(&self, repo: &ForgeSlug, branch: &str) -> Result<bool, ForgeError>;

    /// What folder `path` of `repo` holds on its default branch, where an
    /// empty `path` is the root, or `None` when `repo` or the folder is not there
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn folder(&self, repo: &ForgeSlug, path: &str) -> Result<Option<Vec<FolderEntry>>, ForgeError>;

    /// The text of file `path` of `repo` on its default branch
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked, has no such file, or
    /// its answer is not text.
    fn file_text(&self, repo: &ForgeSlug, path: &str) -> Result<String, ForgeError>;

    /// The names of the labels `repo` has
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn repo_labels(&self, repo: &ForgeSlug) -> Result<Vec<String>, ForgeError>;

    /// Makes `label` on `repo`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses, such as when `repo` already has it.
    fn create_label(&self, repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError>;

    /// Whether the account kelpie acts as may push to `repo`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn can_push(&self, repo: &ForgeSlug) -> Result<bool, ForgeError>;

    /// When the pull request the review bot `login` last reviewed on `repo` was
    /// last updated, or `None` when it has reviewed none
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn last_bot_review(
        &self,
        repo: &ForgeSlug,
        login: Login<'_>,
    ) -> Result<Option<Timestamp>, ForgeError>;

    /// The open pull requests on `repo`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn open_pull_requests(&self, repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError>;

    /// Pull request `number` on `repo`: its state, head and checks
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked, has no such pull
    /// request, or its answer cannot be read.
    fn pull_request(&self, repo: &ForgeSlug, number: u64) -> Result<PullRequest, ForgeError>;

    /// Pull request `number` on `repo` as a rework or an adoption reads it:
    /// its branch, what it closes and the maintainer's latest review
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked, has no such pull
    /// request, or its answer cannot be read.
    fn reviewed(&self, repo: &ForgeSlug, number: u64) -> Result<Reviewed, ForgeError>;

    /// What people wrote on pull request `number`: its comments and its
    /// review threads, resolved ones included
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked, has no such pull
    /// request, or its answer cannot be read.
    fn talk(&self, repo: &ForgeSlug, number: u64) -> Result<Talk, ForgeError>;

    /// The login of the account kelpie acts as, which opens its pull requests
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn viewer(&self) -> Result<String, ForgeError>;

    /// When the account `login` was created, for telling a new account from an
    /// established one
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked, has no such account, or
    /// its answer cannot be read.
    fn account_created(&self, login: &str) -> Result<Timestamp, ForgeError>;

    /// The comments written on any issue or pull request of `repo` since
    /// `since`, oldest first
    ///
    /// The forge counts a comment since its last edit, so one written before
    /// `since` and edited after it is among them: [`ThreadComment::created`]
    /// says when it was written.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn comments_since(
        &self,
        repo: &ForgeSlug,
        since: Timestamp,
    ) -> Result<Vec<ThreadComment>, ForgeError>;

    /// The comments in the review threads of pull request `number` since
    /// `since`, oldest first. Their ids are numbered apart from issue comments'.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked, has no such pull
    /// request, or its answer cannot be read.
    fn thread_replies_since(
        &self,
        repo: &ForgeSlug,
        number: u64,
        since: Timestamp,
    ) -> Result<Vec<ThreadComment>, ForgeError>;

    /// Whether `login` has write access to `repo`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn can_write(&self, repo: &ForgeSlug, login: &str) -> Result<bool, ForgeError>;

    /// The thumbs on comment `comment` of `repo`, what it says now, and who
    /// edited it
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn comment_marks(&self, repo: &ForgeSlug, comment: u64) -> Result<CommentMarks, ForgeError>;

    /// Posts `body` as a comment on issue or pull request `number`, as the
    /// account kelpie acts as, and returns its id
    ///
    /// The runner notes the id: a comment of the maintainer's own login that
    /// kelpie posted is never an answer to a ruling.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the comment cannot be posted or its id read.
    fn comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<u64, ForgeError>;

    /// The open pull requests on `repo`, each with its title and the issues its
    /// body names, for telling one that already covers an issue
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer cannot be read.
    fn open_pulls(&self, repo: &ForgeSlug) -> Result<Vec<OpenPull>, ForgeError>;

    /// Closes pull request `number` without merging it, with no comment
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn close_pull_request(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError>;

    /// Opens a draft pull request on `repo` from branch `head` into `base`,
    /// as the account kelpie acts as, and returns its number
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses, or its answer names no number.
    fn create_pull_request(
        &self,
        repo: &ForgeSlug,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<u64, ForgeError>;

    /// Sets the title and body of pull request `number`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn edit_pull_request(
        &self,
        repo: &ForgeSlug,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<(), ForgeError>;

    /// The end of the logs of the CI runs that failed on `head`, or on the
    /// merge queue when `merge_group`, for a worker whose token cannot read CI
    ///
    /// Where more runs failed than kelpie reads logs of, the answer says how
    /// many it left out.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn failed_run_logs(
        &self,
        repo: &ForgeSlug,
        head: &str,
        merge_group: bool,
    ) -> Result<FailedLogs, ForgeError>;

    /// The latest completed CI run of each workflow and event among the 50
    /// newest completed runs on `branch`, whatever its conclusion
    ///
    /// A workflow whose latest run is older than those 50 is absent, so an
    /// absent workflow is not a green one. A branch named `main` on a fork
    /// has its pull requests' runs listed here too, under their own event.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn branch_runs(&self, repo: &ForgeSlug, branch: &str) -> Result<Vec<BranchRun>, ForgeError>;

    /// Every workflow of the repo, whatever its state
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read, or
    /// lists a state this port does not know.
    fn workflows(&self, repo: &ForgeSlug) -> Result<Vec<Workflow>, ForgeError>;

    /// The end of the failed jobs' log of CI run `run`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn run_log_tail(&self, repo: &ForgeSlug, run: u64) -> Result<String, ForgeError>;

    /// Reruns the failed jobs of the CI runs that failed on `head`, as the
    /// login the forge acts as
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked, or no run
    /// failed on `head`.
    fn rerun_failed(&self, repo: &ForgeSlug, head: &str) -> Result<(), ForgeError>;

    /// Reruns the jobs of CI run `run` that `rerun` names, as the login the
    /// forge acts as
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn rerun_run(&self, repo: &ForgeSlug, run: u64, rerun: Rerun) -> Result<(), ForgeError>;

    /// Approves the CI runs on `head` that wait for a maintainer to approve
    /// them, as the login the forge acts as: the first runs of a fork's
    /// pull request, which run the contributor's code once approved. Returns
    /// `Ok` when no run waits.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn approve_workflow_runs(&self, repo: &ForgeSlug, head: &str) -> Result<(), ForgeError>;

    /// Marks draft pull request `number` ready for review
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn mark_ready(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError>;

    /// Adds `label` to pull request `number`, or takes it off
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn set_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError>;

    /// What the review bot `login` has posted on pull request `number`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn review_bot(
        &self,
        repo: &ForgeSlug,
        number: u64,
        login: Login<'_>,
    ) -> Result<Activity, ForgeError>;

    /// Resolves review thread `thread`
    ///
    /// A thread already resolved, or one the forge no longer has, is done.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn resolve_thread(&self, repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError>;

    /// Replies to review thread `thread` with `body`, as the account kelpie
    /// runs as
    ///
    /// A thread the forge no longer has is done, since there is nothing to
    /// reply to.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn reply_to_thread(&self, repo: &ForgeSlug, thread: &str, body: &str)
    -> Result<(), ForgeError>;

    /// Merges pull request `number` with a merge commit, only while its head is `head`
    ///
    /// Never a squash or a rebase: the branch's history survives the merge.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses, the head moved, or it cannot be asked.
    fn merge(&self, repo: &ForgeSlug, number: u64, head: &str) -> Result<(), ForgeError>;

    /// Where pull request `number` stands in the repo's merge queue
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn merge_queue(&self, repo: &ForgeSlug, number: u64) -> Result<QueueStanding, ForgeError>;

    /// Disarms auto-merge on pull request `number`
    ///
    /// With a merge queue required, a merge call on a pull request that
    /// cannot be queued yet arms auto-merge, which later queues whatever
    /// head the branch has then.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn disable_auto_merge(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError>;

    /// The login's `core`, `graphql` and `search` rate limits as the forge counts them
    ///
    /// Asking spends none of them.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn rate_limit(&self) -> Result<RateLimits, ForgeError>;

    /// When the forge's used-up rate limits reset: the latest reset of the
    /// `core`, `graphql` or `search` limit with nothing left, or `None` when
    /// none is used up
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn rate_limit_reset(&self) -> Result<Option<Timestamp>, ForgeError> {
        Ok(self.rate_limit()?.used_up_until())
    }
}

/// One rate limit as the forge counts it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateBucket {
    /// The calls it has counted since it last reset
    pub used: u64,
    /// The calls it has left before it is used up
    pub remaining: u64,
    /// When it resets and starts counting again
    pub reset: Timestamp,
}

/// The limits the maintainer's login spends from
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimits {
    /// The REST API's
    pub core: RateBucket,
    /// The GraphQL API's
    pub graphql: RateBucket,
    /// The search API's, which `gh search` spends from
    pub search: RateBucket,
}

impl RateLimits {
    /// The latest reset of a limit with nothing left, or `None` when none is used up
    pub fn used_up_until(&self) -> Option<Timestamp> {
        [self.core, self.graphql, self.search]
            .iter()
            .filter(|bucket| bucket.remaining == 0)
            .map(|bucket| bucket.reset)
            .max()
    }
}

/// One entry of a folder on the forge
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEntry {
    /// The entry's name, without its folder
    pub name: String,
    /// Whether it is a folder and not a file
    pub is_dir: bool,
}

/// Where a pull request stands in the merge queue
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueStanding {
    /// Whether it is in the queue now
    pub queued: bool,
    /// Whether auto-merge is armed on it, to queue it once it can be
    pub armed: bool,
    /// How many times the queue has removed it, merged or not
    pub removals: u32,
    /// What the forge gave for the latest removal
    pub reason: Option<String>,
}

/// A pull request as the forge holds it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    /// Open, merged or closed
    pub state: PullRequestState,
    /// Whether it is still a draft
    pub draft: bool,
    /// Its head commit's hash
    pub head: String,
    /// Where CI stands on its head
    pub checks: Checks,
    /// Its labels' names
    pub labels: Vec<String>,
}

/// Whether a pull request is open, merged or closed without merging
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullRequestState {
    /// Open, draft or not
    Open,
    /// Merged
    Merged,
    /// Closed without merging
    Closed,
}

/// Where CI stands on a pull request's head
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checks {
    /// No check has reported on the head
    None,
    /// At least one check is still running or queued, and these check runs
    /// are still queued: none when every pending check has started
    Pending(Vec<Queued>),
    /// Every check finished, and none failed
    Passed,
    /// Every check finished and was skipped, as a workflow whose jobs all
    /// skip a draft leaves on one: nothing ran, and it counts as passed once
    /// the pull request is ready
    Skipped,
    /// Every check finished, and these failed
    Failed(Vec<String>),
}

/// A check run still queued or in progress, as the pull request's rollup lists it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    /// The check's name
    pub name: String,
    /// The workflow run it belongs to, when its link names one
    pub run: Option<u64>,
    /// It started and has not finished, so it ends at its own timeout. A
    /// queued one waits for a runner that may never come.
    pub running: bool,
}

/// What `rerun_run` starts again of a workflow run
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rerun {
    /// The jobs that failed
    Failed,
    /// Every job, as a run does whose job never started
    All,
}

/// A label kelpie makes on a project's repo
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewLabel<'a> {
    /// Its name
    pub name: &'a str,
    /// Its colour, as six lowercase hex digits with no `#`, as GitHub takes it
    pub color: &'a str,
    /// What it means, shown beside it on the forge
    pub description: &'a str,
}

/// How much of one usage window the account has spent
// wire format: changing this is a breaking change to the pacer's status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// Whole percent of the window used
    pub used_pct: u32,
    /// When the window resets
    pub resets_at: Timestamp,
}

/// The account's usage: the 5-hour session window and the weekly window
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Utilization {
    /// The 5-hour session window
    pub session: Window,
    /// The weekly window across all models
    pub week: Window,
}

/// Reads the account's usage
pub trait Meter: Send + Sync {
    /// The account's usage at `now`
    ///
    /// `now` places the reset times, which the account prints without a year.
    ///
    /// # Errors
    ///
    /// [`MeterError`] when usage cannot be read.
    fn read(&self, now: Timestamp) -> Result<Utilization, MeterError>;
}

/// Why the account's usage could not be read
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeterError {
    /// `claude` could not be started, with the OS's reason
    Spawn(String),
    /// `claude` did not answer in time
    TimedOut,
    /// The runner is stopping
    Stopped,
    /// `claude` answered, but not with usage kelpie can read
    Unreadable(String),
    /// `codex` could not be run, did not answer, or answered with no
    /// usage kelpie can read, with which
    Codex(String),
}

impl fmt::Display for MeterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codex(reason) => f.write_str(reason.trim()),
            Self::Spawn(error) => write!(f, "cannot run claude: {error}"),
            Self::TimedOut => f.write_str("claude did not answer /usage in time"),
            Self::Stopped => f.write_str("claude was stopped with the runner"),
            Self::Unreadable(output) => write!(f, "unreadable /usage output: {}", output.trim()),
        }
    }
}

impl core::error::Error for MeterError {}

/// One alert for the maintainer, away from the terminal
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    /// A one-line title naming the project and the ruling
    pub title: String,
    /// The ruling's question, with the triggers that answer it
    pub text: String,
    /// How the maintainer answers it where they read it, on a webhook that
    /// takes replies
    pub reply: Option<ReplyWith>,
}

/// The ruling a reply on the webhook's topic answers, and what it takes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyWith {
    /// The ticket a reply starts with: the ruling's pull request or issue
    pub ticket: TicketId,
    /// What answers it
    pub takes: Takes,
}

/// What answers a ruling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takes {
    /// The worker's question: an answer
    Answer,
    /// The merge ruling: a yes, or a no or a rework with a note
    Merge,
    /// The merge ruling, alerted while coded answers are off: a no or a
    /// rework with a note, and a yes only after the terminal turns them on
    MergeCodedOff,
    /// A yes, or a no with a note
    YesOrNo,
}

/// Where a read of the webhook's replies starts
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Since {
    /// Every message from this time on
    Time(Timestamp),
    /// Every message after the one with this id
    After(String),
}

/// One message on the webhook's topic
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// The webhook's id for it, which the next read starts after
    pub id: String,
    /// When the webhook took it, which the sender cannot set
    pub time: Timestamp,
    /// Its text, or `None` for kelpie's own posts and anything but plain text
    pub text: Option<String>,
    /// Every text it carries, its title included, whose codes are spent
    /// though only `text` can answer
    pub said: Vec<String>,
    /// Whether it carries text kelpie cannot read in full, as ntfy turns a
    /// long message into an attachment
    pub cut: bool,
}

/// Posts alerts to the maintainer's webhook, and reads replies to them
pub trait Alerts: Send + Sync {
    /// Posts `alert` to `webhook`
    ///
    /// # Errors
    ///
    /// [`AlertError`] when the post cannot be made or is refused. Its text
    /// never carries the webhook's URL.
    fn post(&self, webhook: &Webhook, alert: &Alert) -> Result<(), AlertError>;

    /// The messages on `webhook`'s topic since `since`, oldest first, on a
    /// webhook that takes replies
    ///
    /// # Errors
    ///
    /// [`AlertError`] when the read cannot be made, is refused, or cannot
    /// be understood. Its text never carries the webhook's URL.
    fn replies(&self, webhook: &Webhook, since: &Since) -> Result<Vec<Reply>, AlertError>;
}

/// Why an alert was not posted. None of these carry the webhook's URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertError {
    /// `curl` could not be started, with the OS's reason
    Spawn(String),
    /// `curl` could not reach the webhook, with its exit code
    Unreachable(i32),
    /// The webhook answered with this HTTP status, not a success
    Refused(u16),
    /// The webhook's replies came back in a shape kelpie cannot read
    Unreadable,
}

impl fmt::Display for AlertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run curl: {error}"),
            Self::Unreachable(code) => write!(f, "curl could not reach the webhook (exit {code})"),
            Self::Refused(status) => write!(f, "the webhook answered HTTP {status}"),
            Self::Unreadable => f.write_str("the webhook's replies could not be read"),
        }
    }
}

impl core::error::Error for AlertError {}

/// A runner's side of the dog's book leases
///
/// Asking never blocks: the dog grants later, and [`Leases::holds`] says
/// when it has. Each method only raises what the dog should hear.
pub trait Leases: Send + Sync {
    /// Asks for `kind`, or asks again for one already asked for, which
    /// covers an ask the shepherd dropped
    fn want(&self, kind: &LeaseKind);

    /// Whether this run holds `kind`
    fn holds(&self, kind: &LeaseKind) -> bool;

    /// What a handover carries so the next image counts on from this one's
    /// totals, or `None` for a book that keeps none
    fn keep(&self) -> Option<AskerCarry> {
        None
    }

    /// Gives `kind` back, or withdraws the ask
    fn give_back(&self, kind: &LeaseKind);

    /// Tells the dog what this run saw of `kind`'s review window
    fn window(&self, kind: &LeaseKind, fact: WindowFact, value: u64);

    /// When the dog's book says `kind`'s window, defined as `window`, opens
    /// after `now`, or `None` while it is open or the book cannot be read
    fn opens(&self, kind: &LeaseKind, window: ReviewWindow, now: Timestamp) -> Option<Timestamp>;
}

/// Every port the runner uses, as one bundle
pub struct Ports {
    /// The agents every role runs on, shared so a turn runs without holding the runner
    pub agents: Arc<dyn Agents>,
    /// The forge: the project's pull requests, their CI, and the repo's facts
    pub forge: Arc<dyn Forge>,
    /// The project's tickets, as their source holds them (ADR 0014). The
    /// runner sets this to the same wrapped source as `forge`, so a call
    /// through either reaches the same guard, rate-limit hold and ledger
    pub tickets: Arc<dyn Tickets>,
    /// Which repos the forge's reads go through the App's token for, where it has one
    pub app_reads: Option<Arc<dyn AppForges>>,
    /// The tally the forge's conditional reads add a 304 to, for the ledger
    pub revalidated: Revalidated,
    /// Kelpie's own voice on the forge, its GitHub App's where one covers the repo
    pub voice: Arc<dyn Voice>,
    /// The Claude account's usage, from `/usage`
    pub meter: Arc<dyn Meter>,
    /// The Codex account's usage
    pub codex_meter: Arc<dyn Meter>,
    /// The local round's runner, shared so a round runs without holding the runner
    pub reviewer: Arc<dyn Reviewer>,
    /// The locks a local agent's calls hold
    pub local_leases: Arc<dyn LocalLeases>,
    /// A profile for each review bot a round may summon
    pub review_bots: Vec<Arc<dyn Profile>>,
    /// The runner's own git, for the marketplaces a repo's plugins come from
    pub git: Arc<dyn Git>,
    /// The GPU's figures, for `status`
    pub gpu: Arc<dyn GpuMetrics>,
    /// The disk and the build folders on it, which a turn in flight is watched against
    pub disk: Arc<dyn Disk>,
    /// The maintainer's webhook, shared so a post runs without holding the runner
    pub alerts: Arc<dyn Alerts>,
    /// Runs a reviewer's test, inside the worker's sandbox
    pub test_runner: Arc<dyn TestRunner>,
    /// Starts a project's dev server for a worker turn that has the browser
    pub dev_servers: Arc<dyn DevServers>,
    /// Runs a bump's e2e tests on the dog's host
    pub e2e_runner: Arc<dyn E2eRunner>,
    /// Opens a web bump's pages in the local browser
    pub pages: Arc<dyn Pages>,
    /// Opens the bridge a worker's turn reaches its browser through
    pub bridges: Arc<dyn Bridges>,
    /// What each harness says of the MCP servers a call names
    pub harness_mcp: Arc<dyn HarnessMcp>,
    /// The dog's book leases, which the runner's `grant` trigger fills
    pub leases: Arc<dyn Leases>,
    /// Where the runner's metrics go, shep's metrics dog
    pub metrics: Arc<dyn Sink>,
    /// The classifier each project's gateway serves, for triage's spam check
    pub classifier: Arc<dyn Classifier>,
    /// The clock, shared with the forge's rate-limit hold
    pub clock: Arc<dyn Clock>,
    /// The login's forge hold and pace, kept by the dog where it is reached
    pub pace: Arc<dyn Pace>,
    /// The repo of every project in the flock, whose names a post to a public
    /// repo leaves out when GitHub marks them private
    pub flock: Vec<ForgeSlug>,
}

impl Ports {
    /// The meter that reads `account`'s usage
    pub fn meter_of(&self, account: Account) -> &Arc<dyn Meter> {
        match account {
            Account::Claude => &self.meter,
            Account::Codex => &self.codex_meter,
        }
    }
}

impl fmt::Debug for Ports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ports").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod findings_tests;
