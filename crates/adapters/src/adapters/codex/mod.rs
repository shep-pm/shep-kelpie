//! Codex, headless as `codex exec --json`, one process per call, on the
//! ChatGPT account kelpie's own Codex login is signed in to
//!
//! Each call runs whole inside kelpie's sandbox with Codex's own sandbox and
//! approvals off: on macOS a sandbox cannot start inside another. Kelpie's
//! login lives in `codex_home`, apart from `~/.codex`. Each call gets a Codex
//! home beside its settings, with the login's `auth.json` linked in to read
//! only. A fenced call runs `kelpie confine` on each `apply_patch` and `kelpie
//! guard` on each shell command, as `PreToolUse` hooks. Codex names its own
//! sessions, so kelpie keeps which one each session id is, beside the settings.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::{Deserialize, Serialize};

use super::cli::{Cli, CliAdapter, Launch, Turn};
use super::harness::{
    Base, Records, assignments, bind_in_scratch, call_policy, program_folder, step_skill, tail,
};
use super::process::record::CallFiles;
use super::unreachable::reaches_no_host;
use crate::dep_sources::ProcessEnv;
use crate::fence;
use crate::forwarder::Upstream;
use crate::guard::{AFTER_FLAG, FOLDER_FLAG, PIN_FLAG};
use crate::ports::{
    Adopted, AgentCall, AgentError, AgentReply, COMPACT, CallActivity, Fence, Policy, Session,
    SessionId, Tools, Transcript, Usage, written_at,
};
use crate::settings::{Harness, HookPoint};
use crate::spawn::shell_quote;

// What the module's tests take from it by glob.
#[cfg(test)]
use {
    crate::ports::{Agents, Cost, Ending},
    crate::settings::AgentHarness,
};

mod adopt;
mod attach;
mod context;
pub(crate) mod mcp;
mod price;
mod reading;
mod skills;

#[cfg(test)]
mod adopt_tests;

#[cfg(test)]
mod attach_tests;

#[cfg(test)]
mod context_tests;

#[cfg(test)]
mod env_tests;

#[cfg(test)]
mod hook_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ports_tests;

#[cfg(test)]
mod price_tests;

#[cfg(test)]
mod reading_tests;

#[cfg(test)]
mod review_tests;

#[cfg(test)]
mod socket_tests;

#[cfg(test)]
mod unreachable_tests;

/// This adapter's harness, as its errors name it
const CODEX: Harness = Harness::Codex;

/// This harness, as the fence names it
const HARNESS: &str = "Codex";

/// The ChatGPT backend a Codex login on a ChatGPT plan calls, the one host
/// every call reaches
///
/// Not `auth.openai.com`, where a login is refreshed: the sandbox lets the
/// login be read, not written, and a refreshed login that could not be
/// saved would be lost. The Codex usage read runs outside the sandbox
/// before each dispatch, on kelpie's own login, and refreshes it there.
const MODEL_HOST: &str = "chatgpt.com";

/// macOS's bundle of trusted certificates, which a call that blocks the
/// keychain verifies its model's certificate against
///
/// Codex is built on `native-tls`, which asks the keychain services the
/// sandbox then blocks, so without it every request fails as "workspace
/// routing discovery failed" (`docs/facts/codex-as-an-agent.md`).
const SYSTEM_CERTIFICATES: &str = "/etc/ssl/cert.pem";

/// The maintainer's own Codex home, which no call reads
const MAINTAINERS_HOME: &str = "~/.codex/**";

/// Other people's skills Codex finds outside its home, which no call reads
const SKILLS: &str = "~/.agents/**";

/// The login's file in a Codex home
const AUTH: &str = "auth.json";

/// Where Codex keeps its databases in a call's home, by `sqlite_home`
const DATABASES: &str = "db";

/// Where Codex keeps its logs in a call's home, by `log_dir`
const LOGS: &str = "log";

/// The only folders in a call's Codex home the call may write
///
/// Its sessions, databases, logs and locks. Everything else there is Codex's
/// to load, not to write: its login, config, `hooks.json`, `.env`, rules,
/// skills and `AGENTS.md`. Not `tmp`: Codex puts a folder in it first on every
/// hook's PATH, so a planted `git` would run inside `kelpie guard`
/// (`docs/facts/codex-as-an-agent.md`).
const WRITTEN: [&str; 5] = [
    "sessions",
    "archived_sessions",
    DATABASES,
    LOGS,
    "thread-writer-locks",
];

