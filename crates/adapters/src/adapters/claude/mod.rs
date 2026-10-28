//! Headless Claude Code, one `claude -p` process per call

use std::ffi::OsString;
use std::fmt;
use std::path::Path;
#[cfg(any(test, feature = "test-support"))]
use std::path::PathBuf;
use std::process::{Command, Output};

use serde::Deserialize;

use super::cli::{Cli, CliAdapter, Launch, Turn};
use super::harness::{Base, prompt_file};
use crate::forwarder::Upstream;
use crate::ports::{
    Adopted, AgentCall, AgentError, AgentReply, CallActivity, Cost, Policy, Role, Session,
    SessionId, Transcript, Usage, written_at,
};
use crate::settings::Harness;

/// This adapter's harness, as its errors name it
const CLAUDE: Harness = Harness::ClaudeCode;

#[cfg(test)]
mod calls;
#[cfg(all(test, unix))]
pub(super) mod ecosystems;
#[cfg(test)]
mod escapes;
mod graph;
mod mcp;
pub(crate) mod sandbox;
pub(crate) mod settings;

/// What `claude -p --resume` prints when the session has no transcript
const NO_SESSION: &str = "No conversation found with session ID";

/// How to give `claude` the login every call needs, as doctor and a refused call say it
pub const LOGIN_FIX: &str = "run `claude setup-token`, then `shep secret set CLAUDE_CODE_OAUTH_TOKEN` and set \
     `CLAUDE_CODE_OAUTH_TOKEN = \"{{secret:CLAUDE_CODE_OAUTH_TOKEN}}\"` in the runner's `env`";

/// The variable Claude Code reads a login from, in place of the one it keeps per config folder
pub const LOGIN_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// The login `claude setup-token` makes: one token, not the keychain's full login
///
/// Each call keeps its own config folder, which holds no login, and a blocked
/// keychain would hide the maintainer's. The maintainer keeps this one with
/// `shep secret`, and a runner hands it to `claude` alone.
#[derive(Clone, PartialEq, Eq)]
pub struct LoginToken(String);

impl LoginToken {
    /// The token `value` is, or `None` when it is empty
    pub fn new(value: String) -> Option<Self> {
        (!value.trim().is_empty()).then(|| Self(value.trim().to_owned()))
    }

    /// Takes the token from this process's environment, and keeps the variable
    /// out of every process started from now on, `claude`'s apart
    pub fn take() -> Option<Self> {
        let token = std::env::var(LOGIN_VAR).ok().and_then(Self::new);
        crate::spawn::hide([LOGIN_VAR.to_owned()]);
        token
    }

    pub(super) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for LoginToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LoginToken(..)")
    }
}

/// What is true of Claude Code: the part of a headless `claude -p` call that no other CLI shares
#[derive(Debug, Clone)]
pub struct Claude {
    program: OsString,
    // The login `claude` runs with, in place of the maintainer's in the keychain
    login: Option<LoginToken>,
    // Where Claude Code keeps its folders
    home: std::path::PathBuf,
    // What a call with no fence may not read, besides its own folder
    unfenced_no_read: Vec<String>,
}

/// Headless Claude Code, one `claude -p` process per call, each inside the sandbox
///
/// Clones share their calls in flight, so one clone can stop them all.
pub type ClaudeCli = CliAdapter<Claude>;

impl Default for ClaudeCli {
    fn default() -> Self {
        Self::on(&Base::default())
    }
}

impl ClaudeCli {
    /// Claude Code on `base`: its sandbox, lamb labels and calls in flight
    pub fn on(base: &Base) -> Self {
        let claude = Claude {
            program: "claude".into(),
            login: None,
            home: base.home.clone(),
            unfenced_no_read: base.unfenced_no_read.clone(),
        };
        Self::new(claude, base)
    }

    /// Runs `program` in place of `claude`, as a stand-in script does
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_program(mut self, program: PathBuf) -> Self {
        self.cli.program = program.into();
        self
    }

    /// Runs `claude` with `login` in its environment, where there is one, in
    /// place of the keychain's login
    #[must_use]
    pub fn with_login(mut self, login: Option<LoginToken>) -> Self {
        self.cli.login = login;
        self
    }

    /// The call's headless command inside its sandbox, its scratch folder emptied
    #[cfg(test)]
    pub(crate) fn sandboxed_command(&self, call: &AgentCall) -> Result<Command, AgentError> {
        self.launch(call, &(), Turn::Headless, None)
            .map(|l| l.command)
    }

