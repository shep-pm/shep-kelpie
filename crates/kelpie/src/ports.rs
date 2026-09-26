//! The runner's ports: Claude, the forge and the clock
//!
//! The work-item loop reaches the outside world only through these traits.
//! [`crate::adapters`] holds the real ones and the test rig holds stand-ins,
//! so a test sees exactly the calls the runner makes.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A turn of the worker's session
    Worker,
    /// A Claude review round, always a fresh session
    Reviewer,
    /// A one-shot that judges findings
    Judge,
}

/// A Claude session's id
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

/// One headless Claude call
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCall {
    /// The role it is made for
    pub role: Role,
    /// Passed to `--model` as written
    pub model: String,
    /// Passed to `--effort`
    pub effort: Effort,
    /// The session to continue, or a fresh one when `None`
    pub resume: Option<SessionId>,
    /// The folder the session runs in
    pub cwd: PathBuf,
    /// The turn's prompt
    pub prompt: String,
}

/// What a Claude call answered
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeReply {
    /// The session the call ran in
    pub session_id: SessionId,
    /// The final message's text
    pub text: String,
}

/// Runs headless Claude calls
pub trait Claude: Send {
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
    /// `claude` exited without a result it reports as a success
    Failed(String),
    /// `claude`'s output was not the JSON result asked for
    Unreadable(String),
}

impl fmt::Display for ClaudeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run claude: {error}"),
            Self::Failed(detail) => write!(f, "claude failed: {}", detail.trim()),
            Self::Unreadable(output) => write!(f, "unreadable claude output: {}", output.trim()),
        }
    }
}

impl std::error::Error for ClaudeError {}

/// Every port the runner uses, as one bundle
pub struct Ports {
    /// Headless Claude
    pub claude: Box<dyn Claude>,
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
