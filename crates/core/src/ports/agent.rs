//! The agent port: one session call, in terms no harness owns
//!
//! An agent is a harness that runs sessions plus the model and effort it
//! runs them on. A call names what the session may use and what fences it
//! in, and each harness's adapter turns that into its own settings.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use super::Timestamp;
use crate::hooks::Hooks;
use crate::settings::{AgentHarness, Effort, GuardHook, Harness};
use crate::ticket::TicketId;

/// Which role an agent call is made for
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// A turn of the worker's session
    Worker,
    /// A reviewer's session, always a fresh one
    Reviewer,
    /// The project manager's session, resumed across its wakes
    Pm,
    /// The representative's one-shot session, which words a post to a
    /// contributor and reads and runs nothing
    Representative,
    /// The triager's one-shot session, which researches an issue with the
    /// reviewer's reach
    Triager,
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
        assert_eq!(name(Role::Representative), "representative");
        assert_eq!(Role::Representative.as_str(), "representative");
        assert_eq!(name(Role::Triager), "triager");
        assert_eq!(Role::Triager.as_str(), "triager");
    }
}

impl Role {
    /// The role's name, as a lamb's label carries it
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Reviewer => "reviewer",
            Self::Pm => "pm",
            Self::Representative => "representative",
            Self::Triager => "triager",
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
    /// The project manager's: reading its working folder, and adding to
    /// the end of [`PM_NOTES`] there, which a hook holds it to
    Pm,
    /// The project manager's batch retro: reading and searching its folders,
    /// with no file written, no command and no crew. Its reply is the report.
    Retro,
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
    /// Whether its sandbox keeps it from the maintainer's keychain, as it does
    /// on a repo kelpie's App covers, which the runner sets on each call
    pub block_keychain: bool,
    /// The code graph server it is given, where the project turns it on and
    /// the call is a worker's or a reviewer's on Claude Code, pi, Codex or
    /// goose. A goose session is given it, like every server, when it is
    /// created, and a resumed session keeps the set it began with.
    pub code_graph: Option<CodeGraph>,
    /// The browser it drives, where the project turns it on and the call is a
    /// worker's working turn on Claude Code or pi
    pub browser: Option<Box<Browser>>,
    /// The MCP servers of the repo's plugins and of its `.mcp.json`, by the
    /// name their tools are allowed under, each as Claude Code's `mcpServers`
    /// entry, which the pi, Codex and goose adapters turn into their own. A
    /// server added after a goose session began is not seen by it. They run
    /// inside the call's sandbox, so the hosts they reach still need the
    /// call's allowed domains.
    pub mcp_servers: BTreeMap<String, serde_json::Value>,
}

/// The code graph of a worktree, served to a session over MCP
///
/// The server is `binary`, run inside the call's sandbox with `index` as its
/// only store, so it reaches no host of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeGraph {
    /// The server's executable, under kelpie's tools
    pub binary: PathBuf,
    /// The folder the worktree's index is kept in, under kelpie's home
    pub index: PathBuf,
    /// The folder the server's daemon binds its socket in, one a work item, under kelpie's
    /// home. One daemon serves one index folder, and a second index in the same folder is
    /// refused.
    pub runtime: PathBuf,
}

/// The browser kelpie runs for a call, outside its sandbox, reached over a socket
///
/// The socket carries MCP lines to a filter ([`crate::bridge::Filter`]) in front of the
/// Playwright server. The session starts `kelpie mcp-connect <socket>` as the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browser {
    /// The kelpie binary, which carries the session's stdio to the socket
    pub kelpie: PathBuf,
    /// The call's socket, which the session's sandbox may connect to
    pub socket: PathBuf,
}

/// Where a session may write and connect, and what it may not run
///
/// The harness enforces it and fails closed when it cannot.
///
/// `Debug` does not leak `env`: a worker's `GH_TOKEN` is the maintainer's own
/// token. It shows the names and elides the values.
#[derive(Clone, PartialEq, Eq)]
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
    pub env: BTreeMap<String, String>,
    /// Unix sockets it may connect to: the kelpie dog's lease socket,
    /// where a worker takes the lease it runs tests under
    pub sockets: Vec<PathBuf>,
    /// Kelpie's own checks, which judge its actions before they run
    pub guard: Guard,
    /// The project's own hooks, which run after kelpie's
    pub hooks: Vec<GuardHook>,
}

