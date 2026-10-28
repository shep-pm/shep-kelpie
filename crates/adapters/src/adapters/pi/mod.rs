//! pi, headless as `pi -p`, one process per call, on an OpenAI-compatible model
//!
//! Each call runs whole inside kelpie's sandbox. pi has no sandbox of its own
//! and no permission prompts. Kelpie gives it a home beside the call's
//! settings, holding its one model and its sessions, so the maintainer's pi
//! setup never loads. A fenced call loads an extension that runs kelpie's
//! checks before each command and file write. MCP servers go in an `mcp.json`
//! there. pi dials a forwarder outside the sandbox, which passes on only the
//! chat completions call. A gateway's model gets its key, and each call holds
//! its lease where offered.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::Deserialize;
use serde_json::json;

use self::compact::Tokens;
use self::shared::profile;
use super::cli::{Cli, CliAdapter, Launch, Turn};
use super::harness::{
    Base, assignments, bind_in_scratch, call_policy, program_folder, prompt_file, step_skill, tail,
};
use super::process::record::CallFiles;
use super::unreachable::reaches_no_host;
use crate::dep_sources::ProcessEnv;
use crate::fence;
use crate::forwarder::Upstream;
use crate::guard::{AFTER_FLAG, FOLDER_FLAG, Pause};
use crate::ports::{
    Adopted, AgentCall, AgentError, AgentReply, COMPACT, CallActivity, Fence, Policy, Session,
    SessionId, Tools, Transcript, Usage,
};
use crate::settings::{Harness, HookPoint, ModelServer};
use crate::tool_error::GUARD_MARK;

// What the module's tests take from it by glob.
#[cfg(test)]
use {
    super::forwarder::Forwarder,
    crate::forwarder::WORKER_HOST,
    crate::ports::{Agents, Ending, Forward, Role},
    crate::settings::{AgentHarness, Gateways, ModelHost},
};

mod activity;
mod attach;
mod compact;
pub(crate) mod mcp;
mod shared;

#[cfg(test)]
mod activity_tests;

#[cfg(test)]
mod adopt_tests;

#[cfg(test)]
mod attach_tests;

#[cfg(test)]
mod compact_tests;

#[cfg(test)]
mod dep_sources_tests;

#[cfg(test)]
mod env_tests;

#[cfg(test)]
mod socket_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod escapes;

#[cfg(all(test, unix))]
mod ecosystems;

#[cfg(test)]
mod guard_tests;

#[cfg(test)]
mod gateway_tests;

#[cfg(test)]
mod unreachable_tests;

#[cfg(test)]
mod retro_tests;

#[cfg(test)]
mod review_tests;

#[cfg(test)]
mod skills_tests;

#[cfg(test)]
mod window_tests;

/// The oldest pi kelpie is built for: built-in MCP needs 0.99 or later, and
/// compaction events carry a `reason` from 1.0.4
pub const PI_MINIMUM: &str = "1.0.4";

/// This adapter's harness, as its errors and the fence name it
const PI: Harness = Harness::Pi;

/// The provider kelpie's own `models.json` names, for the call's one model
const PROVIDER: &str = "kelpie";

/// The extension that runs kelpie's checks, with `__CHECKS__` and `__MARK__` to fill in
const GUARD: &str = include_str!("guard.ts");

/// pi's tools for each kind, by its own names
///
/// A reviewer that may write has `write` alone, as Claude's has `Write` alone.
fn tool_names(tools: Tools, reads: bool, writes: bool) -> Option<&'static str> {
    match tools {
        Tools::Work => Some("read,bash,edit,write,grep,find,ls"),
        Tools::Review if writes => Some("read,grep,find,ls,write"),
        Tools::Review | Tools::Retro => Some("read,grep,find,ls"),
        // The project manager runs on Claude Code alone, so pi only ever reads for it.
        Tools::Answer | Tools::Pm if reads => Some("read"),
        Tools::Answer | Tools::Pm => None,
    }
}

// The tools the call has, by pi's names
fn tools_of(call: &AgentCall) -> Option<&'static str> {
    let writes = call
        .reach
        .fence
        .as_ref()
        .is_some_and(|f| !f.write.is_empty());
    tool_names(call.tools, !call.reach.read.is_empty(), writes)
}

