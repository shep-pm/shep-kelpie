//! The runner's ports: Claude, the forge, the account's usage, the
//! maintainer's webhook, kelpie's shots and the clock
//!
//! The work-item loop reaches the outside world only through these traits.
//! [`crate::adapters`] holds the real ones and the test rig holds stand-ins,
//! so a test sees exactly the calls the runner makes.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::board::READY;
use crate::board::{OpenPullRequest, ReadyIssue};
use crate::coderabbit::Activity;
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::settings::{Effort, ForgeSlug};
use crate::shots::{ShotsJob, ShotsRun};
use crate::webhook::Webhook;

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

    /// What CodeRabbit has posted on pull request `number`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn coderabbit(&self, repo: &ForgeSlug, number: u64) -> Result<Activity, ForgeError>;

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

/// An issue as the forge holds it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Its title
    pub title: String,
    /// Its body, as written
    pub body: String,
    /// Its labels' names
    pub labels: Vec<String>,
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
}

impl fmt::Display for ForgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run gh: {error}"),
            Self::Failed(stderr) => write!(f, "gh failed: {}", stderr.trim()),
            Self::Unreadable(output) => write!(f, "unreadable gh output: {}", output.trim()),
        }
    }
}

impl core::error::Error for ForgeError {}

/// Which role a Claude call is made for
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// A turn of the worker's session
    Worker,
    /// A Claude review round, always a fresh session
    Reviewer,
    /// A one-shot that judges findings
    Judge,
}

/// A Claude session's id
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

/// Which session a call runs in
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// A new session that takes this id
    New(SessionId),
    /// The existing session with this id
    Resume(SessionId),
}

impl Session {
    /// The session's id, new or resumed
    pub fn id(&self) -> &SessionId {
        match self {
            Self::New(id) | Self::Resume(id) => id,
        }
    }
}

/// One headless Claude call
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCall {
    /// The role it is made for
    pub role: Role,
    /// Passed to `--model` as written
    pub model: String,
    /// Passed to `--effort`
    pub effort: Effort,
    /// The session it runs in
    pub session: Session,
    /// The folder the session runs in
    pub cwd: PathBuf,
    /// The settings file kelpie wrote for the call
    pub settings: PathBuf,
    /// Kelpie's instructions, appended to the system prompt
    ///
    /// A resumed session keeps the instructions it started with, so they
    /// are passed only when the session is new.
    pub instructions: Option<PathBuf>,
    /// The turn's prompt
    pub prompt: String,
    /// The MCP servers the session starts with, beside any the repo names
    pub mcp_config: Option<PathBuf>,
    /// Kills the call, and returns [`ClaudeError::TimedOut`], once it has
    /// run this long
    pub timeout: Option<Duration>,
}

/// Tokens one call used, as `claude -p` reports them
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Uncached input tokens
    pub input: u64,
    /// Tokens written to the prompt cache
    pub cache_write: u64,
    /// Tokens read from the prompt cache
    pub cache_read: u64,
    /// Output tokens, thinking included
    pub output: u64,
}

/// An amount of money in billionths of a US dollar
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cost(pub u64);

impl Cost {
    const PER_USD: f64 = 1e9;

    /// The nearest amount to `usd` dollars, or `None` when it is negative or not a number
    pub fn from_usd(usd: f64) -> Option<Self> {
        // The cast saturates, so an absurd figure cannot wrap.
        (usd.is_finite() && usd >= 0.0).then(|| Self((usd * Self::PER_USD).round() as u64))
    }

    /// The amount in dollars
    pub fn usd(self) -> f64 {
        self.0 as f64 / Self::PER_USD
    }
}

/// What a Claude call answered
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeReply {
    /// The session the call ran in
    pub session_id: SessionId,
    /// The final message's text
    pub text: String,
    /// What this call used
    pub usage: Usage,
    /// What the session has cost so far, this call included
    pub session_cost: Cost,
}

/// Runs headless Claude calls
pub trait Claude: Send + Sync {
    /// Runs one call to its end
    ///
    /// # Errors
    ///
    /// [`ClaudeError`] when the call cannot run or does not succeed.
    fn run(&self, call: &ClaudeCall) -> Result<ClaudeReply, ClaudeError>;
}