/// The one file in a call's Codex home the call may write: an id Codex
/// opens to read and write at start, and will not start without
const INSTALLATION_ID: &str = "installation_id";

/// Codex's features no call runs, each one a way past the call's tools,
/// its hooks or its sandbox, or a turn that never ends
///
/// `unified_exec` is not among them: Codex turns it back on unless an
/// administrator's `requirements.toml` pins it off
/// (`docs/facts/codex-as-an-agent.md`).
const FEATURES_OFF: [&str; 17] = [
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
    // Retrying a refused connection forever, so a turn whose model host is
    // down would wait without end and never be read as unreachable
    "unbounded_connection_retries",
];

/// Codex's own part of a call
#[derive(Debug, Clone)]
pub struct Codex {
    program: OsString,
    login: PathBuf,
    // What a call with no fence may not read, besides its own folder
    unfenced_no_read: Vec<String>,
}

/// Headless Codex, one `codex exec` process per call, each inside the sandbox
///
/// It shares Claude Code's calls in flight, so stopping either stops both.
pub type CodexCli = CliAdapter<Codex>;

impl CodexCli {
    /// Codex on `base` and the login in `codex_home`: same sandbox and lamb
    /// labels, and stopped with it
    pub fn on(base: &Base, codex_home: PathBuf) -> Self {
        let codex = Codex {
            program: "codex".into(),
            login: codex_home,
            unfenced_no_read: base.unfenced_no_read.clone(),
        };
        Self::new(codex, base)
    }

    /// Runs `program` in place of `codex`, as a stand-in script does
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_program(mut self, program: PathBuf) -> Self {
        self.cli.program = program.into();
        self
    }

    /// The call's headless command inside its sandbox
    #[cfg(test)]
    pub(crate) fn sandboxed_command(&self, call: &AgentCall) -> Result<Command, AgentError> {
        let begun = self.cli.begin(call, Turn::Headless)?;
        Ok(self.launch(call, &begun, Turn::Headless, None)?.command)
    }
}

impl Cli for Codex {
    type Begun = Option<Thread>;
    const HARNESS: Harness = CODEX;

    fn mcp_refusal(name: &str, entry: &serde_json::Value) -> Option<String> {
        mcp::refusal(name, entry)
    }

    fn write_config(&self, call: &AgentCall, _: Option<&Upstream>) -> Result<(), AgentError> {
        let files = Files::of(call);
        let written = WRITTEN.map(|name| files.home.join(name));
        for folder in [&files.threads, &files.home].into_iter().chain(&written) {
            std::fs::create_dir_all(folder).map_err(|e| {
                AgentError::Setup(format!("cannot make {}: {}", folder.display(), e.kind()))
            })?;
        }
        link_login(&self.login, &files.home)?;
        skills::link_skills(call, &files.home)
    }

    fn answer(&self, call: &AgentCall) -> Option<Result<AgentReply, AgentError>> {
        (call.prompt == COMPACT).then(|| context::compact(call))
    }

    // A new session is one kelpie has not recorded, and a resumed one, or the
    // maintainer's terminal, is one it has.
    fn begin(&self, call: &AgentCall, turn: Turn) -> Result<Option<Thread>, AgentError> {
        let id = call.session.id();
        let known = thread(&Files::of(call), id)?;
        match (turn, &call.session, known) {
            (Turn::Headless, Session::New(_), None) => Ok(None),
            (Turn::Headless, Session::New(_), Some(_)) => Err(AgentError::Setup(format!(
                "kelpie already started a Codex session as {}",
                id.0
            ))),
            (_, _, Some(thread)) => Ok(Some(thread)),
            (_, _, None) => Err(AgentError::NoSession(CODEX, id.clone())),
        }
    }

