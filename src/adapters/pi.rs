//! pi, headless as `pi -p`, one process per call, on a model an
//! OpenAI-compatible server runs
//!
//! Each call runs whole inside kelpie's sandbox, as Claude Code's do. pi has
//! no sandbox of its own and no permission prompts. Kelpie gives it a home of
//! its own beside the call's settings, holding the one model the call runs
//! and its sessions, so the maintainer's own pi setup never loads. A fenced
//! call also loads an extension that runs kelpie's checks before each
//! command and file write. pi starts no MCP servers. The sandbox allows no
//! model host: pi's calls go to a forwarder outside it, which passes the
//! server only the chat completions call.

use std::ffi::OsString;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::json;

use super::claude::sandbox::fence_policy;
use super::claude::{ClaudeCli, LambLabels};
use super::forwarder::Forwarder;
use super::process::{Processes, RunError};
use crate::fence;
use crate::forwarder::{Upstream, WORKER_HOST};
use crate::guard::{FOLDER_FLAG, NAME_FLAG};
use crate::ports::{
    AgentCall, AgentError, AgentReply, Agents, Fence, Forward, Policy, Sandbox, Session, SessionId,
    Tools, Usage,
};
use crate::profile::CREDENTIALS;
use crate::settings::{AgentHarness, Harness, ModelServer};
use crate::skills::split_command;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod escapes;

/// This adapter's harness, as its errors and the fence name it
const PI: Harness = Harness::Pi;

/// The provider kelpie's own `models.json` names, for the call's one model
const PROVIDER: &str = "kelpie";

/// The extension that runs kelpie's checks, with `__CHECKS__` to fill in
const GUARD: &str = include_str!("pi/guard.ts");

/// pi's tools for each kind, by its own names
fn tool_names(tools: Tools, reads: bool) -> Option<&'static str> {
    match tools {
        Tools::Work => Some("read,bash,edit,write,grep,find,ls"),
        Tools::Review => Some("read,grep,find,ls"),
        Tools::Answer if reads => Some("read"),
        Tools::Answer => None,
    }
}

/// Headless pi, one `pi -p` process per call, each inside the sandbox
///
/// It shares Claude Code's calls in flight, so stopping either stops both.
#[derive(Debug, Clone)]
pub struct PiCli {
    processes: Processes,
    program: OsString,
    lambs: Option<Arc<dyn LambLabels>>,
    sandbox: Arc<dyn Sandbox>,
}

impl ClaudeCli {
    /// pi, run the way this runs Claude Code: same sandbox, same lamb labels,
    /// and stopped with it
    pub fn pi(&self) -> PiCli {
        PiCli {
            processes: self.processes.clone(),
            program: "pi".into(),
            lambs: self.lambs.clone(),
            sandbox: Arc::clone(&self.sandbox),
        }
    }
}

impl PiCli {
    /// Runs `program` in place of `pi`, as a stand-in script does
    #[cfg(test)]
    pub(crate) fn with_program(self, program: PathBuf) -> Self {
        Self {
            program: program.into(),
            ..self
        }
    }
}

impl Agents for PiCli {
    fn prepare(&self, call: &AgentCall) -> Result<(), AgentError> {
        let server = server(call)?;
        let upstream = upstream(server)?;
        refuse_unsupported(call)?;
        let files = Files::of(call);
        let setup = |what: &Path, e: std::io::Error| {
            AgentError::Setup(format!("cannot write {}: {}", what.display(), e.kind()))
        };
        std::fs::create_dir_all(files.sessions()).map_err(|e| setup(&files.sessions(), e))?;
        let models = json!({ "providers": { PROVIDER: {
            "baseUrl": upstream.worker_url(),
            "api": "openai-completions",
            // The server ignores it, and pi lists no model without one.
            "apiKey": PROVIDER,
            "models": [{
                "id": call.model,
                "contextWindow": server.context.get(),
                "reasoning": true,
            }],
        } } });
        let text = serde_json::to_string_pretty(&models).expect("models are JSON");
        let written = [
            (files.home.join("models.json"), text.as_str()),
            // Written here, pi only reads them, and locks them beside them.
            (files.home.join("auth.json"), "{}"),
            (files.home.join("models-store.json"), "{}"),
        ];
        for (path, text) in written {
            std::fs::write(&path, text).map_err(|e| setup(&path, e))?;
        }
        if let Some(fence) = &call.reach.fence {
            let guard = files.guard();
            std::fs::write(&guard, guard_extension(fence)).map_err(|e| setup(&guard, e))?;
        }
        Ok(())
    }