    /// The call's session for the maintainer's terminal, inside the same sandbox
    #[cfg(test)]
    pub(crate) fn terminal_command(&self, call: &AgentCall) -> Result<Command, AgentError> {
        self.launch(call, &(), Turn::Terminal, None)
            .map(|l| l.command)
    }
}

impl Cli for Claude {
    type Begun = ();
    const HARNESS: Harness = Harness::ClaudeCode;

    fn write_config(&self, call: &AgentCall, _: Option<&Upstream>) -> Result<(), AgentError> {
        write_settings(call, &self.unfenced_no_read)?;
        sandbox::policy(
            call,
            &self.home,
            Path::new(&self.program),
            &self.unfenced_no_read,
        )
        .map(drop)
    }

    // A session from before the config folder moved is resumed from a copy of its
    // transcript, which the maintainer's `~/.claude` keeps as it was.
    fn begin(&self, call: &AgentCall, _: Turn) -> Result<(), AgentError> {
        let Session::Resume(id) = &call.session else {
            return Ok(());
        };
        let new = sandbox::transcripts(&sandbox::config_folder(call), &call.cwd)?;
        let old = sandbox::transcripts(&self.home.join(".claude"), &call.cwd)?;
        let file = format!("{}.jsonl", id.0);
        if new.join(&file).exists() || !old.join(&file).is_file() {
            return Ok(());
        }
        let copied = std::fs::create_dir_all(&new)
            .and_then(|()| std::fs::copy(old.join(&file), new.join(&file)).map(drop));
        copied.map_err(|e| {
            AgentError::Setup(format!(
                "cannot copy {} to {}: {}",
                old.join(&file).display(),
                new.display(),
                e.kind()
            ))
        })
    }

    // `claude` keeps its login per config folder, so none a call could use is there
    // and it would start and fail with its own "not logged in", which a ruling
    // would carry, in place of the fix.
    fn ready(&self, _: &AgentCall) -> Result<(), AgentError> {
        if self.login.is_none() {
            return Err(AgentError::Setup(format!(
                "{LOGIN_VAR} is not set in the runner's environment, so claude has no login: \
                 each call keeps its own config folder, where the keychain's is not: {LOGIN_FIX}"
            )));
        }
        Ok(())
    }

    fn scratch(&self, call: &AgentCall) -> std::path::PathBuf {
        sandbox::scratch(call)
    }

    fn sandbox_settings(&self, call: &AgentCall) -> std::path::PathBuf {
        sandbox::sandbox_settings(call)
    }

    fn folders(&self, call: &AgentCall) -> Result<Vec<std::path::PathBuf>, AgentError> {
        Ok(vec![sandbox::transcripts(
            &sandbox::config_folder(call),
            &call.cwd,
        )?])
    }

    fn command(
        &self,
        call: &AgentCall,
        _: &(),
        launch: &Launch<'_>,
    ) -> Result<Command, AgentError> {
        let mut command = match launch.turn {
            Turn::Headless => self.program_command(call, argv(call)),
            Turn::Terminal => self.program_command(call, terminal_argv(call)),
        };
        if launch.turn == Turn::Headless {
            // A headless turn that ends is over, so nothing it waits on in
            // the background can ever wake it.
            command.env("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1");
        }
        Ok(command)
    }

    fn policy(&self, call: &AgentCall, _: &Launch<'_>) -> Result<Policy, AgentError> {
        sandbox::policy(
            call,
            &self.home,
            Path::new(&self.program),
            &self.unfenced_no_read,
        )
    }

    fn parse(&self, call: &AgentCall, _: (), output: &Output) -> Result<AgentReply, AgentError> {
        let mut reply = parse_result(output, &call.session)?;
        reply.compactions = compactions(call);
        Ok(reply)
    }

    fn parse_adopted(
        &self,
        adopted: &Adopted,
        _: (),
        output: &Output,
    ) -> Result<AgentReply, AgentError> {
        let session = match &adopted.resumes {
            Some(id) => Session::Resume(id.clone()),
            None => Session::New(SessionId(String::new())),
        };
        parse_result(output, &session)
    }

    fn transcript(&self, call: &AgentCall) -> Option<Transcript> {
        let folder = sandbox::transcripts(&sandbox::config_folder(call), &call.cwd).ok()?;
        Some(Transcript {
            folder,
            threads: None,
        })
    }