    // `argv` run as the call: Codex's home and scratch folder, and the fence's
    // environment.
    fn command(
        &self,
        call: &AgentCall,
        begun: &Option<Thread>,
        launch: &Launch<'_>,
    ) -> Result<Command, AgentError> {
        let files = Files::of(call);
        let instructions = instructions(call)?;
        let argv = match (launch.turn, begun) {
            (Turn::Headless, resumed) => {
                argv(call, &files.home, resumed.as_ref(), instructions.as_deref())?
            }
            (Turn::Terminal, Some(resumed)) => {
                attach::terminal_argv(call, &files.home, resumed, instructions.as_deref())?
            }
            (Turn::Terminal, None) => {
                return Err(AgentError::NoSession(CODEX, call.session.id().clone()));
            }
        };
        let fence = (call.reach.fence.iter()).flat_map(|fence| &fence.env);
        let fence = fence.map(|(name, value)| (name.clone(), value.into()));
        let mut command = crate::spawn::command("env");
        command
            .args(assignments(fence))
            .arg(&self.program)
            .args(argv)
            .current_dir(&call.cwd)
            .env("CODEX_HOME", &files.home)
            .env("TMPDIR", &files.scratch)
            // zsh ignores `TMPDIR` for a heredoc and writes under this prefix.
            .env("TMPPREFIX", files.scratch.join("zsh"));
        // Codex checks certificates through the keychain on macOS.
        if cfg!(target_os = "macos") && call.reach.block_keychain {
            command.env("CODEX_CA_CERTIFICATE", SYSTEM_CERTIFICATES);
        }
        Ok(command)
    }

    fn policy(&self, call: &AgentCall, _: &Launch<'_>) -> Result<Policy, AgentError> {
        let files = Files::of(call);
        Ok(policy(
            call,
            &self.login,
            &files,
            Path::new(&self.program),
            &self.unfenced_no_read,
        ))
    }

    fn parse(
        &self,
        call: &AgentCall,
        _: Option<Thread>,
        output: &Output,
    ) -> Result<AgentReply, AgentError> {
        finish(
            &Files::of(call),
            call.session.id(),
            Some(&call.model),
            output,
        )
    }

    fn adopted_begin(&self, adopted: &Adopted) -> Result<Option<Thread>, String> {
        adopted
            .session
            .as_ref()
            .map(|_| None)
            .ok_or_else(|| NO_SESSION.to_owned())
    }

    fn parse_adopted(
        &self,
        adopted: &Adopted,
        _: Option<Thread>,
        output: &Output,
    ) -> Result<AgentReply, AgentError> {
        let id = (adopted.session.as_ref()).ok_or_else(|| AgentError::Setup(NO_SESSION.into()))?;
        // A carried call's model is not in its record, so it goes unpriced.
        finish(
            &Files::at(&adopt::settings_of(&adopted.record)),
            id,
            None,
            output,
        )
    }

    // Last, so a setup that fails leaves no trust behind for an end that is never told.
    fn started(&self, call: &AgentCall, _: &Self::Begun) -> Result<(), AgentError> {
        attach::trust_folder(&Files::of(call).home, &call.cwd)
    }

    fn ended(&self, settings: &Path, session: &SessionId) {
        attach::ended(settings, session);
    }

    fn transcript(&self, call: &AgentCall) -> Option<Transcript> {
        let files = Files::of(call);
        Some(Transcript {
            folder: files.home.join("sessions"),
            threads: Some(files.threads),
        })
    }

    // `codex exec --json` writes a line to its output file at each event.
    fn last_active(&self, call: &AgentCall) -> CallActivity {
        written_at(&Files::of(call).stdout)
    }
}

/// Why a carried call cannot be finished without the session kelpie gave it
const NO_SESSION: &str = "the carried Codex call names no session";

// Reads what the ended call left and writes its thread record, so its tokens
// are counted from where the session stood before it.
fn finish(
    files: &Files,
    id: &SessionId,
    model: Option<&str>,
    output: &Output,
) -> Result<AgentReply, AgentError> {
    // The model has run by now, so what fails from here is the call's,
    // not its setup's.
    let ran = |e: AgentError| match e {
        AgentError::Setup(why) => AgentError::Failed(CODEX, why),
        e => e,
    };
    let known = thread(files, id).map_err(ran)?;
    let turn = parse_result(output, known.as_ref())?;
    let before = known.map(|k| k.spent).unwrap_or_default();
    let context = context::last_request_input(&files.home, &turn.thread);
    // The turn's report leaves out a compaction's request, which the rollout counts.
    let spent = context::thread_total(&files.home, &turn.thread, turn.spent).unwrap_or(turn.spent);
    let record = Thread {
        id: turn.thread,
        spent,
        context,
        compact: false,
    };
    save_thread(files, id, &record).map_err(ran)?;
    Ok(AgentReply {
        session_id: id.clone(),
        text: turn.text,
        usage: spent.since(before),
        session_cost: model.and_then(|m| price::price(m, &spent.since(Spent::default()))),
        context,
        compactions: 0,
    })
}

