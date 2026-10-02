//! The runner's ports: agents, the forge, the account's usage, the
//! maintainer's webhook, kelpie's shots and the clock
//!
//! The work-item loop reaches the outside world only through these traits.
//! [`crate::adapters`] holds the real ones and the test rig holds stand-ins,
//! so a test sees exactly the calls the runner makes.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::board::READY;
use crate::board::{OpenPullRequest, ReadyIssue};
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::local_paths::Leak;
use crate::review_bot::{Activity, Login, Profile};
use crate::settings::{Account, ForgeSlug};
use crate::shots::{ShotsJob, ShotsRun};
use crate::webhook::Webhook;

mod agent;
mod gpu;
mod leased;
mod local_paths;
mod model_seat;
mod relay;
mod reviewer;
mod routed;
mod sandbox;

pub use agent::{
    AgentCall, AgentError, AgentReply, Agents, Cost, Fence, Guard, Reach, Role, Session, SessionId,
    Tools, Usage,
};
pub use gpu::{Gpu, GpuError, GpuMetrics};
pub use leased::{Leased, LocalLeases};
pub use local_paths::Guarded;
pub use model_seat::ModelSeat;
pub use relay::{Cleared, Relay, RelayError};
pub use reviewer::{Reviewer, ReviewerError, RoundStage};
pub use routed::Routed;
pub use sandbox::{Forward, Policy, Sandbox, SandboxError, Unreadable};

/// Seconds since the Unix epoch
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub u64);

/// Tells the time
pub trait Clock: Send {
    /// The current time
    fn now(&self) -> Timestamp;
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

/// The forge that holds the project's issues and pull requests
pub trait Forge: Send {
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

    /// Whether the review bot `login` has commented on any pull request of `repo`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn review_bot_seen(&self, repo: &ForgeSlug, login: Login<'_>) -> Result<bool, ForgeError>;

    /// Issue `number` on `repo`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked, has no such issue, or
    /// its answer cannot be read.
    fn issue(&self, repo: &ForgeSlug, number: u64) -> Result<Issue, ForgeError>;

    /// The open issues on `repo` labelled [`READY`]
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn ready_issues(&self, repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError>;

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

    /// The login of the account kelpie acts as, which opens its pull requests
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn viewer(&self) -> Result<String, ForgeError>;

    /// Posts `body` as a comment on pull request `number`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the comment cannot be posted.
    fn comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError>;

    /// Posts `body` as a comment on pull request `number`, and returns its id
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the comment cannot be posted or its id read.
    fn post_comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<u64, ForgeError>;

    /// Replaces comment `id`'s body with `body`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the comment is gone or cannot be edited.
    fn edit_comment(&self, repo: &ForgeSlug, id: u64, body: &str) -> Result<(), ForgeError>;

    /// The open issues on `repo`, for telling a finding already filed
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn open_issues(&self, repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError>;

    /// Opens an issue on `repo` with these labels, and returns its number
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses, or its answer names no number.
    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u64, ForgeError>;

    /// Makes issue `child` a sub-issue of issue `parent`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn add_sub_issue(&self, repo: &ForgeSlug, parent: u64, child: u64) -> Result<(), ForgeError>;

    /// Marks issue `number` blocked by issue `blocker`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn add_blocker(&self, repo: &ForgeSlug, number: u64, blocker: u64) -> Result<(), ForgeError>;

    /// Closes issue `number` with `comment`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn close_issue(&self, repo: &ForgeSlug, number: u64, comment: &str) -> Result<(), ForgeError>;

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

    /// Adds `label` to issue `number`, or takes it off
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn set_issue_label(
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
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn resolve_thread(&self, repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError>;

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

/// A pull request as a rework or an adoption reads it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reviewed {
    /// Open, merged or closed
    pub state: PullRequestState,
    /// Its title
    pub title: String,
    /// Its body, as written
    pub body: String,
    /// The issues on the same repo that it closes when it merges
    pub closes: Vec<u64>,
    /// The branch it merges into
    pub base: String,
    /// The branch it merges from
    pub branch: String,
    /// Whether that branch is on a fork rather than the repo itself
    pub from_fork: bool,
    /// Its author's login, empty for a deleted account
    pub author: String,
    /// Whether it is still a draft
    pub draft: bool,
    /// Its labels' names
    pub labels: Vec<String>,
    /// The maintainer's latest review: the latest one a person left, since
    /// kelpie's own reviewers post none and bots are left out
    pub review: Option<MaintainerReview>,
}

/// One review the maintainer left, as written
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintainerReview {
    /// The forge's id for it
    pub id: String,
    /// Whether it requests changes, which only a reviewer other than the
    /// pull request's author can do
    pub changes_requested: bool,
    /// Its body, empty when it has none
    pub body: String,
    /// Its comments whose threads are still unresolved, in the forge's order
    pub comments: Vec<ReviewComment>,
}

/// One comment on a line of a pull request's diff
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    /// The file it is on
    pub file: String,
    /// The line, when the forge still places it
    pub line: Option<u32>,
    /// What it says
    pub body: String,
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
    /// At least one check is still running or queued
    Pending,
    /// Every check finished, and none failed
    Passed,
    /// Every check finished, and these failed
    Failed(Vec<String>),
}

/// A label kelpie makes on a project's repo
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewLabel {
    /// Its name
    pub name: &'static str,
    /// Its colour, as six lowercase hex digits with no `#`, as GitHub takes it
    pub color: &'static str,
    /// What it means, shown beside it on the forge
    pub description: &'static str,
}

/// An issue as the forge holds it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Its title
    pub title: String,
    /// Its body, as written
    pub body: String,
    /// Its labels' names
    pub labels: Vec<String>,
    /// Whether it is still open
    pub open: bool,
    /// The issue it is a sub-issue of, if any
    pub parent: Option<u64>,
    /// The issues it is blocked by, as far as the forge lists them
    pub blocked_by: Vec<u64>,
}

/// An open issue, as the follow-up check reads it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenIssue {
    /// Its number
    pub number: u64,
    /// Its title
    pub title: String,
    /// Its body, as written
    pub body: String,
}

/// Why a forge call failed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeError {
    /// The forge's command line tool could not be started, with the OS's reason
    Spawn(String),
    /// The tool ran and exited unsuccessfully, with this on stderr
    Failed(String),
    /// The tool succeeded but its output was not what was asked for
    Unreadable(String),
    /// A field of the post names something private to this machine, so kelpie never sent it
    LocalPath {
        /// The field, such as "the comment"
        what: &'static str,
        /// What it names
        leak: Leak,
    },
}

impl fmt::Display for ForgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run gh: {error}"),
            Self::Failed(stderr) => write!(f, "gh failed: {}", stderr.trim()),
            Self::Unreadable(output) => write!(f, "unreadable gh output: {}", output.trim()),
            Self::LocalPath { what, leak } => write!(f, "not posted: {what} names {leak}"),
        }
    }
}