    fn run(&self, call: &AgentCall) -> Result<AgentReply, AgentError> {
        // The forwarder stays open until the call has ended.
        let (mut command, _forwarder) = self.sandboxed_command(call)?;
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
            RunError::Io(e) => AgentError::Spawn(PI, e.to_string()),
            RunError::Stopped => AgentError::Stopped,
            RunError::TimedOut => AgentError::TimedOut(PI),
        })?;
        parse_result(&output, call.session.id())
    }
}

impl PiCli {
    fn sandboxed_command(&self, call: &AgentCall) -> Result<(Command, Forwarder), AgentError> {
        let upstream = upstream(server(call)?)?;
        let files = Files::of(call);
        if let Session::Resume(id) = &call.session
            && session_file(&files.sessions(), id).is_none()
        {
            return Err(AgentError::NoSession(PI, id.clone()));
        }
        match std::fs::remove_dir_all(&files.scratch) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(AgentError::Setup(format!(
                    "cannot empty {}: {}",
                    files.scratch.display(),
                    e.kind()
                )));
            }
            _ => {}
        }
        std::fs::create_dir_all(&files.scratch).map_err(|e| {
            AgentError::Setup(format!(
                "cannot make {}: {}",
                files.scratch.display(),
                e.kind()
            ))
        })?;
        let folder = call.settings.parent().unwrap_or(Path::new("/"));
        let forwarder = Forwarder::open(folder, upstream).map_err(AgentError::Setup)?;
        // `env` runs inside the sandbox, after the sandbox has set its own variables.
        let mut command = Command::new("env");
        command
            .args(env_args(call, &files))
            .arg(&self.program)
            .args(argv(call, &files))
            .current_dir(&call.cwd);
        let policy = policy(call, &files, &forwarder.socket);
        let wrapped = self
            .sandbox
            .wrap(&policy, &files.sandbox_settings, &command)
            .map_err(|e| AgentError::Setup(e.to_string()))?;
        Ok((wrapped, forwarder))
    }
}

/// The server the call's model is on, which every pi call needs
fn server(call: &AgentCall) -> Result<&ModelServer, AgentError> {
    match &call.harness {
        AgentHarness::Pi(server) => Ok(server),
        _ => Err(AgentError::Setup(
            "kelpie routed a call to pi that names no model server".into(),
        )),
    }
}

/// What the forwarder passes chat calls to, for the call's model server
fn upstream(server: &ModelServer) -> Result<Upstream, AgentError> {
    Upstream::new(&server.url).map_err(|e| AgentError::Setup(e.to_string()))
}

// What a worker on Claude Code has and pi cannot give it.
fn refuse_unsupported(call: &AgentCall) -> Result<(), AgentError> {
    if call.mcp_config.is_some() {
        return Err(AgentError::Setup(
            "pi runs no MCP servers, so a worker on pi cannot run the preview: \
             turn `preview` off or put the worker on Claude Code"
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
            "pi cannot run `worker.guard_hooks`, which are Claude Code hooks: \
             remove them or put the worker on Claude Code"
                .into(),
        ));
    }
    Ok(())
}