// The developer instructions the call carries, read from their file.
fn instructions(call: &AgentCall) -> Result<Option<String>, AgentError> {
    call.instructions
        .as_ref()
        .map(|path| {
            std::fs::read_to_string(path).map_err(|e| {
                AgentError::Setup(format!("cannot read {}: {}", path.display(), e.kind()))
            })
        })
        .transpose()
}

/// The files one call keeps beside its settings path
struct Files {
    /// The call's own Codex home: its sessions, Codex's database and logs,
    /// and the login linked in
    home: PathBuf,
    /// Which Codex session each of kelpie's session ids is, out of the
    /// call's reach
    threads: PathBuf,
    /// The call's temporary files, emptied before each call
    scratch: PathBuf,
    /// Codex's stdout, which kelpie opens for it outside the call's reach
    stdout: PathBuf,
}

impl Files {
    fn of(call: &AgentCall) -> Self {
        Self::at(&call.settings)
    }

    fn at(settings: &Path) -> Self {
        Self {
            home: settings.with_extension("codex"),
            threads: settings.with_extension("threads"),
            scratch: settings.with_extension("tmp"),
            stdout: CallFiles::at(settings).stdout,
        }
    }

    fn threads(&self) -> Records<Thread> {
        Records::in_folder(self.threads.clone())
    }

    #[cfg(test)]
    fn thread(&self, id: &SessionId) -> PathBuf {
        self.threads().file(id).expect("a session id kelpie makes")
    }
}

// Links the login into the call's home, so Codex signs in without kelpie
// reading it. A link that points elsewhere, or a file left in its place,
// is made again.
fn link_login(login: &Path, home: &Path) -> Result<(), AgentError> {
    let target = login.join(AUTH);
    let link = home.join(AUTH);
    let setup = |e: std::io::Error| {
        AgentError::Setup(format!("cannot link {}: {}", link.display(), e.kind()))
    };
    match std::fs::read_link(&link) {
        Ok(to) if to == target => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        _ => std::fs::remove_file(&link).map_err(setup)?,
    }
    crate::adapters::files::link(&target, &link).map_err(setup)
}

/// The Codex session one of kelpie's session ids is
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thread {
    /// Codex's own id for it
    id: String,
    /// The session's tokens as Codex last reported them, which a resumed
    /// turn's report runs on from
    #[serde(default)]
    spent: Spent,
    /// The input tokens of the session's last model request, where the
    /// rollout told them
    #[serde(default)]
    context: Option<u64>,
    /// Whether a `COMPACT` call came after that turn, so the next one
    /// compacts before it runs
    #[serde(default)]
    compact: bool,
}

/// A Codex session's tokens so far, as its last turn reported them
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Spent {
    /// Every input token, cached or not
    input_tokens: u64,
    /// The input tokens read from the cache
    cached_input_tokens: u64,
    /// The input tokens written to the cache
    cache_write_input_tokens: u64,
    /// Every output token, reasoning included
    output_tokens: u64,
}

impl Spent {
    /// What was spent after `before`, as kelpie counts a turn's tokens
    ///
    /// Codex counts cached input, read or written, within its input.
    fn since(self, before: Self) -> Usage {
        let less = |now: u64, then: u64| now.saturating_sub(then);
        let cache_read = less(self.cached_input_tokens, before.cached_input_tokens);
        let cache_write = less(
            self.cache_write_input_tokens,
            before.cache_write_input_tokens,
        );
        Usage {
            input: less(self.input_tokens, before.input_tokens)
                .saturating_sub(cache_read)
                .saturating_sub(cache_write),
            cache_write,
            cache_write_5m: 0,
            cache_read,
            output: less(self.output_tokens, before.output_tokens),
        }
    }
}

fn save_thread(files: &Files, id: &SessionId, record: &Thread) -> Result<(), AgentError> {
    files.threads().put(id, record)
}

/// The Codex session kelpie started as `id`, if any
fn thread(files: &Files, id: &SessionId) -> Result<Option<Thread>, AgentError> {
    files.threads().get(id)
}

