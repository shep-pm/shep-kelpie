//! Codex, headless as `codex exec --json`, one process per call, on the
//! ChatGPT account the `codex` command line is logged in to
//!
//! Each call runs whole inside kelpie's sandbox, as Claude Code's do, with
//! Codex's own sandbox and approvals off: on macOS a sandbox cannot start
//! inside another. Codex keeps its login and its sessions in `~/.codex`,
//! and the sandbox lets a call read the login and read and write the
//! sessions, and nothing else there. Kelpie never reads either. The
//! maintainer's own Codex config, rules, skills and instructions stay
//! unread. A fenced call runs kelpie's checks as Codex's own `PreToolUse`
//! hooks: `kelpie confine` on each `apply_patch`, the tool Codex writes
//! files with, and `kelpie guard` on each shell command.
//!
//! Codex names its own sessions, so kelpie keeps which Codex session each
//! of its session ids is, beside the call's settings.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::claude::sandbox::fence_policy;
use super::claude::settings::shell_quote;
use super::claude::{ClaudeCli, LambLabels};
use super::pi::step_skill;
use super::process::{Processes, RunError};
use crate::fence;
use crate::guard::{FOLDER_FLAG, NAME_FLAG};
use crate::ports::{
    AgentCall, AgentError, AgentReply, Agents, Fence, Policy, Sandbox, Session, SessionId, Tools,
    Usage,
};
use crate::profile::CREDENTIALS;
use crate::settings::{AgentHarness, Harness};

#[cfg(test)]
mod tests;

/// This adapter's harness, as its errors name it
const CODEX: Harness = Harness::Codex;

/// This harness, as the fence names it
const HARNESS: &str = "Codex";

/// The ChatGPT backend a Codex login on a ChatGPT plan calls, the one host
/// every call reaches
///
/// Not `auth.openai.com`, where a login is refreshed: the sandbox lets the
/// login be read, not written, so a refresh there would be lost. The
/// Codex usage read runs outside the sandbox before each dispatch, and
/// refreshes it there.
const MODEL_HOST: &str = "chatgpt.com";

/// Codex's home, as a path the sandbox reads under `~/`
const HOME: &str = "~/.codex/**";

/// Other people's skills Codex finds outside its home, which no call reads
const SKILLS: &str = "~/.agents/**";

/// Codex's features no call runs, each one a way past the call's tools,
/// its hooks or its sandbox
///
/// `unified_exec` keeps a shell running between tool calls, and its
/// `write_stdin` runs no hook, so commands typed there would pass no check.
/// Without it Codex runs each command as a tool call of its own.
const FEATURES_OFF: [&str; 17] = [
    "unified_exec",
    // A crew of Codex's own, whose calls kelpie's checks were not measured on
    "multi_agent",
    "apps",
    "plugins",
    "remote_plugin",
    "browser_use",
    "browser_use_external",
    "computer_use",
    "in_app_browser",
    "image_generation",
    "memories",
    "realtime_conversation",
    "shell_snapshot",
    "skill_mcp_dependency_install",
    "tool_suggest",
    "workspace_dependencies",
    "worktrees",
];

/// Headless Codex, one `codex exec` process per call, each inside the sandbox
///
/// It shares Claude Code's calls in flight, so stopping either stops both.
#[derive(Debug, Clone)]
pub struct CodexCli {
    processes: Processes,
    program: OsString,
    lambs: Option<Arc<dyn LambLabels>>,
    sandbox: Arc<dyn Sandbox>,
    home: PathBuf,
}

impl ClaudeCli {
    /// Codex, run the way this runs Claude Code: same sandbox, same lamb
    /// labels and home, and stopped with it
    pub fn codex(&self) -> CodexCli {
        CodexCli {
            processes: self.processes.clone(),
            program: "codex".into(),
            lambs: self.lambs.clone(),
            sandbox: Arc::clone(&self.sandbox),
            home: self.home.clone(),
        }
    }
}

impl CodexCli {
    /// Runs `program` in place of `codex`, as a stand-in script does
    #[cfg(test)]
    pub(crate) fn with_program(self, program: PathBuf) -> Self {
        Self {
            program: program.into(),
            ..self
        }
    }
}

impl Agents for CodexCli {
    fn prepare(&self, call: &AgentCall) -> Result<(), AgentError> {
        is_codex(call)?;
        refuse_unsupported(call)?;
        let files = Files::of(call);
        for folder in [files.threads(), files.state.clone(), self.sessions()] {
            std::fs::create_dir_all(&folder).map_err(|e| {
                AgentError::Setup(format!("cannot make {}: {}", folder.display(), e.kind()))
            })?;
        }
        Ok(())
    }