/// The files one call keeps beside its settings path
struct Files {
    /// pi's own home for the call: the model, and the sessions folder
    home: PathBuf,
    /// The call's temporary files, emptied before each call
    scratch: PathBuf,
    /// Where the sandbox's settings go
    sandbox_settings: PathBuf,
    /// The guard extension, outside anything the call may write
    guard_path: PathBuf,
}

impl Files {
    fn of(call: &AgentCall) -> Self {
        Self {
            home: call.settings.with_extension("pi"),
            scratch: call.settings.with_extension("tmp"),
            sandbox_settings: call.settings.with_extension("sandbox.json"),
            guard_path: call.settings.with_extension("guard.ts"),
        }
    }

    fn sessions(&self) -> PathBuf {
        self.home.join("sessions")
    }

    fn guard(&self) -> PathBuf {
        self.guard_path.clone()
    }

    // pi takes a lock folder beside each store it reads at start, even when
    // it changes nothing there.
    fn locks(&self) -> [PathBuf; 2] {
        ["auth.json.lock", "models-store.json.lock"].map(|lock| self.home.join(lock))
    }
}

/// The session file pi keeps for `id`, named `<time>_<id>.jsonl`
fn session_file(sessions: &Path, id: &SessionId) -> Option<PathBuf> {
    let suffix = format!("_{}.jsonl", id.0);
    std::fs::read_dir(sessions)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
        })
}

fn env_args(call: &AgentCall, files: &Files) -> Vec<OsString> {
    let mut vars: Vec<(String, OsString)> = vec![
        // pi is Node, whose fetch ignores the sandbox's proxy without it.
        ("NODE_USE_ENV_PROXY".into(), "1".into()),
        ("PI_CODING_AGENT_DIR".into(), files.home.clone().into()),
        ("TMPDIR".into(), files.scratch.clone().into()),
    ];
    if let Some(fence) = &call.reach.fence {
        vars.extend(
            fence
                .env
                .iter()
                .map(|(name, value)| (name.clone(), value.clone().into())),
        );
    }
    vars.into_iter()
        .map(|(name, value)| {
            let mut pair = OsString::from(format!("{name}="));
            pair.push(value);
            pair
        })
        .collect()
}

// pi loads nothing of the maintainer's or the repo's own: no extensions,
// skills, prompt templates or themes it finds, and no project-local files.
// It does read the repo's AGENTS.md and CLAUDE.md, as Claude Code does.
fn argv(call: &AgentCall, files: &Files) -> Vec<OsString> {
    let (prompt, skill) = skill_prompt(call);
    let mut argv: Vec<OsString> = vec![
        "-p".into(),
        "--mode".into(),
        "json".into(),
        "--model".into(),
        format!("{PROVIDER}/{}", call.model).into(),
        "--thinking".into(),
        call.effort.as_str().into(),
        "--no-extensions".into(),
        "--no-skills".into(),
        "--no-prompt-templates".into(),
        "--no-themes".into(),
        "--no-approve".into(),
        "--offline".into(),
        "--session-dir".into(),
        files.sessions().into(),
        "--session-id".into(),
        call.session.id().0.as_str().into(),
    ];
    match tool_names(call.tools, !call.reach.read.is_empty()) {
        Some(names) => argv.extend(["--tools".into(), names.into()]),
        None => argv.push("--no-tools".into()),
    }
    // pi builds its system prompt afresh on every call, a resumed one too.
    if let Some(instructions) = &call.instructions {
        argv.extend(["--append-system-prompt".into(), instructions.into()]);
    }
    if call.reach.fence.is_some() {
        argv.extend(["--extension".into(), files.guard().into()]);
    }
    if let Some(skill) = skill {
        argv.extend(["--skill".into(), skill.into()]);
    }
    argv.extend(["--".into(), prompt.into()]);
    argv
}