/// `key=value` for `-c`, with the value written as TOML
fn config(key: &str, value: impl Into<toml::Value>) -> OsString {
    format!("{key}={}", value.into()).into()
}

// Codex ignores the user config and rules, and keeps its databases and
// logs in folders of `home` the call may write. It reads the repo's AGENTS.md,
// and loads only the MCP servers it is given.
fn argv(
    call: &AgentCall,
    home: &Path,
    resumed: Option<&Thread>,
    instructions: Option<&str>,
) -> Result<Vec<OsString>, AgentError> {
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
    argv.extend(overrides(call, home, resumed, instructions));
    argv.push("--".into());
    if let Some(thread) = resumed {
        argv.push(thread.id.as_str().into());
    }
    argv.push(reading::told(call, prompt(call)?).into());
    Ok(argv)
}

// What a call changes in Codex, as `-c` overrides and features turned off, for
// the headless call and the terminal session alike, so the terminal holds the
// worker to what its calls are held to.
fn overrides(
    call: &AgentCall,
    home: &Path,
    resumed: Option<&Thread>,
    instructions: Option<&str>,
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = Vec::new();
    argv.extend(
        [
            // Codex's levels share Claude Code's names; a model refuses one it lacks.
            config("model_reasoning_effort", call.effort.as_str()),
            config("web_search", "disabled"),
            config("check_for_update_on_startup", false),
            config(
                "sqlite_home",
                home.join(DATABASES).to_string_lossy().as_ref(),
            ),
            config("log_dir", home.join(LOGS).to_string_lossy().as_ref()),
        ]
        .into_iter()
        .flat_map(|c| [OsString::from("-c"), c]),
    );
    if let Some(text) = instructions {
        argv.extend(["-c".into(), config("developer_instructions", text)]);
    }
    argv.extend(context::limit_args(call.compact_window, resumed));
    argv.extend(mcp::overrides(call));
    let mut off: Vec<&str> = FEATURES_OFF.into();
    // A call with no fence has no shell, and a worker's runs under the worker's guard.
    if !(has_shell(call) || reading::has_shell(call)) {
        off.push("shell_tool");
    }
    for feature in off {
        argv.extend(["--disable".into(), feature.into()]);
    }
    let hooks = match &call.reach.fence {
        Some(fence) => hooks(fence, call.tools),
        None => no_writes(),
    };
    // Codex runs a hook only once it is trusted; kelpie wrote these itself.
    argv.push("--dangerously-bypass-hook-trust".into());
    argv.extend(["-c".into(), config("hooks.PreToolUse", hooks)]);
    let after = match &call.reach.fence {
        Some(fence) if call.tools == Tools::Work => after_hooks(fence),
        _ => None,
    };
    if let Some(after) = after {
        argv.extend(["-c".into(), config("hooks.PostToolUse", after)]);
    }
    argv
}

/// Whether the call is a worker's, whose shell opens a step's `SKILL.md`
///
/// Codex gets a call's skill links from this alone, so `agent_tools`, which
/// Claude Code reads, does not change them. A reviewer's and retro's shell
/// only reads (`reading`).
fn has_shell(call: &AgentCall) -> bool {
    call.tools == Tools::Work
}

/// The prompt as Codex takes it
///
/// Kelpie starts a step's prompt with Claude Code's `/<plugin>:<skill>`.
/// A worker is told to follow that skill's file, which its shell reads. A
/// call with no shell has no tool that reads a file, so kelpie reads the
/// skill's `SKILL.md` itself and puts it in the prompt.
fn prompt(call: &AgentCall) -> Result<String, AgentError> {
    let Some(step) = step_skill(call) else {
        return Ok(call.prompt.clone());
    };
    let file = step.dir.join("SKILL.md");
    if call.tools == Tools::Work {
        return Ok(format!(
            "Follow the skill `{}` in {}: read it first, then do what it says for this.\n\n{}",
            step.name,
            file.display(),
            step.rest
        ));
    }
    let skill = std::fs::read_to_string(&file)
        .map_err(|e| AgentError::Setup(format!("cannot read {}: {}", file.display(), e.kind())))?;
    Ok(format!(
        "Follow the skill `{}`, whose SKILL.md is below, and do what it says for this.\n\n\
         <skill name=\"{}\">\n{}\n</skill>\n\n{}",
        step.name,
        step.name,
        skill.trim_end(),
        step.rest
    ))
}