// The servers the call starts: none for a call with no tools, which could not use them.
fn mcp_config(call: &AgentCall) -> Option<serde_json::Value> {
    tools_of(call).and_then(|_| mcp::config(call))
}

/// What is true of pi: the part of a headless `pi -p` call that no other CLI shares
#[derive(Debug, Clone)]
pub struct Pi {
    program: OsString,
    // What a call with no fence may not read, besides its own folder
    unfenced_no_read: Vec<String>,
}

/// Headless pi, one `pi -p` process per call, each inside the sandbox
///
/// It shares Claude Code's calls in flight, so stopping either stops both.
pub type PiCli = CliAdapter<Pi>;

impl PiCli {
    /// pi on `base`: its sandbox, lamb labels and calls in flight
    pub fn on(base: &Base) -> Self {
        let pi = Pi {
            program: "pi".into(),
            unfenced_no_read: base.unfenced_no_read.clone(),
        };
        Self::new(pi, base)
    }

    /// Runs `program` in place of `pi`, as a stand-in script does
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_program(mut self, program: PathBuf) -> Self {
        self.cli.program = program.into();
        self
    }

    /// The call's headless command inside its sandbox and the forwarder it dials
    #[cfg(test)]
    pub(crate) fn sandboxed_command(
        &self,
        call: &AgentCall,
        lease: Option<&str>,
    ) -> Result<(Command, super::forwarder::Forwarder), AgentError> {
        let launched = self.launch(call, &(), Turn::Headless, lease)?;
        let forwarder = launched.forwarder.expect("pi runs on a model server");
        Ok((launched.command, forwarder))
    }
}

impl Cli for Pi {
    type Begun = ();
    const HARNESS: Harness = Harness::Pi;

    fn mcp_refusal(name: &str, entry: &serde_json::Value) -> Option<String> {
        mcp::refusal(name, entry)
    }