    fn run(&self, call: &AgentCall) -> Result<AgentReply, AgentError> {
        let files = Files::of(call);
        let mut command = self.sandboxed_command(call)?;
        let label = format!("#{} {}", call.issue, call.role.as_str());
        let spawned = |pid| {
            if let Some(lambs) = &self.lambs {
                lambs.label(pid, &label);
            }
        };
        let run = self
            .processes
            .output_telling(&mut command, call.timeout, &spawned);
        let output = run.map_err(|e| match e {
            RunError::Io(e) => AgentError::Spawn(CODEX, e.to_string()),
            RunError::Stopped => AgentError::Stopped,
            RunError::TimedOut => AgentError::TimedOut(CODEX),
        })?;
        let known = thread(&files, call.session.id())?;
        let turn = parse_result(&output, known.as_ref())?;
        if known.is_none() {
            let record = Thread { id: turn.thread };
            let path = files.thread(call.session.id());
            let text = serde_json::to_string(&record).expect("a thread is JSON");
            std::fs::write(&path, text).map_err(|e| {
                AgentError::Setup(format!("cannot write {}: {}", path.display(), e.kind()))
            })?;
        }
        Ok(AgentReply {
            session_id: call.session.id().clone(),
            text: turn.text,
            usage: turn.usage,
            session_cost: None,
        })
    }
}

impl CodexCli {
    fn sessions(&self) -> PathBuf {
        self.home.join(".codex").join("sessions")
    }

    fn sandboxed_command(&self, call: &AgentCall) -> Result<Command, AgentError> {
        is_codex(call)?;
        let files = Files::of(call);
        let resumed = match &call.session {
            Session::New(id) => {
                if thread(&files, id)?.is_some() {
                    return Err(AgentError::Setup(format!(
                        "kelpie already started a Codex session as {}",
                        id.0
                    )));
                }
                None
            }
            Session::Resume(id) => match thread(&files, id)? {
                Some(thread) => Some(thread),
                None => return Err(AgentError::NoSession(CODEX, id.clone())),
            },
        };
        let setup = |what: &Path, e: std::io::Error| {
            AgentError::Setup(format!("cannot make {}: {}", what.display(), e.kind()))
        };
        match std::fs::remove_dir_all(&files.scratch) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(setup(&files.scratch, e));
            }
            _ => {}
        }
        std::fs::create_dir_all(&files.scratch).map_err(|e| setup(&files.scratch, e))?;
        let instructions = match &call.instructions {
            Some(path) => Some(std::fs::read_to_string(path).map_err(|e| {
                AgentError::Setup(format!("cannot read {}: {}", path.display(), e.kind()))
            })?),
            None => None,
        };
        let mut command = Command::new(&self.program);
        command
            .args(argv(call, &files, resumed.as_ref(), instructions.as_deref()))
            .current_dir(&call.cwd)
            .env("TMPDIR", &files.scratch);
        if let Some(fence) = &call.reach.fence {
            command.envs(&fence.env);
        }
        self.sandbox
            .wrap(
                &policy(call, &self.home, &files),
                &files.sandbox_settings,
                &command,
            )
            .map_err(|e| AgentError::Setup(e.to_string()))
    }
}

/// Fails a call that names another harness
fn is_codex(call: &AgentCall) -> Result<(), AgentError> {
    match call.harness {
        AgentHarness::Codex => Ok(()),
        _ => Err(AgentError::Setup(
            "kelpie routed a call to codex that names another harness".into(),
        )),
    }
}

// What a worker on Claude Code has and Codex here does not give it.
fn refuse_unsupported(call: &AgentCall) -> Result<(), AgentError> {
    if call.mcp_config.is_some() {
        return Err(AgentError::Setup(
            "kelpie bridges MCP servers to Claude Code only, so a worker on codex \
             cannot run the preview: turn `preview` off or put the worker on Claude Code"
                .into(),
        ));
    }
    if call
        .reach
        .fence
        .as_ref()
        .is_some_and(|f| !f.hooks.is_empty())
    {
        return Err(AgentError::Setup(
            "kelpie does not run `worker.guard_hooks` on codex: remove them or put \
             the worker on Claude Code"
                .into(),
        ));
    }
    Ok(())
}

/// The files one call keeps beside its settings path
struct Files {
    /// Kelpie's own folder for the call's Codex state: its sessions' ids,
    /// and Codex's database and logs
    state: PathBuf,
    /// The call's temporary files, emptied before each call
    scratch: PathBuf,
    /// Where the sandbox's settings go
    sandbox_settings: PathBuf,
}

impl Files {
    fn of(call: &AgentCall) -> Self {
        Self {
            state: call.settings.with_extension("codex"),
            scratch: call.settings.with_extension("tmp"),
            sandbox_settings: call.settings.with_extension("sandbox.json"),
        }
    }

    fn threads(&self) -> PathBuf {
        self.state.join("threads")
    }