/// The `PreToolUse` hooks that run kelpie's checks, as Codex's config takes them
///
/// `apply_patch` is how Codex writes files, and `Bash` is every command. A
/// reviewer's and retro's commands go to the read-only guard.
fn hooks(fence: &Fence, tools: Tools) -> toml::Value {
    let guard = &fence.guard;
    let quoted = |p: &Path| shell_quote(&p.to_string_lossy());
    let mut confine = vec![
        quoted(&guard.kelpie),
        shell_quote("confine"),
        quoted(&guard.worktree),
        quoted(&guard.build),
    ];
    // The project's `write` hooks, which `confine` runs on each file a patch writes.
    if let Some(flag) = guard
        .hooks
        .flag()
        .filter(|_| guard.hooks.has(HookPoint::Write))
    {
        confine.push(shell_quote(&flag));
    }
    let confine = confine.join(" ");
    let commands: Vec<String> = [
        quoted(&guard.kelpie),
        shell_quote("guard"),
        quoted(&guard.git_common_dir),
        quoted(&guard.worktree),
        shell_quote(PIN_FLAG),
    ]
    .into_iter()
    .chain(
        guard
            .folders
            .iter()
            .map(|p| shell_quote(&format!("{FOLDER_FLAG}{}", p.display()))),
    )
    .chain(guard.hooks.flag().map(|flag| shell_quote(&flag)))
    .chain(guard.base_flag().map(|flag| shell_quote(&flag)))
    .chain(guard.kelpie_pushes_flag().map(shell_quote))
    .collect();
    let patch = match reading::patches_nothing(tools, fence) {
        true => refuse_patch(),
        false => hook_entry("^apply_patch$", confine),
    };
    let commands = match reading::judges_reads(tools) {
        true => reading::guard_command(fence),
        false => commands.join(" "),
    };
    toml::Value::Array(vec![patch, hook_entry("^Bash$", commands)])
}

/// The `PostToolUse` hook that runs the project's `after_command` hooks on a
/// command's result, if the project has any
///
/// It is the guard again, without the pin that keeps a command to the folder
/// the guard judged it in, since the command has already run.
fn after_hooks(fence: &Fence) -> Option<toml::Value> {
    let guard = &fence.guard;
    let flag = guard
        .hooks
        .flag()
        .filter(|_| guard.hooks.has(HookPoint::AfterCommand))?;
    let quoted = |p: &Path| shell_quote(&p.to_string_lossy());
    let command = [
        quoted(&guard.kelpie),
        shell_quote("guard"),
        quoted(&guard.git_common_dir),
        quoted(&guard.worktree),
        shell_quote(&flag),
        shell_quote(AFTER_FLAG),
    ]
    .join(" ");
    Some(toml::Value::Array(vec![hook_entry("^Bash$", command)]))
}

/// The `PreToolUse` hook of a call with no fence, which writes no file
///
/// Its model's `apply_patch` cannot be turned off from a call, so the hook
/// refuses every one.
fn no_writes() -> toml::Value {
    toml::Value::Array(vec![refuse_patch()])
}

// The `apply_patch` entry that refuses every patch.
fn refuse_patch() -> toml::Value {
    let refuse = "echo 'kelpie: this call writes no files' >&2; exit 2";
    hook_entry("^apply_patch$", refuse.into())
}

/// One `PreToolUse` entry: a command hook for the tools `matcher` names
fn hook_entry(matcher: &str, command: String) -> toml::Value {
    let mut hook = toml::Table::new();
    hook.insert("type".into(), "command".into());
    hook.insert("command".into(), command.into());
    let mut entry = toml::Table::new();
    entry.insert("matcher".into(), matcher.into());
    entry.insert("hooks".into(), toml::Value::Array(vec![hook.into()]));
    toml::Value::Table(entry)
}