impl fmt::Debug for Fence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env: BTreeMap<&String, &str> = self.env.keys().map(|name| (name, "..")).collect();
        f.debug_struct("Fence")
            .field("write", &self.write)
            .field("no_write", &self.no_write)
            .field("no_read", &self.no_read)
            .field("read", &self.read)
            .field("hosts", &self.hosts)
            .field("no_commands", &self.no_commands)
            .field("env", &env)
            .field("sockets", &self.sockets)
            .field("guard", &self.guard)
            .field("hooks", &self.hooks)
            .finish()
    }
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
    /// The project's base branch, which only the project manager changes
    pub base: String,
    /// Whether kelpie pushes the worker's commits itself, so the guard refuses
    /// the worker's own push: its branch is a contributor's, on their fork
    pub kelpie_pushes: bool,
    /// Folders besides the home folder whose paths stay off the forge: kelpie's
    /// home and the project's checkout
    pub folders: Vec<PathBuf>,
    /// The project's own hooks, which the guard runs on a commit, a push and
    /// the text of a `gh` command, whatever the harness
    pub hooks: Hooks,
}

impl Guard {
    /// The flag that names the base branch on `kelpie guard`'s command line,
    /// none for the default
    pub fn base_flag(&self) -> Option<String> {
        crate::guard::base_flag(&self.base)
    }

    /// The flag that says kelpie pushes the worker's commits, when it does
    pub fn kelpie_pushes_flag(&self) -> Option<&'static str> {
        self.kelpie_pushes
            .then_some(crate::guard::KELPIE_PUSHES_FLAG)
    }
}

/// The prompt that asks a session to compact itself, which Claude Code runs as
/// its own command
pub const COMPACT: &str = "/compact";

/// One session call to an agent
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCall {
    /// The role it is made for
    pub role: Role,
    /// The harness it runs on
    pub harness: AgentHarness,
    /// The ticket of the work item it is made for, which its lamb is labelled with
    ///
    /// None for a call that is no work item's, such as the project manager's.
    pub issue: Option<TicketId>,
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
    /// The plugin folders its steps' skills, and the repo's plugins, are in
    pub plugin_dirs: Vec<PathBuf>,
    /// The kinds of tool it may use
    pub tools: Tools,
    /// The tools its agent file names, as `claude --tools` takes them, in
    /// place of what a worker's or a reviewer's own kind starts with. A
    /// retro and the project manager keep their own.
    pub agent_tools: Option<String>,
    /// What it may reach
    pub reach: Reach,
    /// Tokens of context at which the harness compacts the session in the
    /// middle of the turn, if it is to. A compaction summarizes the turn's
    /// own prompt, so the harness is set to hand `prompt` back after one.
    pub compact_window: Option<NonZeroU32>,
}

