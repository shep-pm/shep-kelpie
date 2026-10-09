//! The agent port: one session call, in terms no harness owns
//!
//! An agent is a harness that runs sessions plus the model and effort it
//! runs them on. A call names what the session may use and what fences it
//! in, and each harness's adapter turns that into its own settings.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::Timestamp;
use crate::guard::IssueRules;
use crate::settings::{AgentHarness, Effort, GuardHook, Harness, LeaseName};

/// Which role an agent call is made for
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// A turn of the worker's session
    Worker,
    /// A reviewer's session, always a fresh one
    Reviewer,
    /// The issue writer's one-shot session, which no work item records
    #[serde(rename = "issue-writer")]
    IssueWriter,
    /// The project manager's session, resumed across its wakes
    Pm,
}

#[cfg(test)]
mod role_tests {
    use super::Role;

    // A call record keeps its role in the state file by this name, and a build
    // from before a role refuses a record that holds it.
    #[test]
    fn every_role_keeps_its_name_in_the_state_file() {
        let name = |role| serde_json::to_value(role).unwrap();
        assert_eq!(name(Role::Reviewer), "reviewer");
        assert_eq!(Role::Reviewer.as_str(), "reviewer");
        assert_eq!(name(Role::IssueWriter), "issue-writer");
        assert_eq!(Role::IssueWriter.as_str(), "issue-writer");
    }
}

impl Role {
    /// The role's name, as a lamb's label carries it
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::IssueWriter => "issue-writer",
            Self::Pm => "pm",
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
    /// Reading and searching files, and the commands the issue writer's
    /// guard allows, with no crew and no file written
    Issues,
    /// The project manager's: reading its working folder, and adding to
    /// the end of [`PM_NOTES`] there, which a hook holds it to
    Pm,
}

/// The project manager's own notes, the one file in its folder it may change
pub const PM_NOTES: &str = "pm-notes.md";

/// What a session may reach, named for no harness
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reach {
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
    /// Folders inside `no_read` it may read after all: its own, inside the
    /// shepherd's home
    pub read: Vec<PathBuf>,
    /// The hosts it may reach, and no others
    pub hosts: Vec<String>,
    /// Commands it may not run, as prefixes where `*` matches any text
    pub no_commands: Vec<String>,
    /// Variables set for every command it runs
    pub env: BTreeMap<String, PathBuf>,
    /// Unix sockets it may connect to: the kelpie dog's lease socket,
    /// where a worker takes the lease it runs tests under
    pub sockets: Vec<PathBuf>,
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
    /// Folders besides the home folder whose paths stay off the forge: kelpie's
    /// home and the project's checkout
    pub folders: Vec<PathBuf>,
    /// For the issue writer, what it may file. The guard then runs only the
    /// commands that file, label and link issues, and git's read-only ones.
    pub issues: Option<IssueRules>,
}

/// One session call to an agent
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCall {
    /// The role it is made for
    pub role: Role,
    /// The harness it runs on
    pub harness: AgentHarness,
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
    /// The plugin folders its steps' skills are in
    pub plugin_dirs: Vec<PathBuf>,
    /// The kinds of tool it may use
    pub tools: Tools,
    /// The lease the call holds from start to end, for a local agent
    pub lease: Option<LeaseName>,
    /// What it may reach
    pub reach: Reach,
}

impl AgentCall {
    /// What its lamb is labelled: its issue and role, such as `#7 worker`,
    /// or `pm` for the project manager, which is no work item's
    pub fn label(&self) -> String {
        match self.role {
            Role::Pm => Role::Pm.as_str().to_owned(),
            role => format!("#{} {}", self.issue, role.as_str()),
        }
    }
}

/// Tokens one call used, as the harness reports them
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Uncached input tokens
    pub input: u64,
    /// Tokens written to the prompt cache, for five minutes or an hour
    pub cache_write: u64,
    /// The part of `cache_write` cached for five minutes, which costs less
    /// than an hour's, where the harness tells them apart
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write_5m: u64,
    /// Tokens read from the prompt cache
    pub cache_read: u64,
    /// Output tokens, thinking included
    pub output: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.cache_write_5m = self.cache_write_5m.saturating_add(other.cache_write_5m);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.output = self.output.saturating_add(other.output);
    }
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
    /// The tokens the session's context held at the call's last model
    /// request, where the harness reports it
    pub context: Option<u64>,
}