impl core::error::Error for ForgeError {}

/// How much of one usage window the account has spent
// wire format: changing this is a breaking change to the pacer's status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
    /// The ruling
    pub id: u64,
    /// What answers it
    pub takes: Takes,
}

/// What answers a ruling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takes {
    /// The worker's question: an answer
    Answer,
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
    /// The webhook is off and the relay could not take the ruling, with why
    Relay(String),
    /// The webhook's replies came back in a shape kelpie cannot read
    Unreadable,
}

impl fmt::Display for AlertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run curl: {error}"),
            Self::Unreachable(code) => write!(f, "curl could not reach the webhook (exit {code})"),
            Self::Refused(status) => write!(f, "the webhook answered HTTP {status}"),
            Self::Relay(reason) => f.write_str(reason),
            Self::Unreadable => f.write_str("the webhook's replies could not be read"),
        }
    }
}

impl core::error::Error for AlertError {}

/// How serious a review finding is
///
/// Qwen and the Claude review round report only these three; the judge may
/// regrade to any of them but never invents a fourth. LOW is the nit level.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A nit
    Low,
    /// Worth fixing before merge
    Medium,
    /// A real defect
    High,
}

/// One review finding, as a reviewer reports it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    /// How serious the reviewer thinks it is
    pub severity: Severity,
    /// The file it is in
    pub file: String,
    /// The line, or 0 when the reviewer names none
    pub line: u32,
    /// What is wrong
    pub what: String,
    /// Why it matters
    pub why: String,
}

/// How `what` begins in the line qwen-review.sh writes for a file it did not
/// review, whatever the reason
const NOT_REVIEWED: &str = "not reviewed: ";

/// What the script says of a file over its chunk limit, after
/// [`NOT_REVIEWED`]
const TOO_LARGE: &str = "exceeds the chunk limit";

impl Finding {
    /// Whether this is the line the script writes for a file it skipped as
    /// over the chunk limit
    ///
    /// The model is up, so the line is a finding: the file needs a hunk
    /// review, or reviewing by hand. This and [`Finding::is_unreviewed`] are
    /// the two kinds of `not reviewed:` line, so they never both hold.
    pub fn is_skipped_for_size(&self) -> bool {
        self.is_not_reviewed() && self.what.contains(TOO_LARGE)
    }

    /// Whether this is a line the script writes for a file it could not
    /// review for any other reason, such as the model not being reached
    ///
    /// Both kinds are told apart by the script's own wording, since the
    /// state file's findings have no field to hold the reason.
    pub fn is_unreviewed(&self) -> bool {
        self.is_not_reviewed() && !self.what.contains(TOO_LARGE)
    }

    fn is_not_reviewed(&self) -> bool {
        self.severity == Severity::Low && self.line == 0 && self.what.starts_with(NOT_REVIEWED)
    }
}