/// Why a Claude call failed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeError {
    /// `claude` could not be started, with the OS's reason
    Spawn(String),
    /// The session to resume has no transcript, so it never started
    NoSession(SessionId),
    /// The call was ended because the runner is stopping
    Stopped,
    /// The call ran past its turn's ceiling and was stopped
    TimedOut,
    /// `claude` exited without a result it reports as a success
    Failed(String),
    /// `claude`'s output was not the JSON result asked for
    Unreadable(String),
}

impl fmt::Display for ClaudeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run claude: {error}"),
            Self::NoSession(id) => write!(f, "claude has no session {}", id.0),
            Self::Stopped => f.write_str("claude was stopped with the runner"),
            Self::TimedOut => f.write_str("claude ran past its turn's ceiling"),
            Self::Failed(detail) => write!(f, "claude failed: {}", detail.trim()),
            Self::Unreadable(output) => write!(f, "unreadable claude output: {}", output.trim()),
        }
    }
}

impl core::error::Error for ClaudeError {}

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
}

impl fmt::Display for MeterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
}

/// Posts alerts to the maintainer's webhook
pub trait Alerts: Send + Sync {
    /// Posts `alert` to `webhook`
    ///
    /// # Errors
    ///
    /// [`AlertError`] when the post cannot be made or is refused. Its text
    /// never carries the webhook's URL.
    fn post(&self, webhook: &Webhook, alert: &Alert) -> Result<(), AlertError>;
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
}

impl fmt::Display for AlertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run curl: {error}"),
            Self::Unreachable(code) => write!(f, "curl could not post it (exit {code})"),
            Self::Refused(status) => write!(f, "the webhook answered HTTP {status}"),
            Self::Relay(reason) => f.write_str(reason),
        }
    }
}

impl core::error::Error for AlertError {}

/// Sends a ruling to the maintainer's relay session
///
/// The relay is one background Claude Code session, found by its fixed
/// name so a second one is never started. Sending only delivers the
/// message: the relay's own reply, if any, is not read here. The
/// maintainer's answer comes back later through `shep trigger`, on its own.
pub trait Relay: Send + Sync {
    /// Sends `message`, starting the relay first if none is running
    ///
    /// `model` and `effort` are passed to `--model`/`--effort` only when a
    /// start is needed: a relay already running keeps what it started with.
    ///
    /// # Errors
    ///
    /// [`RelayError`] when the relay cannot be started or reached.
    fn send(&self, message: &str, model: &str, effort: Effort) -> Result<(), RelayError>;

    /// Sends `message` to the relay if one is running, and never starts one
    ///
    /// A relay started afresh never asked what `message` is about.
    ///
    /// # Errors
    ///
    /// [`RelayError`] when a running relay cannot be reached.
    fn tell(&self, message: &str) -> Result<(), RelayError>;

    /// Deletes the relay, if one exists, conversation included, so kelpie
    /// starts a fresh one next time and nothing a worker's question tried
    /// to carry into it survives the clear
    ///
    /// # Errors
    ///
    /// [`RelayError`] when a running relay could not be deleted. Not an
    /// error when none was running.
    fn clear(&self) -> Result<(), RelayError>;
}

/// Why the relay could not be reached
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayError {
    /// The relay session could not be started, with the reason
    CannotStart(String),
    /// The relay was started but never appeared in `claude agents --json --all`
    NeverAppeared,
    /// Its messaging socket could not be reached, with the reason
    Unreachable(String),
    /// A running relay could not be stopped, with the reason
    CannotStop(String),
}

impl fmt::Display for RelayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CannotStart(reason) => write!(f, "cannot start the relay: {reason}"),
            Self::NeverAppeared => f.write_str("the relay never appeared after starting"),
            Self::Unreachable(reason) => write!(f, "cannot reach the relay: {reason}"),
            Self::CannotStop(reason) => write!(f, "cannot stop the relay: {reason}"),
        }
    }
}

impl core::error::Error for RelayError {}

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

