//! The runner's ports: Claude, the forge and the clock
//!
//! The work-item loop reaches the outside world only through these traits.
//! [`crate::adapters`] holds the real ones and the test rig holds stand-ins,
//! so a test sees exactly the calls the runner makes.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use crate::board::READY;
use crate::board::{OpenPullRequest, ReadyIssue};
use crate::settings::{Effort, ForgeSlug};

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

    /// Posts `body` as a comment on pull request `number`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the comment cannot be posted.
    fn comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError>;

    /// Marks draft pull request `number` ready for review
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn mark_ready(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError>;

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

impl std::error::Error for ForgeError {}

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
            Self::Failed(detail) => write!(f, "claude failed: {}", detail.trim()),
            Self::Unreadable(output) => write!(f, "unreadable claude output: {}", output.trim()),
        }
    }
}

impl std::error::Error for ClaudeError {}

/// Every port the runner uses, as one bundle
pub struct Ports {
    /// Headless Claude, shared so a turn runs without holding the runner
    pub claude: Arc<dyn Claude>,
    /// The forge
    pub forge: Box<dyn Forge>,
    /// The clock
    pub clock: Box<dyn Clock>,
}

impl fmt::Debug for Ports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ports").finish_non_exhaustive()
    }
}