    fn write_config(&self, call: &AgentCall, served: Option<&Upstream>) -> Result<(), AgentError> {
        let (server, upstream) = (server(call)?, served.ok_or_else(unserved)?);
        let files = Files::of(call);
        let setup = |what: &Path, e: std::io::Error| {
            AgentError::Setup(format!("cannot write {}: {}", what.display(), e.kind()))
        };
        std::fs::create_dir_all(files.sessions()).map_err(|e| setup(&files.sessions(), e))?;
        let models = json!({ "providers": { PROVIDER: {
            "baseUrl": upstream.worker_url(),
            "api": "openai-completions",
            // pi lists no model without one. The forwarder sends a gateway's own key.
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
        // A compact call of any role loads it, the project manager's included.
        if loads_compact(call) {
            let compact = files.compact();
            let again = call.compact_window.map(|_| files.prompt());
            std::fs::write(&compact, compact::extension(again.as_deref()))
                .map_err(|e| setup(&compact, e))?;
        }
        if call.compact_window.is_some() {
            let prompt = files.prompt();
            std::fs::write(&prompt, compact::handed_back(call)).map_err(|e| setup(&prompt, e))?;
        }
        // The home outlives the call, so an earlier call's reserve goes with its window.
        let path = files.settings();
        match call
            .compact_window
            .and_then(|w| compact::settings(server.context.get(), w))
        {
            Some(settings) => {
                let text = serde_json::to_string_pretty(&settings).expect("settings are JSON");
                std::fs::write(&path, text).map_err(|e| setup(&path, e))?;
            }
            None => match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(setup(&path, e)),
                _ => {}
            },
        }
        mcp::prepare(mcp_config(call), &files)
    }

    // A resumed session pi never wrote is a call that cannot run.
    fn begin(&self, call: &AgentCall, _: Turn) -> Result<(), AgentError> {
        refuse_missing_session(call)
    }

    // `pi` in the call's folder, its variables set by `env` inside the sandbox,
    // after the sandbox has set its own.
    fn command(
        &self,
        call: &AgentCall,
        _: &(),
        launch: &Launch<'_>,
    ) -> Result<Command, AgentError> {
        let files = Files::of(call);
        let argv = match launch.turn {
            Turn::Headless => argv(call, &files),
            Turn::Terminal => attach::terminal_argv(call, &files),
        };
        let mut command = crate::spawn::command("env");
        command
            .args(env_args(call, &files))
            .arg(&self.program)
            .args(argv)
            .current_dir(&call.cwd);
        Ok(command)
    }

    fn policy(&self, call: &AgentCall, _: &Launch<'_>) -> Result<Policy, AgentError> {
        Ok(fenced(
            call,
            &Files::of(call),
            Path::new(&self.program),
            &self.unfenced_no_read,
        ))
    }

    fn parse(&self, call: &AgentCall, _: (), output: &Output) -> Result<AgentReply, AgentError> {
        read(call, output)
    }

    fn parse_adopted(
        &self,
        adopted: &Adopted,
        _: (),
        output: &Output,
    ) -> Result<AgentReply, AgentError> {
        let kept = adopted.kept.as_ref().ok_or_else(unserved)?;
        let compacting = adopted.flying.as_ref().is_some_and(|f| f.compact);
        read_as(compacting, output, &kept.session)
    }

    fn transcript(&self, call: &AgentCall) -> Option<Transcript> {
        Some(Transcript {
            folder: Files::of(call).sessions(),
            threads: None,
        })
    }

    fn last_active(&self, call: &AgentCall) -> CallActivity {
        activity::of_stdout(&CallFiles::of(call).stdout)
    }
}

fn unserved() -> AgentError {
    AgentError::Setup("kelpie routed a call to pi that names no model server".into())
}

// A resumed session pi never wrote is a call that cannot run.
fn refuse_missing_session(call: &AgentCall) -> Result<(), AgentError> {
    match &call.session {
        Session::Resume(id) if session_file(&Files::of(call).sessions(), id).is_none() => {
            Err(AgentError::NoSession(PI, id.clone()))
        }
        _ => Ok(()),
    }
}

/// The server the call's model is on, which every pi call needs
fn server(call: &AgentCall) -> Result<&ModelServer, AgentError> {
    call.harness.server().ok_or_else(unserved)
}

// A compact call, and a call bounded by a window that hands its prompt back
// after pi compacts it, load the compact extension.
fn loads_compact(call: &AgentCall) -> bool {
    call.prompt == COMPACT || call.compact_window.is_some()
}

/// The files one call keeps beside its settings path
struct Files {
    /// pi's own home for the call: the model, and the sessions folder
    home: PathBuf,
    /// The call's temporary files, emptied before each call
    scratch: PathBuf,
    /// The guard extension, outside anything the call may write
    guard_path: PathBuf,
    /// The extension a compact call loads, outside anything the call may write
    compact_path: PathBuf,
    /// The turn's prompt, which the compact extension hands back after a compaction
    prompt_path: PathBuf,
}

impl Files {
    fn of(call: &AgentCall) -> Self {
        Self {
            home: call.settings.with_extension("pi"),
            scratch: call.settings.with_extension("tmp"),
            guard_path: call.settings.with_extension("guard.ts"),
            compact_path: call.settings.with_extension("compact.ts"),
            prompt_path: prompt_file(&call.settings),
        }
    }

    fn sessions(&self) -> PathBuf {
        self.home.join("sessions")
    }

    fn guard(&self) -> PathBuf {
        self.guard_path.clone()
    }

    fn compact(&self) -> PathBuf {
        self.compact_path.clone()
    }

    fn prompt(&self) -> PathBuf {
        self.prompt_path.clone()
    }

    // pi's own settings, which it reads from its home.
    fn settings(&self) -> PathBuf {
        self.home.join("settings.json")
    }

    // pi takes a lock folder beside each store it reads at start, even when
    // it changes nothing there.
    fn locks(&self) -> [PathBuf; 3] {
        [
            "auth.json.lock",
            "models-store.json.lock",
            "settings.json.lock",
        ]
        .map(|lock| self.home.join(lock))
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
        // pi's `grep` and `find` download `rg` and `fd` when neither is in its
        // `bin` or on `PATH`, and the sandbox refuses that. `--offline` says it
        // to pi's argument parsing; this reaches every program pi starts, and
        // is what pi's tools read.
        ("PI_OFFLINE".into(), "1".into()),
        ("PI_CODING_AGENT_DIR".into(), files.home.clone().into()),
        ("TMPDIR".into(), files.scratch.clone().into()),
        // zsh ignores `TMPDIR` for a heredoc and writes under this prefix.
        ("TMPPREFIX".into(), files.scratch.join("zsh").into()),
    ];
    if let Some(fence) = &call.reach.fence {
        vars.extend(
            fence
                .env
                .iter()
                .map(|(name, value)| (name.clone(), value.clone().into())),
        );
    }
    assignments(vars).collect()
}

// pi loads nothing of the maintainer's or the repo's own: no extensions,
// skills, prompt templates or themes it finds, and no project-local files.
// The skills it loads are the ones named with `--skill`, which need no project trust.
// It does read the repo's AGENTS.md and CLAUDE.md, as Claude Code does.
fn argv(call: &AgentCall, files: &Files) -> Vec<OsString> {
    let compacting = call.prompt == COMPACT;
    let (prompt, skill) = match compacting {
        true => (compact::COMMAND.to_owned(), None),
        false => skill_prompt(call),
    };
    let mut argv = profile(call, files, skill);
    // The terminal session has no turn for the extension to hand back after a compaction.
    if loads_compact(call) {
        argv.extend(["--extension".into(), files.compact().into()]);
    }
    argv.extend(["-p".into(), "--mode".into(), "json".into()]);
    argv.extend(["--session-id".into(), call.session.id().0.as_str().into()]);
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
        Some(step) => (
            format!("/skill:{} {}", step.name, step.rest),
            Some(step.dir),
        ),
        None => (call.prompt.clone(), None),
    }
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
    commands.extend(guard.hooks.flag());
    commands.extend(guard.base_flag());
    commands.extend(guard.kelpie_pushes_flag().map(str::to_owned));
    // The project's `write` hooks ride on `confine`, and its `after_command` ones on
    // the guard again, run on each command's result.
    let mut confine = vec![
        "confine".to_owned(),
        text(&guard.worktree),
        text(&guard.build),
    ];
    if let Some(flag) = guard
        .hooks
        .flag()
        .filter(|_| guard.hooks.has(HookPoint::Write))
    {
        confine.push(flag);
    }
    let after = guard.hooks.has(HookPoint::AfterCommand).then(|| {
        let mut after = commands.clone();
        after.push(AFTER_FLAG.to_owned());
        after
    });
    let pause = Pause::in_build(&guard.build);
    let checks = json!({
        "kelpie": text(&guard.kelpie),
        "confine": confine,
        "guard": commands,
        "after": after,
        "pause": { "requested": pause.requested_file(), "ended": pause.ended_file() },
    });
    GUARD
        .replace("__CHECKS__", &checks.to_string())
        .replace("__MARK__", &json!(GUARD_MARK).to_string())
}

/// The whole call's policy: its fence, or none, and what pi itself needs
///
/// The model's host is not in it. The one host pi may dial is the forwarder's,
/// which the adapter adds.
fn fenced(call: &AgentCall, files: &Files, program: &Path, unfenced: &[String]) -> Policy {
    let mut policy = call_policy(call, unfenced);
    policy.no_keychain = call.reach.block_keychain;
    // pi logs in nowhere, so no harness's login is its own, the maintainer's pi included.
    let logins = fence::HARNESSES.iter().flat_map(|own| own.credentials);
    policy.no_read.extend(logins.map(|&path| path.to_owned()));
    policy.write.extend(
        [files.sessions(), files.scratch.clone()]
            .into_iter()
            .chain(files.locks()),
    );
    // Node aborts at start when it cannot stat its own stdout and stderr,
    // the files kelpie opens for them.
    let outputs = CallFiles::of(call);
    // A fence denies the home folder whole, so pi's own files are named beside
    // what the call is granted: its home, its scratch, and the
    // program it runs, wherever that sits.
    policy.read.extend(
        [
            files.home.clone(),
            files.guard(),
            files.compact(),
            files.scratch.clone(),
        ]
        .into_iter()
        .chain(program_folder(program, &ProcessEnv, &files.home))
        .chain(call.compact_window.map(|_| files.prompt()))
        .chain([outputs.stdout, outputs.stderr])
        .chain(call.instructions.iter().cloned())
        .chain(call.plugin_dirs.iter().cloned())
        .chain(call.reach.read.iter().cloned()),
    );
    bind_in_scratch(&mut policy, call, &files.scratch);
    super::mcp::fence_the_graph(&mut policy, call);
    super::mcp::fence_the_browser(&mut policy, call);
    policy
}

// The policy as a running call has it, the forwarder at `forwarder` its one host.
#[cfg(test)]
fn policy(
    call: &AgentCall,
    files: &Files,
    upstream: &Upstream,
    forwarder: &Path,
    program: &Path,
    unfenced: &[String],
) -> Policy {
    let mut policy = fenced(call, files, program, unfenced);
    super::harness::dial_the_forwarder(&mut policy, upstream, forwarder);
    policy
}

// A compact call's output is read for its compaction, any other call's for its turn.
fn read(call: &AgentCall, output: &Output) -> Result<AgentReply, AgentError> {
    if call.prompt == COMPACT {
        return read_as(true, output, call.session.id());
    }
    parse_turn(output, call.session.id(), ended_for_pause(call))
}

// Whether the guard extension aborted the call's turn for a pause: it leaves
// its marker before it aborts.
fn ended_for_pause(call: &AgentCall) -> bool {
    (call.reach.fence.as_deref())
        .is_some_and(|fence| crate::guard::Pause::in_build(&fence.guard.build).ended_turn())
}

// The same for a call another image carried, which knows only whether it compacts.
fn read_as(
    compacting: bool,
    output: &Output,
    session: &SessionId,
) -> Result<AgentReply, AgentError> {
    match compacting {
        true => compact::parse(output, session),
        false => parse_result(output, session),
    }
}

// An error for a call that failed with `detail`, unreachable where it says the
// model host could not be reached: a 502 or 503 as the SDK leads its message
// with the status, or a connection refused or reset.
fn failed(detail: &str) -> AgentError {
    if reaches_no_host(detail, &["502", "503"], &[]) {
        AgentError::Unreachable(PI, detail.to_owned())
    } else {
        AgentError::Failed(PI, detail.to_owned())
    }
}

// `pi -p --mode json` prints one event per line: a session header, then
// every message as it ends. The call's usage is the sum of its assistant
// messages and its own compactions, its context the last request's prompt,
// and its answer the last message's text. A local model has no price.
fn parse_result(output: &Output, asked: &SessionId) -> Result<AgentReply, AgentError> {
    parse_turn(output, asked, false)
}

// The same for a turn that may have ended because the guard extension aborted it for a pause.
fn parse_turn(output: &Output, asked: &SessionId, paused: bool) -> Result<AgentReply, AgentError> {
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
        usage: Option<Tokens>,
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut session = None;
    let mut usage = Usage::default();
    let mut context = None;
    let mut compactions = 0;
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
                    usage += u.usage();
                }
                // The latest message alone: an earlier one's prompt is stale.
                context = message.usage.as_ref().map(Tokens::context);
                last = Some(message);
            }
            // pi's own threshold compaction, which runs inside a turn.
            ("compaction_end", _) => {
                usage += compact::usage_of(text);
                compactions += u32::from(compact::made_summary(text));
            }
            _ => {}
        }
    }
    // A turn the guard extension aborted for a pause stops with the reason
    // `aborted`, and is read for its usage like one that ended on its own.
    let paused = paused
        && last
            .as_ref()
            .is_some_and(|m| m.stop_reason.as_deref() == Some("aborted"));
    if !paused && !output.status.success() {
        let why = last.as_ref().and_then(|m| m.error_message.clone());
        return Err(failed(why.as_deref().unwrap_or(tail(&stderr))));
    }
    let (Some(session), Some(last), false) = (session, last, unread) else {
        return Err(AgentError::Unreadable(PI, tail(&stdout).to_owned()));
    };
    match last.stop_reason.as_deref() {
        Some("stop") => {}
        Some("length") => return Err(failed("the model ran out of output tokens")),
        Some("aborted") if paused => {}
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
        context,
        compactions,
    })
}