/// Parses reviewer output in qwen's `SEVERITY|file:line|what|why` format
///
/// A line that does not fit the shape is skipped rather than failing the
/// whole round: the script itself only ever writes well-formed lines, so one
/// that does not fit is safer to drop than to invent a location for.
pub fn parse_findings(text: &str) -> Vec<Finding> {
    text.lines().filter_map(parse_finding_line).collect()
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

/// Runs one round of the maintainer's qwen-review script
pub trait Reviewer: Send + Sync {
    /// Runs round `round` against `worktree`'s diff from `base`, usually
    /// `origin/main`, writing the script's own findings under `out`
    ///
    /// Feeds hunk files for anything the script skips as too large, folding
    /// their findings back in against the original file.
    ///
    /// # Errors
    ///
    /// [`ReviewerError`] when the script cannot be run or its round did not
    /// finish.
    fn round(
        &self,
        worktree: &std::path::Path,
        base: &str,
        out: &std::path::Path,
        round: u32,
    ) -> Result<Vec<Finding>, ReviewerError>;
}

/// Why a qwen-review round did not produce findings
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewerError {
    /// The script could not be started, with the OS's reason
    Spawn(String),
    /// The script ran and exited unsuccessfully, with this on stderr
    Failed(String),
    /// The script exited successfully but left no completion marker
    Incomplete,
    /// The round was ended because the runner is stopping
    Stopped,
}

impl fmt::Display for ReviewerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run qwen-review.sh: {error}"),
            Self::Failed(stderr) => write!(f, "qwen-review.sh failed: {}", stderr.trim()),
            Self::Incomplete => f.write_str("qwen-review.sh left no completion marker"),
            Self::Stopped => f.write_str("qwen-review.sh was stopped with the runner"),
        }
    }
}

impl core::error::Error for ReviewerError {}

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
    /// Headless Claude, shared so a turn runs without holding the runner
    pub claude: Arc<dyn Claude>,
    /// The forge
    pub forge: Box<dyn Forge>,
    /// The account's usage
    pub meter: Box<dyn Meter>,
    /// The qwen-review script, shared so a round runs without holding the runner
    pub reviewer: Arc<dyn Reviewer>,
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

impl fmt::Debug for Ports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ports").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod findings_tests {
    use super::*;

    #[test]
    fn well_formed_lines_parse_in_order() {
        let text = "HIGH|src/lib.rs:42|does the bad thing|breaks prod\n\
                    LOW|src/main.rs:0|not reviewed: 900 lines exceeds the chunk limit|split the file or review it by hand";
        assert_eq!(
            parse_findings(text),
            vec![
                Finding {
                    severity: Severity::High,
                    file: "src/lib.rs".into(),
                    line: 42,
                    what: "does the bad thing".into(),
                    why: "breaks prod".into(),
                },
                Finding {
                    severity: Severity::Low,
                    file: "src/main.rs".into(),
                    line: 0,
                    what: "not reviewed: 900 lines exceeds the chunk limit".into(),
                    why: "split the file or review it by hand".into(),
                },
            ]
        );
    }

    #[test]
    fn blank_and_malformed_lines_are_skipped() {
        let text = "\nCLEAN\nnot a finding at all\nMEDIUM|only|two|fields|extra\nMEDIUM|a.rs:no-number|what|why";
        assert_eq!(parse_findings(text), vec![]);
    }

    // What a live Claude round wrote, with the screenshot's path shortened
    #[test]
    fn a_screenshot_named_without_a_line_is_line_zero() {
        let text = "HIGH|/k/shots/lab/7/events-mobile-dark.png|dark matches light|no dark theme\n\
                    LOW|src/app.tsx|no line|dropped";
        let [finding] = parse_findings(text).try_into().unwrap();
        assert_eq!(finding.file, "/k/shots/lab/7/events-mobile-dark.png");
        assert_eq!(finding.line, 0);
    }

    #[test]
    fn severities_order_low_to_high() {
        assert!(Severity::Low < Severity::Medium);
        assert!(Severity::Medium < Severity::High);
    }

    // Recorded shape of a real round-N.txt, one line per severity plus a
    // skipped-file placeholder.
    #[test]
    fn a_recorded_findings_file_parses() {
        let text = include_str!("../fixtures/qwen-round.txt");
        let findings = parse_findings(text);
        assert_eq!(findings.len(), 4);
        assert_eq!(findings[0].severity, Severity::High);
        assert_eq!(findings[0].file, "src/pricing.rs");
        assert_eq!(
            findings[3].what,
            "not reviewed: 900 lines exceeds the chunk limit"
        );
    }
}