impl AgentCall {
    /// A call for `role` that uses `tools`, to run `model` on `harness` at
    /// `effort` in `session`, from `cwd`, with `prompt`
    ///
    /// Every other field starts at what a call that needs nothing more has:
    /// no issue, settings path, instructions, plugin folders, agent tools or
    /// compaction window, and a reach that is nothing.
    #[expect(clippy::too_many_arguments, reason = "each is a field with no default")]
    pub fn new(
        role: Role,
        tools: Tools,
        harness: AgentHarness,
        model: impl Into<String>,
        effort: Effort,
        session: Session,
        cwd: PathBuf,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            role,
            harness,
            issue: None,
            model: model.into(),
            effort,
            session,
            cwd,
            settings: PathBuf::new(),
            instructions: None,
            prompt: prompt.into(),
            plugin_dirs: Vec::new(),
            tools,
            agent_tools: None,
            reach: Reach::default(),
            compact_window: None,
        }
    }

    /// What its lamb is labelled: its issue and role, such as `#7 worker`,
    /// or `pm` for the project manager, which is no work item's
    pub fn label(&self) -> String {
        match self.role {
            Role::Pm => Role::Pm.as_str().to_owned(),
            role => match self.issue {
                Some(ticket) => format!("{} {}", ticket.shown(), role.as_str()),
                None => format!("#0 {}", role.as_str()),
            },
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

    /// Billionths of a dollar in a cent
    pub const PER_CENT: u64 = 10_000_000;

    /// The nearest amount to `usd` dollars, or `None` when it is negative or not a number
    pub fn from_usd(usd: f64) -> Option<Self> {
        // The cast saturates, so an absurd figure cannot wrap.
        (usd.is_finite() && usd >= 0.0).then(|| Self((usd * Self::PER_USD).round() as u64))
    }

    /// Whether it is nothing
    pub fn is_free(&self) -> bool {
        self.0 == 0
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
    /// Times the harness compacted the session by itself partway through
    /// the call, which a `COMPACT` call is not
    pub compactions: u32,
}

/// A call a runner's earlier image started, which still runs as the process's
/// child
///
/// An image that hands over to a new build carries one for each call in flight,
/// and the new build finishes the call from it.
// wire format: changing this is a change to the handover's state format
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adopted {
    /// The process the call runs as
    pub pid: u32,
    /// The call's record on disk, which says where its output goes
    pub record: PathBuf,
    /// The session the call resumes, told apart from a failure when the
    /// harness has none by that id
    pub resumes: Option<SessionId>,
    /// What the runner noted of the call when it started, which the record
    /// keeps
    pub flying: Option<Flying>,
    /// The harness the call runs on, whose adapter finishes it
    #[serde(default = "claude_code")]
    pub harness: Harness,
    /// What the old image's memory held for the call, which the `execve`
    /// ends, for a harness whose calls have any
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept: Option<Kept>,
    /// The session id kelpie gave the call, for a harness that keeps a record
    /// of its own by it and cannot tell it from the output
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
}

fn claude_code() -> Harness {
    Harness::ClaudeCode
}

/// Why a call in flight cannot be handed to another image
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotAdopted {
    /// The call's record is gone, as it is once the call has ended, so a call
    /// whose end is not yet heard shows this
    RecordGone(String),
    /// Anything else: the call is not a worker's, its harness cannot be
    /// handed over, or its record is there and cannot be read
    Refused(String),
}

impl fmt::Display for NotAdopted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Self::RecordGone(why) | Self::Refused(why)) = self;
        f.write_str(why)
    }
}

impl From<String> for NotAdopted {
    fn from(why: String) -> Self {
        Self::Refused(why)
    }
}

impl From<&str> for NotAdopted {
    fn from(why: &str) -> Self {
        Self::Refused(why.to_owned())
    }
}

/// What a call needs from the old image's memory to go on after a handover
///
/// The new image opens each again from this and from its own settings, and
/// holds it until the adopted call ends. A gateway's key is never in it.
// wire format: changing this is a change to the handover's state format
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    /// The socket of the forwarder that passes the call's model traffic on,
    /// which the new image opens again at that path
    pub forwarder: PathBuf,
    /// The model server the forwarder passes it to
    pub server: KeptServer,
    /// The session id the harness was given, which it answers in
    pub session: SessionId,
    /// The gateway lease the call holds, which the new image goes on renewing
    /// and releases when the call ends, or none for a model with no lease
    pub lease: Option<String>,
    /// The model the call runs on, which names its lease in the runner's
    /// metrics; none from an image that did not carry it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// The model server a carried call's forwarder passes to
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeptServer {
    /// An OpenAI-compatible server's base URL
    Url(String),
    /// A gateway, by its name in kelpie's settings
    Gateway(String),
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

    /// What a runner that did not start `call` needs to finish it, while it
    /// runs
    ///
    /// # Errors
    ///
    /// [`NotAdopted::RecordGone`] when the call's record is missing, and
    /// [`NotAdopted::Refused`] for any other reason it cannot be finished that
    /// way: its harness cannot be, or its record is unreadable.
    fn adoption(&self, call: &AgentCall) -> Result<Adopted, NotAdopted> {
        let _ = call;
        Err("this harness cannot hand a call to another image of the runner".into())
    }

    /// Finishes a call a runner's earlier image started, as [`Agents::run`]
    /// would have: waits for its process, which is this process's child, and
    /// reads its output
    ///
    /// # Errors
    ///
    /// As [`Agents::run`], and [`AgentError::Failed`] when the call's end was
    /// missed, so its status is lost.
    fn adopt(&self, adopted: &Adopted, ending: &Ending) -> Result<AgentReply, AgentError> {
        let _ = (adopted, ending);
        Err(AgentError::Setup(
            "this harness cannot finish a call another image of the runner started".into(),
        ))
    }

    /// Where `call`'s session writes its transcript, for `shep kelpie watch`
    /// to follow, or `None` for a harness that keeps none kelpie can read
    fn transcript(&self, call: &AgentCall) -> Option<Transcript> {
        let _ = call;
        None
    }

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

    /// The terminal session [`Agents::foreground`] started for `session` has
    /// ended, so what the harness kept for it can go
    ///
    /// The default keeps nothing. Asking again for a session with nothing kept
    /// does nothing.
    fn foreground_ended(&self, session: &SessionId) {
        let _ = session;
    }
}