    // Claude Code appends to the session's transcript at each message and
    // tool call, so a write is the session doing something.
    fn last_active(&self, call: &AgentCall) -> CallActivity {
        let Ok(folder) = sandbox::transcripts(&sandbox::config_folder(call), &call.cwd) else {
            return CallActivity::Untracked;
        };
        written_at(&folder.join(format!("{}.jsonl", call.session.id().0)))
    }
}

/// Writes the call's settings file whole, from the call alone
pub fn write_settings(call: &AgentCall, unfenced: &[String]) -> Result<(), AgentError> {
    let mut value = settings::settings(call.tools, &call.reach);
    // The prompt beside the settings file, for the hook that hands it back.
    let again = prompt_file(&call.settings);
    if let Some(window) = call.compact_window {
        let marks = settings::compactions_file(&sandbox::scratch(call));
        settings::compacting(&mut value, window, &again, &marks);
    }
    mcp::allow(&mut value, call);
    let reads = settings::unfenced_reads(call, unfenced);
    if let Some(deny) = value["permissions"]["deny"].as_array_mut() {
        deny.extend(reads.into_iter().map(serde_json::Value::from));
    }
    let text = serde_json::to_string_pretty(&value).expect("settings are JSON");
    let folder = call.settings.parent().unwrap_or(std::path::Path::new("/"));
    let write = |path: &std::path::Path, text: String| {
        std::fs::write(path, text).map_err(|e| {
            AgentError::Setup(format!("cannot write {}: {}", path.display(), e.kind()))
        })
    };
    std::fs::create_dir_all(folder).map_err(|e| {
        AgentError::Setup(format!(
            "cannot write {}: {}",
            call.settings.display(),
            e.kind()
        ))
    })?;
    write(&call.settings, text)?;
    if let Some(config) = mcp::config(call) {
        let text = serde_json::to_string_pretty(&config).expect("an MCP config is JSON");
        write(&mcp::mcp_file(&call.settings), text)?;
    }
    match call.compact_window {
        Some(_) => write(&again, compacted_prompt(&call.prompt)),
        None => Ok(()),
    }
}

// What a compaction's hook prints into the session: the turn's prompt again.
fn compacted_prompt(prompt: &str) -> String {
    format!(
        "Claude Code compacted your session partway through this turn, so what came before is \
         a summary, and the prompt may have been cut from it. This is the prompt again:\n\n\
         {prompt}\n"
    )
}

// `--setting-sources project` keeps the project's CLAUDE.md and skills and
// drops the maintainer's own hooks, plugins and skills. It also drops the
// worktree's `settings.local.json`, which nothing kelpie runs needs.
fn argv(call: &AgentCall) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec!["-p".into(), call.prompt.as_str().into()];
    argv.extend(profile(call));
    argv.extend(["--output-format".into(), "json".into()]);
    // Only a worker's working call is let edit; a retro changes nothing.
    if call.role == Role::Worker && call.tools == crate::ports::Tools::Work {
        argv.extend(["--permission-mode".into(), "bypassPermissions".into()]);
    }
    argv.extend(session(call));
    argv
}

// The same session in the maintainer's terminal: Claude Code asks before
// each tool call as it always does, and a prompt is its first message.
fn terminal_argv(call: &AgentCall) -> Vec<OsString> {
    let mut argv = profile(call);
    argv.extend(session(call));
    if !call.prompt.is_empty() {
        argv.push(call.prompt.as_str().into());
    }
    argv
}

fn profile(call: &AgentCall) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "--model".into(),
        call.model.as_str().into(),
        "--effort".into(),
        call.effort.as_str().into(),
        "--setting-sources".into(),
        "project".into(),
        "--settings".into(),
        call.settings.clone().into(),
    ];
    // Every kind of call but an answer's runs on an allow-list, and loads no
    // MCP server but those kelpie names, so what a project's settings allow
    // adds nothing. An agent file's own list replaces a worker's and a
    // reviewer's, and no other's.
    use crate::ports::Tools as Kind;
    let own = call.agent_tools.as_deref();
    let tools = match (call.role, call.tools) {
        (_, Kind::Retro) => Some(settings::RETRO_TOOLS),
        (Role::Pm, _) => Some(settings::PM_TOOLS),
        (_, Kind::Work) => Some(own.unwrap_or(settings::WORK_TOOLS)),
        (_, Kind::Review) => Some(own.unwrap_or(crate::settings::CLAUDE_REVIEW_TOOLS)),
        (_, Kind::Answer | Kind::Pm) => None,
    };
    if crate::ports::mcp::loads(call) {
        argv.extend(["--mcp-config".into(), mcp::mcp_file(&call.settings).into()]);
    }
    if let Some(tools) = tools {
        argv.extend(["--tools".into(), tools.into()]);
        // Only the config named above is loaded, so its servers are the call's MCP
        // servers.
        argv.push("--strict-mcp-config".into());
    }
    argv
}