    fn thread(&self, id: &SessionId) -> PathBuf {
        self.threads().join(format!("{}.json", id.0))
    }
}

/// The Codex session one of kelpie's session ids is
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Thread {
    /// Codex's own id for it
    id: String,
}

/// The Codex session kelpie started as `id`, if any
fn thread(files: &Files, id: &SessionId) -> Result<Option<Thread>, AgentError> {
    let plain = !id.0.is_empty()
        && id
            .0
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !plain {
        return Err(AgentError::Setup(format!(
            "{} is not a session id kelpie makes",
            id.0
        )));
    }
    let path = files.thread(id);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(AgentError::Setup(format!(
                "cannot read {}: {}",
                path.display(),
                e.kind()
            )));
        }
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| AgentError::Setup(format!("cannot read {}: {e}", path.display())))
}

/// `key=value` for `-c`, with the value written as TOML
fn config(key: &str, value: impl Into<toml::Value>) -> OsString {
    format!("{key}={}", value.into()).into()
}

// Codex loads nothing of the maintainer's: no config, no rules, no skills
// or instructions from its home. It does read the repo's AGENTS.md.
fn argv(
    call: &AgentCall,
    files: &Files,
    resumed: Option<&Thread>,
    instructions: Option<&str>,
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec!["exec".into()];
    if resumed.is_some() {
        argv.push("resume".into());
    }
    argv.extend(
        [
            "--json",
            "--ignore-user-config",
            "--ignore-rules",
            "--skip-git-repo-check",
            // Kelpie's sandbox holds the whole call, and Codex's cannot start inside it.
            "--dangerously-bypass-approvals-and-sandbox",
            "--model",
        ]
        .map(OsString::from),
    );
    argv.push(call.model.as_str().into());
    let state = |name: &str| files.state.join(name).to_string_lossy().into_owned();
    argv.extend(
        [
            // Codex's levels share Claude Code's names; a model refuses one it lacks.
            config("model_reasoning_effort", call.effort.as_str()),
            config("web_search", "disabled"),
            config("check_for_update_on_startup", false),
            config("sqlite_home", state("db")),
            config("log_dir", state("log")),
        ]
        .into_iter()
        .flat_map(|c| [OsString::from("-c"), c]),
    );
    if let Some(text) = instructions {
        argv.extend(["-c".into(), config("developer_instructions", text)]);
    }
    let mut off: Vec<&str> = FEATURES_OFF.into();
    if call.tools == Tools::Answer {
        off.push("shell_tool");
    }
    for feature in off {
        argv.extend(["--disable".into(), feature.into()]);
    }
    if let Some(fence) = &call.reach.fence {
        // Codex runs a hook only once it is trusted; kelpie wrote these itself.
        argv.push("--dangerously-bypass-hook-trust".into());
        argv.extend(["-c".into(), config("hooks.PreToolUse", hooks(fence))]);
    }
    argv.push("--".into());
    if let Some(thread) = resumed {
        argv.push(thread.id.as_str().into());
    }
    argv.push(prompt(call).into());
    argv
}

/// The prompt as Codex takes it
///
/// Kelpie starts a step's prompt with Claude Code's `/<plugin>:<skill>`.
/// Codex is told to follow that skill's file, which its sandbox reads.
fn prompt(call: &AgentCall) -> String {
    match step_skill(call) {
        Some(step) => format!(
            "Follow the skill `{}` in {}: read it first, then do what it says for this.\n\n{}",
            step.name,
            step.dir.join("SKILL.md").display(),
            step.rest
        ),
        None => call.prompt.clone(),
    }
}

/// The `PreToolUse` hooks that run kelpie's checks, as Codex's config takes them
///
/// `apply_patch` is how Codex writes files, and `Bash` is every command.
fn hooks(fence: &Fence) -> toml::Value {
    let guard = &fence.guard;
    let quoted = |p: &Path| shell_quote(&p.to_string_lossy());
    let confine = [
        quoted(&guard.kelpie),
        shell_quote("confine"),
        quoted(&guard.worktree),
        quoted(&guard.build),
    ]
    .join(" ");
    let commands: Vec<String> = [
        quoted(&guard.kelpie),
        shell_quote("guard"),
        quoted(&guard.git_common_dir),
        quoted(&guard.worktree),
    ]
    .into_iter()
    .chain(
        guard
            .folders
            .iter()
            .map(|p| shell_quote(&format!("{FOLDER_FLAG}{}", p.display()))),
    )
    .chain(
        guard
            .private_names
            .iter()
            .map(|n| shell_quote(&format!("{NAME_FLAG}{n}"))),
    )
    .collect();
    let entry = |matcher: &str, command: String| {
        let mut hook = toml::Table::new();
        hook.insert("type".into(), "command".into());
        hook.insert("command".into(), command.into());
        let mut entry = toml::Table::new();
        entry.insert("matcher".into(), matcher.into());
        entry.insert("hooks".into(), toml::Value::Array(vec![hook.into()]));
        toml::Value::Table(entry)
    };
    toml::Value::Array(vec![
        entry("^apply_patch$", confine),
        entry("^Bash$", commands.join(" ")),
    ])
}