/// Where a harness writes a session's transcript
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Transcript {
    /// The folder the harness writes its transcripts in, which the session's
    /// own file is found in once it is written
    pub folder: PathBuf,
    /// For a harness that names its own sessions, the folder where kelpie
    /// keeps which of them each of its session ids is
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threads: Option<PathBuf>,
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

/// What a call waits for before its model can answer
// wire format: changing this is a breaking change to `status`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wait {
    /// A lease on its model, from a gateway
    Lease,
    /// Its gateway answered busy, HTTP 503, and is asked again
    Busy,
    /// Nothing: its model has shown no output for a while, as kelpie reads it
    Silent,
    /// Its model host could not be reached, and the turn is tried again
    Unreachable,
}

impl Wait {
    /// What it waits for, in words
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lease => "a lease on its model",
            Self::Busy => "its gateway, which answered busy",
            Self::Silent => "its model's first output",
            Self::Unreachable => "its model host, which could not be reached",
        }
    }
}

/// The runner's hold on one call in flight, to end it past its turn's ceiling
///
/// Clones share the one call. The adapter running it ends its process
/// group once asked, however soon after the call began that is. A call that
/// waits for a lease says so, and says when it has it, so its ceiling
/// counts from then.
#[derive(Clone, Default)]
pub struct Ending {
    asked: Arc<AtomicBool>,
    flying: Arc<Mutex<Option<Flying>>>,
    began: Option<Arc<dyn Fn() + Send + Sync>>,
    waits: Option<Arc<dyn Fn(Wait) + Send + Sync>>,
}

impl fmt::Debug for Ending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ending")
            .field("asked", &self.asked())
            .finish_non_exhaustive()
    }
}

impl Ending {
    /// One that runs `waits` when the call waits for its model, and `began`
    /// when it has the lease it waited for
    pub fn telling(
        began: impl Fn() + Send + Sync + 'static,
        waits: impl Fn(Wait) + Send + Sync + 'static,
    ) -> Self {
        Self {
            asked: Arc::default(),
            flying: Arc::default(),
            began: Some(Arc::new(began)),
            waits: Some(Arc::new(waits)),
        }
    }

    /// Says the call waits for `wait` before its model can answer
    pub fn wait(&self, wait: Wait) {
        if let Some(waits) = &self.waits {
            waits(wait);
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

    /// Notes what the runner knows of the call that its process does not, for
    /// the record the call keeps on disk
    pub fn carry(&self, flying: Flying) {
        *self.flying.lock().unwrap_or_else(PoisonError::into_inner) = Some(flying);
    }

    /// What the runner last noted of the call, if it noted any
    pub fn carried(&self) -> Option<Flying> {
        *self.flying.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Says the call has the lease it waited for, and begins now
    pub fn begin(&self) {
        if let Some(began) = &self.began {
            began();
        }
    }
}

/// What the runner knows of a call in flight that the call's process does
/// not, which its record on disk keeps beside the process's own facts
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flying {
    /// Seconds of the turn's ceiling the call had when it started, which it
    /// has again from `began`
    pub left: u64,
    /// When the ceiling counts from: the call's start, or the grant of the
    /// lease it waited for
    pub began: Timestamp,
    /// Whether the call starts over a session that died before it began
    pub again: bool,
    /// Whether a worker's turn is its work item's first
    pub first: bool,
    /// Whether the call is a `/compact` of the worker's session
    pub compact: bool,
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
    /// The harness could not reach its model host: a 502 or 503 from it, or a
    /// connection it refused or reset
    ///
    /// It passes, so a turn that ends with it is tried again, not parked.
    Unreachable(Harness, String),
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
            Self::Unreachable(h, detail) => write!(
                f,
                "{} could not reach its model host: {}",
                h.command(),
                detail.trim()
            ),
            Self::Unreadable(h, output) => {
                write!(f, "unreadable {} output: {}", h.command(), output.trim())
            }
        }
    }
}

impl core::error::Error for AgentError {}