/// Runs agent sessions, one call at a time per session
pub trait Agents: Send + Sync {
    /// Writes what the harness needs on disk for `call`, before it is due
    ///
    /// # Errors
    ///
    /// [`AgentError::Setup`] when that cannot be written.
    fn prepare(&self, call: &AgentCall) -> Result<(), AgentError>;

    /// Runs one call to its end, or until `ending` is asked to end it
    ///
    /// # Errors
    ///
    /// [`AgentError`] when the call cannot run or does not succeed, and
    /// [`AgentError::TimedOut`] when `ending` ended it.
    fn run(&self, call: &AgentCall, ending: &Ending) -> Result<AgentReply, AgentError>;

    /// When `call`, in flight, last showed something: a tool call or a line
    /// of output, as its transcript or its output file was last written
    fn last_active(&self, call: &AgentCall) -> CallActivity {
        let _ = call;
        CallActivity::Untracked
    }

    /// `call`'s session as a command for the maintainer's terminal, inside
    /// the same sandbox and settings a call runs with, after [`Agents::prepare`]
    ///
    /// # Errors
    ///
    /// [`AgentError::Setup`] when the harness cannot run a session that way,
    /// or its sandbox cannot be set up.
    fn foreground(&self, call: &AgentCall) -> Result<std::process::Command, AgentError> {
        Err(AgentError::Setup(format!(
            "kelpie cannot start a {} session in a terminal",
            call.harness.harness().command()
        )))
    }
}

/// What a harness can say of a call's activity
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallActivity {
    /// The harness keeps no record kelpie can read
    Untracked,
    /// The harness keeps a record, and the call has written none of it yet
    Nothing,
    /// The call last made a tool call or wrote output at this time
    At(Timestamp),
}

/// When the file at `path` was last written, or [`CallActivity::Nothing`] while
/// it cannot be read
pub fn written_at(path: &std::path::Path) -> CallActivity {
    let modified = std::fs::metadata(path).and_then(|m| m.modified());
    let since = modified
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok());
    since.map_or(CallActivity::Nothing, |s| {
        CallActivity::At(Timestamp(s.as_secs()))
    })
}

/// The runner's hold on one call in flight, to end it past its turn's ceiling
///
/// Clones share the one call. The adapter running it ends its process
/// group once asked, however soon after the call began that is. A call that
/// waits for a lease first says when it has it, so its ceiling counts from
/// then.
#[derive(Clone, Default)]
pub struct Ending {
    asked: Arc<AtomicBool>,
    began: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl fmt::Debug for Ending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ending")
            .field("asked", &self.asked())
            .finish_non_exhaustive()
    }
}

impl Ending {
    /// One that runs `began` when the call has the lease it waited for
    pub fn telling(began: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            asked: Arc::default(),
            began: Some(Arc::new(began)),
        }
    }

    /// Asks the call to end
    pub fn end(&self) {
        self.asked.store(true, Ordering::SeqCst);
    }

    /// Whether the call has been asked to end
    pub fn asked(&self) -> bool {
        self.asked.load(Ordering::SeqCst)
    }

    /// Says the call has the lease it waited for, and begins now
    pub fn begin(&self) {
        if let Some(began) = &self.began {
            began();
        }
    }
}

/// Why an agent call failed, naming the harness where it was the harness's doing
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    /// The harness's settings for the call could not be written, with why
    Setup(String),
    /// The harness could not be started, with the OS's reason
    Spawn(Harness, String),
    /// The session to resume has no transcript, so it never started
    NoSession(Harness, SessionId),
    /// The call was ended because the runner is stopping
    Stopped,
    /// The call ran past its turn's ceiling and was ended
    TimedOut(Harness),
    /// The harness exited without a result it reports as a success
    Failed(Harness, String),
    /// The harness's output was not the result asked for
    Unreadable(Harness, String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(reason) => f.write_str(reason),
            Self::Spawn(h, error) => write!(f, "cannot run {}: {error}", h.command()),
            Self::NoSession(h, id) => write!(f, "{} has no session {}", h.command(), id.0),
            Self::Stopped => f.write_str("the agent was stopped with the runner"),
            Self::TimedOut(h) => write!(f, "{} ran past its turn's ceiling", h.command()),
            Self::Failed(h, detail) => write!(f, "{} failed: {}", h.command(), detail.trim()),
            Self::Unreadable(h, output) => {
                write!(f, "unreadable {} output: {}", h.command(), output.trim())
            }
        }
    }
}

impl core::error::Error for AgentError {}
