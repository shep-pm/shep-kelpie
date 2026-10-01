//! The agent port: one session call, in terms no harness owns
//!
//! An agent is a harness that runs sessions plus the model and effort it
//! runs them on. A call names what the session may use and what fences it
//! in, and each harness's adapter turns that into its own settings.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::settings::{Effort, GuardHook};

/// Which role an agent call is made for
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
    /// A one-shot that plans a ready issue before any work item opens
    Planner,
}

impl Role {
    /// The role's name, as a lamb's label carries it
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::Judge => "judge",
            Self::Planner => "planner",
        }
    }
}

/// A session's id
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

/// The kinds of tool a session may use
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tools {
    /// Everything a worker needs to change a worktree, inside its fence
    Work,
    /// Reading and searching files, with no commands and no crew
    Review,
    /// None, beyond reading the folders its sandbox lists
    Answer,
}

/// What a session may reach, named for no harness
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sandbox {
    /// Folders outside its working folder that it may read
    pub read: Vec<PathBuf>,
    /// The fence on its writes, reads, hosts and commands. Without one,
    /// its tools alone hold it.
    pub fence: Option<Box<Fence>>,
}

/// Where a session may write and connect, and what it may not run
///
/// The harness enforces it and fails closed when it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fence {
    /// Folders it may write, and everything under them
    pub write: Vec<PathBuf>,
    /// Paths under those it may not write. `**` matches folders at any depth.
    pub no_write: Vec<PathBuf>,
    /// Paths it may not read, absolute or under `~/`, with `**` globs
    pub no_read: Vec<String>,
    /// The hosts it may reach, and no others
    pub hosts: Vec<String>,
    /// Commands it may not run, as prefixes where `*` matches any text
    pub no_commands: Vec<String>,
    /// Variables set for every command it runs
    pub env: BTreeMap<String, PathBuf>,
    /// The domains its browser may open, when it runs the preview's dev
    /// server and browser. `None` without a preview.
    pub preview: Option<Vec<String>>,
    /// Kelpie's own checks, which judge its actions before they run
    pub guard: Guard,
    /// The project's own hooks, which run after kelpie's
    pub hooks: Vec<GuardHook>,
}

/// What kelpie's own checks are run with
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guard {
    /// The kelpie binary, which runs the checks
    pub kelpie: PathBuf,
    /// The work item's worktree
    pub worktree: PathBuf,
    /// The worker's build folder
    pub build: PathBuf,
    /// The project repo's common git dir
    pub git_common_dir: PathBuf,
}

/// One session call to an agent
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCall {
    /// The role it is made for
    pub role: Role,
    /// The issue of the work item it is made for, which its lamb is labelled with
    pub issue: u64,
    /// The model, as the agent names it
    pub model: String,
    /// How hard the model thinks
    pub effort: Effort,
    /// The session it runs in
    pub session: Session,
    /// The folder the session runs in
    pub cwd: PathBuf,
    /// Where the harness's settings for the call go, which
    /// [`Agents::prepare`] writes from `tools` and `sandbox`
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
    /// The plugin folders its steps' skills are in
    pub plugin_dirs: Vec<PathBuf>,
    /// Kills the call, and returns [`AgentError::TimedOut`], once it has
    /// run this long
    pub timeout: Option<Duration>,
    /// The kinds of tool it may use
    pub tools: Tools,
    /// What it may reach
    pub sandbox: Sandbox,
}

/// Tokens one call used, as the harness reports them
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

/// What an agent call answered
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentReply {
    /// The session the call ran in
    pub session_id: SessionId,
    /// The final message's text
    pub text: String,
    /// What this call used
    pub usage: Usage,
    /// What the session has cost so far, this call included, where the
    /// harness reports it
    pub session_cost: Option<Cost>,
}

/// Runs agent sessions, one call at a time per session
pub trait Agents: Send + Sync {
    /// Writes what the harness needs on disk for `call`, before it is due
    ///
    /// # Errors
    ///
    /// [`AgentError::Setup`] when that cannot be written.
    fn prepare(&self, call: &AgentCall) -> Result<(), AgentError>;

    /// Runs one call to its end
    ///
    /// # Errors
    ///
    /// [`AgentError`] when the call cannot run or does not succeed.
    fn run(&self, call: &AgentCall) -> Result<AgentReply, AgentError>;
}

/// Why an agent call failed
// Every agent runs on Claude Code, which a ruling names as `claude`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    /// The harness's settings for the call could not be written, with why
    Setup(String),
    /// The harness could not be started, with the OS's reason
    Spawn(String),
    /// The session to resume has no transcript, so it never started
    NoSession(SessionId),
    /// The call was ended because the runner is stopping
    Stopped,
    /// The call ran past its turn's ceiling and was stopped
    TimedOut,
    /// The harness exited without a result it reports as a success
    Failed(String),
    /// The harness's output was not the result asked for
    Unreadable(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(reason) => f.write_str(reason),
            Self::Spawn(error) => write!(f, "cannot run claude: {error}"),
            Self::NoSession(id) => write!(f, "claude has no session {}", id.0),
            Self::Stopped => f.write_str("claude was stopped with the runner"),
            Self::TimedOut => f.write_str("claude ran past its turn's ceiling"),
            Self::Failed(detail) => write!(f, "claude failed: {}", detail.trim()),
            Self::Unreadable(output) => write!(f, "unreadable claude output: {}", output.trim()),
        }
    }
}

impl core::error::Error for AgentError {}