/// The whole call's policy: its fence, or none, and what Codex itself needs
fn policy(
    call: &AgentCall,
    login: &Path,
    files: &Files,
    program: &Path,
    unfenced: &[String],
) -> Policy {
    let mut policy = call_policy(call, unfenced);
    if reading::writes_nothing(call.tools) {
        policy.write.clear();
    }
    policy.no_read.extend(fence::others_credentials(HARNESS));
    policy.no_keychain = call.reach.block_keychain;
    policy
        .no_read
        .extend([MAINTAINERS_HOME, SKILLS].map(str::to_owned));
    policy.write.extend(
        WRITTEN
            .map(|name| files.home.join(name))
            .into_iter()
            .chain([files.home.join(INSTALLATION_ID), files.scratch.clone()]),
    );
    // Codex reads the login through its link to run; kelpie never does.
    policy.read.extend(
        [login.join(AUTH), files.home.clone(), files.scratch.clone()]
            .into_iter()
            .chain(program_folder(program, &ProcessEnv, login))
            .chain(call.instructions.iter().cloned())
            .chain(call.plugin_dirs.iter().cloned())
            .chain(call.reach.read.iter().cloned()),
    );
    bind_in_scratch(&mut policy, call, &files.scratch);
    super::mcp::fence_the_graph(&mut policy, call);
    super::mcp::fence_the_browser(&mut policy, call);
    policy.hosts.push(MODEL_HOST.to_owned());
    // Codex verifies the model's certificate through macOS, as `gh` does
    // (`docs/facts/codex-as-an-agent.md`).
    policy.verify_tls = true;
    policy
}

// An error for a turn that failed with `detail`, unreachable where Codex says
// its provider could not be reached or is overloaded.
fn failed(detail: &str) -> AgentError {
    // Codex words a status as "exceeded retry limit, last status: 503 ..." or
    // "unexpected status 400 ...", so the status leads from there.
    let status = STATUS_MARKERS
        .iter()
        .find_map(|marker| detail.split_once(marker))
        .map_or(detail, |(_, status)| status);
    // A 400 prints the provider's JSON body alone, with no status: it answered.
    let answered = detail.trim_start().starts_with('{');
    if !answered && reaches_no_host(status, &["502", "503", "504"], &UNREACHABLE) {
        AgentError::Unreachable(CODEX, detail.to_owned())
    } else {
        AgentError::Failed(CODEX, detail.to_owned())
    }
}

/// What Codex puts before the status of a response it gave up on
const STATUS_MARKERS: [&str; 2] = ["last status: ", "unexpected status "];

/// What Codex prints, in lower case, when its provider cannot be reached or
/// is overloaded, beyond a refused or reset connection
///
/// Recorded from codex-cli 0.159.3 (`docs/facts/codex-as-an-agent.md`): a
/// dropped stream, a failed connection, a 500 and a 503 that says the server
/// is overloaded.
const UNREACHABLE: [&str; 4] = [
    "stream disconnected before completion",
    "error sending request",
    "currently experiencing high demand",
    "selected model is at capacity",
];

/// One `codex exec` turn, read from what it printed
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecTurn {
    /// The Codex session it ran in
    thread: String,
    /// Its last message's text
    text: String,
    /// The session's tokens so far, this turn's included
    spent: Spent,
}

// `codex exec --json` prints one event per line. The answer is the last agent
// message. Its tokens are the session's so far, earlier turns of a resumed
// session included (`docs/facts/codex-as-an-agent.md`). A ChatGPT plan has no
// price per call, so each is priced at OpenAI's list price (`price`).
fn parse_result(output: &Output, known: Option<&Thread>) -> Result<ExecTurn, AgentError> {
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
        cache_write_input_tokens: u64,
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
                usage = Some(Spent {
                    input_tokens: u.input_tokens,
                    cached_input_tokens: u.cached_input_tokens,
                    cache_write_input_tokens: u.cache_write_input_tokens,
                    output_tokens: u.output_tokens,
                });
            }
            "turn.failed" => failure = event.error.map(|e| e.message),
            "error" => error = event.message.or(event.error.map(|e| e.message)),
            _ => {}
        }
    }
    // A turn that ended well is a success whatever retries came before it.
    if let Some(why) = failure.or(usage.is_none().then_some(error).flatten()) {
        return Err(failed(&why));
    }
    if !output.status.success() {
        return Err(failed(tail(&stderr)));
    }
    match (thread, text, usage) {
        (Some(thread), Some(text), Some(spent)) => Ok(ExecTurn {
            thread,
            text,
            spent,
        }),
        _ => Err(AgentError::Unreadable(CODEX, tail(&stdout).to_owned())),
    }
}