/// The prompt as pi takes it, and the skill folder its slash command runs
///
/// Kelpie starts a step's prompt with Claude Code's `/<plugin>:<skill>`.
/// pi runs a skill it is given as `/skill:<name>`. A skill none of the
/// call's plugins holds leaves the prompt as it is.
fn skill_prompt(call: &AgentCall) -> (String, Option<PathBuf>) {
    match step_skill(call) {
        Some(step) => (format!("/skill:{} {}", step.name, step.rest), Some(step.dir)),
        None => (call.prompt.clone(), None),
    }
}

/// The skill a step's prompt starts with, as Claude Code's `/<plugin>:<skill>`
pub(super) struct StepSkill {
    /// The skill's name
    pub(super) name: String,
    /// Its folder, which holds its `SKILL.md`
    pub(super) dir: PathBuf,
    /// The prompt after the command
    pub(super) rest: String,
}

/// The skill `call`'s prompt runs, from one of its plugins, if any
pub(super) fn step_skill(call: &AgentCall) -> Option<StepSkill> {
    let (Some(command), rest) = split_command(&call.prompt) else {
        return None;
    };
    let (plugin, skill) = command.trim_start_matches('/').split_once(':')?;
    let dir = call
        .plugin_dirs
        .iter()
        .filter(|dir| plugin_name(dir).as_deref() == Some(plugin))
        .map(|dir| dir.join("skills").join(skill))
        .find(|dir| dir.join("SKILL.md").is_file())?;
    Some(StepSkill {
        name: skill.to_owned(),
        dir,
        rest: rest.to_owned(),
    })
}

// A plugin's name, from its manifest
fn plugin_name(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".claude-plugin/plugin.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    manifest["name"].as_str().map(str::to_owned)
}

/// The guard extension's source for a call fenced by `fence`
fn guard_extension(fence: &Fence) -> String {
    let guard = &fence.guard;
    let text = |p: &Path| p.to_string_lossy().into_owned();
    let mut commands = vec![
        "guard".to_owned(),
        text(&guard.git_common_dir),
        text(&guard.worktree),
    ];
    commands.extend(
        guard
            .folders
            .iter()
            .map(|p| format!("{FOLDER_FLAG}{}", p.display())),
    );
    commands.extend(
        guard
            .private_names
            .iter()
            .map(|n| format!("{NAME_FLAG}{n}")),
    );
    let checks = json!({
        "kelpie": text(&guard.kelpie),
        "confine": ["confine", text(&guard.worktree), text(&guard.build)],
        "guard": commands,
    });
    GUARD.replace("__CHECKS__", &checks.to_string())
}

/// The whole call's policy: its fence, or none, and what pi itself needs
///
/// The model's host is not in it. The one host pi may dial is the forwarder's.
fn policy(call: &AgentCall, files: &Files, forwarder: &Path) -> Policy {
    let mut policy = match &call.reach.fence {
        Some(fence) => fence_policy(fence),
        None => Policy {
            no_read: CREDENTIALS.map(str::to_owned).into(),
            ..Policy::default()
        },
    };
    // pi logs in nowhere, so no harness's login is its own, the maintainer's pi included.
    let logins = fence::HARNESSES.iter().flat_map(|own| own.credentials);
    policy.no_read.extend(logins.map(|&path| path.to_owned()));
    policy.write.extend(
        [files.sessions(), files.scratch.clone()]
            .into_iter()
            .chain(files.locks()),
    );
    policy.read.extend(
        [files.home.clone(), files.guard(), files.scratch.clone()]
            .into_iter()
            .chain(call.instructions.iter().cloned())
            .chain(call.plugin_dirs.iter().cloned())
            .chain(call.reach.read.iter().cloned()),
    );
    if let AgentHarness::Pi(server) = &call.harness
        && let Ok(upstream) = Upstream::new(&server.url)
    {
        deny_the_server(&mut policy, &upstream);
    }
    policy.forward = Some(Forward {
        host: WORKER_HOST.to_owned(),
        socket: forwarder.to_owned(),
    });
    policy
}