/// The whole call's policy: its fence, or none, and what Codex itself needs
fn policy(call: &AgentCall, home: &Path, files: &Files) -> Policy {
    let mut policy = match &call.reach.fence {
        Some(fence) => fence_policy(fence),
        None => Policy {
            no_read: CREDENTIALS.map(str::to_owned).into(),
            ..Policy::default()
        },
    };
    policy.no_read.extend(fence::others_credentials(HARNESS));
    policy
        .no_read
        .extend([HOME, SKILLS].map(str::to_owned));
    let codex = home.join(".codex");
    let sessions = codex.join("sessions");
    policy
        .write
        .extend([files.state.clone(), files.scratch.clone(), sessions.clone()]);
    // Codex reads its login to run; kelpie never does.
    policy.read.extend(
        [codex.join("auth.json"), sessions, files.state.clone(), files.scratch.clone()]
            .into_iter()
            .chain(call.plugin_dirs.iter().cloned())
            .chain(call.reach.read.iter().cloned()),
    );
    policy.hosts.push(MODEL_HOST.to_owned());
    policy
}

/// How much of Codex's output an error carries: its end, where the reason is
const ERROR_TAIL: usize = 2048;

// The last `ERROR_TAIL` bytes of `text`, since a worker's output runs to
// megabytes and an error reaches a ruling, the state file and the webhook.
fn tail(text: &str) -> &str {
    let mut start = text.len().saturating_sub(ERROR_TAIL);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// One `codex exec` turn, read from what it printed
#[derive(Debug, Clone, PartialEq, Eq)]
struct Turn {
    /// The Codex session it ran in
    thread: String,
    /// Its last message's text
    text: String,
    /// What the turn used
    usage: Usage,
}

// `codex exec --json` prints one event per line: the session it runs in,
// each item as it starts and ends, and the turn's end with its tokens or
// its failure. The answer is the last agent message. A ChatGPT plan has
// no price per call, so kelpie records each one as unpriced.
fn parse_result(output: &Output, known: Option<&Thread>) -> Result<Turn, AgentError> {
    #[derive(Deserialize)]
    struct Event {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        thread_id: Option<String>,
        #[serde(default)]
        item: Option<Item>,
        #[serde(default)]
        usage: Option<TurnUsage>,
        #[serde(default)]
        error: Option<Failure>,
        #[serde(default)]
        message: Option<String>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        text: Option<String>,
    }
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct TurnUsage {
        input_tokens: u64,
        cached_input_tokens: u64,
        output_tokens: u64,
    }
    #[derive(Deserialize)]
    struct Failure {
        message: String,
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut thread = known.map(|t| t.id.clone());
    let mut text = None;
    let mut usage = None;
    let mut failure = None;
    let mut error = None;
    for line in stdout.lines().filter(|l| l.starts_with('{')) {
        let Ok(event) = serde_json::from_str::<Event>(line) else {
            continue;
        };
        match event.kind.as_str() {
            "thread.started" => {
                if let (Some(started), Some(known)) = (&event.thread_id, known)
                    && *started != known.id
                {
                    return Err(AgentError::Unreadable(
                        CODEX,
                        format!("codex answered in session {started}, not {}", known.id),
                    ));
                }
                thread = thread.or(event.thread_id);
            }
            "item.completed" => {
                if let Some(item) = event.item.filter(|i| i.kind == "agent_message") {
                    text = item.text;
                }
            }
            "turn.completed" => {
                let u = event.usage.unwrap_or_default();
                usage = Some(Usage {
                    // Codex counts cached input within its input.
                    input: u.input_tokens.saturating_sub(u.cached_input_tokens),
                    cache_write: 0,
                    cache_read: u.cached_input_tokens,
                    output: u.output_tokens,
                });
            }
            "turn.failed" => failure = event.error.map(|e| e.message),
            "error" => error = event.message.or(event.error.map(|e| e.message)),
            _ => {}
        }
    }
    let failed = |detail: &str| AgentError::Failed(CODEX, detail.to_owned());
    // A turn that ended well is a success whatever retries came before it.
    if let Some(why) = failure.or(usage.is_none().then_some(error).flatten()) {
        return Err(failed(&why));
    }
    if !output.status.success() {
        return Err(failed(tail(&stderr)));
    }
    match (thread, text, usage) {
        (Some(thread), Some(text), Some(usage)) => Ok(Turn {
            thread,
            text,
            usage,
        }),
        _ => Err(AgentError::Unreadable(CODEX, tail(&stdout).to_owned())),
    }
}