/// Parses reviewer output in qwen's `SEVERITY|file:line|what|why` format
///
/// A line that does not fit the shape is skipped rather than failing the
/// whole round: the script itself only ever writes well-formed lines, so one
/// that does not fit is safer to drop than to invent a location for.
pub fn parse_findings(text: &str) -> Vec<Finding> {
    text.lines().filter_map(parse_finding_line).collect()
}

/// Reads a model's review reply: its findings, or none when its last
/// non-empty line is exactly `CLEAN`
///
/// A summary ahead of that line is fine. `CLEAN` anywhere else, or inside a
/// longer line such as "not CLEAN", is not a verdict.
///
/// # Errors
///
/// The reply, trimmed, when it holds no finding and does not end on `CLEAN`:
/// an empty reply or prose reviewed nothing, which is not clean.
pub fn read_review(text: &str) -> Result<Vec<Finding>, String> {
    let findings = parse_findings(text);
    let ends_clean = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim() == "CLEAN");
    if findings.is_empty() && !ends_clean {
        return Err(text.trim().to_owned());
    }
    Ok(findings)
}

fn parse_finding_line(line: &str) -> Option<Finding> {
    let mut parts = line.splitn(4, '|');
    let severity = match parts.next()? {
        "LOW" => Severity::Low,
        "MEDIUM" => Severity::Medium,
        "HIGH" => Severity::High,
        _ => return None,
    };
    let location = parts.next()?;
    let what = parts.next()?;
    let why = parts.next()?;
    // A screenshot has no lines, and a Claude round names one without `:0`.
    let (file, line) = match location.rsplit_once(':') {
        Some((file, n)) => (file, n.parse().ok()?),
        None if location.ends_with(".png") => (location, 0),
        None => return None,
    };
    Some(Finding {
        severity,
        file: file.to_owned(),
        line,
        what: what.to_owned(),
        why: why.to_owned(),
    })
}

/// The judge's ruling on one finding
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    /// Whether the finding holds
    pub holds: bool,
    /// The judge's severity, which may regrade the reviewer's in either
    /// direction. Meaningless when the finding does not hold.
    pub severity: Severity,
    /// One sentence
    pub reason: String,
}

/// Takes a work item's shots
pub trait Shots: Send + Sync {
    /// Runs `job` to its end
    ///
    /// Never fails: whatever went wrong, the whole run included, is in the
    /// run it returns, since a shots run never holds a gate.
    fn take(&self, job: &ShotsJob) -> ShotsRun;

    /// Stops the dev server a run recorded in `server_pid` and left behind,
    /// such as one the worker's shots tool started before its `claude` was
    /// killed. Reads that one file, never a folder's listing.
    fn stop_left(&self, server_pid: &std::path::Path);

    /// Stops dev servers nothing runs any more that sit under `folders`,
    /// the worktrees and build folders kelpie owns. Matches only by those
    /// folders, never by a program's name or a port alone.
    fn stop_orphans(&self, folders: &[std::path::PathBuf]);
}

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

    /// Gives `kind` back, or withdraws the ask
    fn give_back(&self, kind: &LeaseKind);

    /// Tells the dog what this run saw of `kind`'s review window
    fn window(&self, kind: &LeaseKind, fact: WindowFact, value: u64);
}

/// Every port the runner uses, as one bundle
pub struct Ports {
    /// The agents every role runs on, shared so a turn runs without holding the runner
    pub agents: Arc<dyn Agents>,
    /// The forge
    pub forge: Box<dyn Forge>,
    /// The Claude account's usage, from `/usage`
    pub meter: Box<dyn Meter>,
    /// The Codex account's usage
    pub codex_meter: Box<dyn Meter>,
    /// The local round's runner, shared so a round runs without holding the runner
    pub reviewer: Arc<dyn Reviewer>,
    /// The locks a local agent's calls hold
    pub local_leases: Arc<dyn LocalLeases>,
    /// A profile for each review bot a round may summon
    pub review_bots: Vec<Arc<dyn Profile>>,
    /// The GPU's figures, for `status`
    pub gpu: Arc<dyn GpuMetrics>,
    /// The maintainer's relay session, sent every ruling alongside the webhook
    pub relay: Arc<dyn Relay>,
    /// The maintainer's webhook, shared so a post runs without holding the runner
    pub alerts: Arc<dyn Alerts>,
    /// The dog's book leases, which the runner's `grant` trigger fills
    pub leases: Arc<dyn Leases>,
    /// Kelpie's shots, shared so a run takes them without holding the runner
    pub shots: Arc<dyn Shots>,
    /// The clock
    pub clock: Box<dyn Clock>,
}

impl Ports {
    /// The meter that reads `account`'s usage
    pub fn meter_of(&self, account: Account) -> &dyn Meter {
        match account {
            Account::Claude => self.meter.as_ref(),
            Account::Codex => self.codex_meter.as_ref(),
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