fn session(call: &AgentCall) -> Vec<OsString> {
    let mut argv = Vec::new();
    for plugin in &call.plugin_dirs {
        argv.extend(["--plugin-dir".into(), plugin.into()]);
    }
    match &call.session {
        Session::New(id) => {
            argv.extend(["--session-id".into(), id.0.as_str().into()]);
            if let Some(instructions) = &call.instructions {
                argv.extend([
                    "--append-system-prompt-file".into(),
                    instructions.clone().into(),
                ]);
            }
        }
        Session::Resume(id) => argv.extend(["--resume".into(), id.0.as_str().into()]),
    }
    argv
}

// `claude -p` exits 1 on an error result but still prints its JSON, so the
// JSON is read before the exit status.
fn parse_result(output: &Output, session: &Session) -> Result<AgentReply, AgentError> {
    #[derive(Deserialize)]
    struct ResultMessage {
        is_error: bool,
        session_id: String,
        #[serde(default)]
        result: String,
        #[serde(default)]
        usage: Option<ResultUsage>,
        total_cost_usd: Option<f64>,
    }
    #[derive(Deserialize)]
    struct ResultUsage {
        input_tokens: u64,
        cache_creation_input_tokens: u64,
        cache_read_input_tokens: u64,
        output_tokens: u64,
        // The cache writes split by how long they are kept, where reported
        #[serde(default)]
        cache_creation: Option<CacheCreation>,
        // One entry per model request, the last one's input being the context
        #[serde(default)]
        iterations: Vec<Iteration>,
    }
    #[derive(Deserialize)]
    struct CacheCreation {
        #[serde(default)]
        ephemeral_5m_input_tokens: u64,
    }
    #[derive(Deserialize)]
    struct Iteration {
        input_tokens: u64,
        cache_creation_input_tokens: u64,
        cache_read_input_tokens: u64,
    }
    let stdout = || String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let r = match serde_json::from_slice::<ResultMessage>(&output.stdout) {
        Ok(r) if !r.is_error && output.status.success() => r,
        Ok(r) => return Err(AgentError::Failed(CLAUDE, r.result)),
        Err(_) if output.status.success() => return Err(AgentError::Unreadable(CLAUDE, stdout())),
        Err(_) => {
            return Err(match session {
                Session::Resume(id) if stderr.contains(NO_SESSION) => {
                    AgentError::NoSession(CLAUDE, id.clone())
                }
                _ => AgentError::Failed(CLAUDE, stderr.into_owned()),
            });
        }
    };
    let (Some(u), Some(cost)) = (r.usage, r.total_cost_usd.and_then(Cost::from_usd)) else {
        return Err(AgentError::Unreadable(CLAUDE, stdout()));
    };
    let context = (u.iterations.last()).map(|last| {
        last.input_tokens + last.cache_creation_input_tokens + last.cache_read_input_tokens
    });
    Ok(AgentReply {
        session_id: SessionId(r.session_id),
        text: r.result,
        usage: Usage {
            input: u.input_tokens,
            cache_write: u.cache_creation_input_tokens,
            cache_write_5m: (u.cache_creation.map_or(0, |c| c.ephemeral_5m_input_tokens))
                .min(u.cache_creation_input_tokens),
            cache_read: u.cache_read_input_tokens,
            output: u.output_tokens,
        },
        session_cost: Some(cost),
        context,
        compactions: 0,
    })
}

// The compactions Claude Code did in the call: one mark for each, left by the hook that hands
// the prompt back. A call another image adopted is not counted, its marks out of reach.
fn compactions(call: &AgentCall) -> u32 {
    let marks = settings::compactions_file(&sandbox::scratch(call));
    let len = std::fs::metadata(marks).map_or(0, |m| m.len());
    u32::try_from(len).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;