// Whatever `allowed_domains` says, the sandbox never reaches the model server
// or this machine's loopback by name or by the address the server resolves to.
fn deny_the_server(policy: &mut Policy, upstream: &Upstream) {
    let bracketed = |host: &str| match host.contains(':') {
        true => format!("[{host}]"),
        false => host.to_owned(),
    };
    for host in [upstream.host(), "localhost", "127.0.0.1", "::1"] {
        let host = bracketed(host);
        if !policy.denied_hosts.contains(&host) {
            policy.denied_hosts.push(host);
        }
    }
    let resolved = upstream.address().to_socket_addrs().into_iter().flatten();
    for address in resolved {
        let address = match address.ip() {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address.ip(), IpAddr::V4),
            v4 => v4,
        };
        let address = address.to_string();
        if !policy.denied_addresses.contains(&address) {
            policy.denied_addresses.push(address);
        }
    }
}

/// How much of pi's output an error carries: its end, where the reason is
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

// `pi -p --mode json` prints one event per line: a session header, then
// every message as it ends. The call's usage is the sum of its assistant
// messages, and its answer the last one's text. A local model has no price.
fn parse_result(output: &Output, asked: &SessionId) -> Result<AgentReply, AgentError> {
    #[derive(Deserialize)]
    struct Line {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        message: Option<Message>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Message {
        role: String,
        #[serde(default)]
        content: Vec<Content>,
        #[serde(default)]
        usage: Option<MessageUsage>,
        #[serde(default)]
        stop_reason: Option<String>,
        #[serde(default)]
        error_message: Option<String>,
    }
    #[derive(Deserialize)]
    struct Content {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        text: Option<String>,
    }
    #[derive(Default, Deserialize)]
    #[serde(default, rename_all = "camelCase")]
    struct MessageUsage {
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut session = None;
    let mut usage = Usage::default();
    let mut last: Option<Message> = None;
    // Whether the latest message to end could not be read, so `last` is stale.
    let mut unread = false;
    for text in stdout.lines().filter(|l| l.starts_with('{')) {
        let Ok(line) = serde_json::from_str::<Line>(text) else {
            unread |= text.contains(r#""type":"message_end""#);
            continue;
        };
        if line.kind == "message_end" {
            unread = false;
        }
        match (line.kind.as_str(), line.message) {
            ("session", _) => session = line.id,
            ("message_end", Some(message)) if message.role == "assistant" => {
                if let Some(u) = &message.usage {
                    usage += Usage {
                        input: u.input,
                        cache_write: u.cache_write,
                        cache_read: u.cache_read,
                        output: u.output,
                    };
                }
                last = Some(message);
            }
            _ => {}
        }
    }
    let failed = |detail: &str| AgentError::Failed(PI, detail.to_owned());
    if !output.status.success() {
        let why = last.as_ref().and_then(|m| m.error_message.clone());
        return Err(failed(why.as_deref().unwrap_or(tail(&stderr))));
    }
    let (Some(session), Some(last), false) = (session, last, unread) else {
        return Err(AgentError::Unreadable(PI, tail(&stdout).to_owned()));
    };
    match last.stop_reason.as_deref() {
        Some("stop") => {}
        Some("length") => return Err(failed("the model ran out of output tokens")),
        Some("error" | "aborted") => {
            let why = last.error_message.as_deref();
            return Err(failed(why.unwrap_or("no reason given")));
        }
        other => {
            let why = other.unwrap_or("none");
            return Err(failed(&format!("the model stopped for the reason `{why}`")));
        }
    }
    if session != asked.0 {
        return Err(AgentError::Unreadable(
            PI,
            format!("pi answered in session {session}, not {}", asked.0),
        ));
    }
    let text = last
        .content
        .iter()
        .filter(|c| c.kind == "text")
        .filter_map(|c| c.text.as_deref())
        .collect();
    Ok(AgentReply {
        session_id: SessionId(session),
        text,
        usage,
        session_cost: None,
    })
}
