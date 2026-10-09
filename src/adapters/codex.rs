//! Codex, headless as `codex exec --json`, one process per call, on the
//! ChatGPT account kelpie's own Codex login is signed in to
//!
//! Each call runs whole inside kelpie's sandbox, as Claude Code's do, with
//! Codex's own sandbox and approvals off: on macOS a sandbox cannot start
//! inside another. Kelpie's login lives in `codex_home`, apart from the
//! maintainer's `~/.codex`, which no call reads. Each call gets a Codex home
//! of its own beside its settings, holding its sessions and Codex's
//! database, with the login's `auth.json` linked in for Codex to read and
//! not write. Kelpie never reads the login itself. A fenced call runs
//! kelpie's checks as Codex's own `PreToolUse` hooks: `kelpie confine` on
//! each `apply_patch`, the tool Codex writes files with, and `kelpie guard`
//! on each shell command.
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
use crate::guard::{FOLDER_FLAG, PIN_FLAG};
use crate::ports::{
    AgentCall, AgentError, AgentReply, Agents, CallActivity, Ending, Fence, Policy, Sandbox,
    Session, SessionId, Tools, Usage, written_at,
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
/// login be read, not written, and a refreshed login that could not be
/// saved would be lost. The Codex usage read runs outside the sandbox
/// before each dispatch, on kelpie's own login, and refreshes it there.
const MODEL_HOST: &str = "chatgpt.com";

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

/// The only folders in a call's Codex home the call may write: its
/// sessions, databases, logs and locks
///
/// Everything else there is Codex's to load, not to write: its login, its
/// config, `hooks.json`, `.env` (whose variables reach every hook's shell),
/// `AGENTS.md` and `AGENTS.override.md`, rules, skills and the model catalog.
/// A list of those would miss the next one Codex adds, so the call writes
/// only these. Not `tmp`: Codex puts a folder in it first on every hook's
/// PATH, so a `git` planted there would run inside `kelpie guard`. Codex
/// runs on without it (Facts).
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
/// its hooks or its sandbox
///
/// `unified_exec` is not among them: Codex turns it back on unless an
/// administrator's `requirements.toml` pins it off (Facts).
const FEATURES_OFF: [&str; 16] = [
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
    login: PathBuf,
}

impl ClaudeCli {
    /// Codex on the login in `codex_home`, run the way this runs Claude
    /// Code: same sandbox and lamb labels, and stopped with it
    pub fn codex(&self, codex_home: PathBuf) -> CodexCli {
        CodexCli {
            processes: self.processes.clone(),
            program: "codex".into(),
            lambs: self.lambs.clone(),
            sandbox: Arc::clone(&self.sandbox),
            login: codex_home,
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
        let written = WRITTEN.map(|name| files.home.join(name));
        for folder in [&files.threads, &files.home].into_iter().chain(&written) {
            std::fs::create_dir_all(folder).map_err(|e| {
                AgentError::Setup(format!("cannot make {}: {}", folder.display(), e.kind()))
            })?;
        }
        link_login(&self.login, &files.home)
    }

    fn run(&self, call: &AgentCall, ending: &Ending) -> Result<AgentReply, AgentError> {
        let files = Files::of(call);
        let mut command = self.sandboxed_command(call)?;
        let label = call.label();
        let spawned = |pid| {
            if let Some(lambs) = &self.lambs {
                lambs.label(pid, &label);
            }
        };
        // Codex dies of a full stdout pipe the sandbox's Node made non-blocking (Facts).
        let outputs = [files.stdout.as_path(), files.stderr.as_path()];
        let run = self
            .processes
            .output_to_files(&mut command, Some(ending), &spawned, outputs);
        let output = run.map_err(|e| match e {
            RunError::Io(e) => AgentError::Spawn(CODEX, e.to_string()),
            RunError::Stopped => AgentError::Stopped,
            RunError::TimedOut => AgentError::TimedOut(CODEX),
        })?;
        // The model has run by now, so what fails from here is the call's,
        // not its setup's.
        let ran = |e: AgentError| match e {
            AgentError::Setup(why) => AgentError::Failed(CODEX, why),
            e => e,
        };
        let known = thread(&files, call.session.id()).map_err(ran)?;
        let turn = parse_result(&output, known.as_ref())?;
        let before = known.map(|k| k.spent).unwrap_or_default();
        let record = Thread {
            id: turn.thread,
            spent: turn.spent,
        };
        let path = files.thread(call.session.id());
        let text = serde_json::to_string(&record).expect("a thread is JSON");
        std::fs::write(&path, text).map_err(|e| {
            ran(AgentError::Setup(format!(
                "cannot write {}: {}",
                path.display(),
                e.kind()
            )))
        })?;
        Ok(AgentReply {
            session_id: call.session.id().clone(),
            text: turn.text,
            usage: turn.spent.since(before),
            session_cost: None,
            context: None,
        })
    }

    // `codex exec --json` writes a line to its output file at each event.
    fn last_active(&self, call: &AgentCall) -> CallActivity {
        written_at(&Files::of(call).stdout)
    }
}

impl CodexCli {
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
        let mut command = crate::spawn::command(&self.program);
        command
            .args(argv(
                call,
                &files.home,
                resumed.as_ref(),
                instructions.as_deref(),
            )?)
            .current_dir(&call.cwd)
            .env("CODEX_HOME", &files.home)
            .env("TMPDIR", &files.scratch);
        if let Some(fence) = &call.reach.fence {
            command.envs(&fence.env);
        }
        self.sandbox
            .wrap(
                &policy(call, &self.login, &files),
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
    /// The call's own Codex home: its sessions, Codex's database and logs,
    /// and the login linked in
    home: PathBuf,
    /// Which Codex session each of kelpie's session ids is, out of the
    /// call's reach
    threads: PathBuf,
    /// The call's temporary files, emptied before each call
    scratch: PathBuf,
    /// Where the sandbox's settings go
    sandbox_settings: PathBuf,
    /// Codex's stdout and stderr, which kelpie opens for it outside the
    /// call's reach and reads when it ends
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Files {
    fn of(call: &AgentCall) -> Self {
        Self {
            home: call.settings.with_extension("codex"),
            threads: call.settings.with_extension("threads"),
            scratch: call.settings.with_extension("tmp"),
            sandbox_settings: call.settings.with_extension("sandbox.json"),
            stdout: call.settings.with_extension("stdout.jsonl"),
            stderr: call.settings.with_extension("stderr.log"),
        }
    }

    fn thread(&self, id: &SessionId) -> PathBuf {
        self.threads.join(format!("{}.json", id.0))
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
    std::os::unix::fs::symlink(&target, &link).map_err(setup)
}

/// The Codex session one of kelpie's session ids is
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Thread {
    /// Codex's own id for it
    id: String,
    /// The session's tokens as Codex last reported them, which a resumed
    /// turn's report runs on from
    #[serde(default)]
    spent: Spent,
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

// Codex ignores the user config and rules, and keeps its databases and
// logs in folders of `home` the call may write. It reads the repo's AGENTS.md.
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
    let mut off: Vec<&str> = FEATURES_OFF.into();
    // Only a worker runs commands. A review round reads what
    // its prompt holds, as Claude Code's do, with no command to run.
    if call.tools != Tools::Work {
        off.push("shell_tool");
    }
    for feature in off {
        argv.extend(["--disable".into(), feature.into()]);
    }
    let hooks = match &call.reach.fence {
        Some(fence) => hooks(fence),
        None => no_writes(),
    };
    // Codex runs a hook only once it is trusted; kelpie wrote these itself.
    argv.push("--dangerously-bypass-hook-trust".into());
    argv.extend(["-c".into(), config("hooks.PreToolUse", hooks)]);
    argv.push("--".into());
    if let Some(thread) = resumed {
        argv.push(thread.id.as_str().into());
    }
    argv.push(prompt(call)?.into());
    Ok(argv)
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
        shell_quote(PIN_FLAG),
    ]
    .into_iter()
    .chain(
        guard
            .folders
            .iter()
            .map(|p| shell_quote(&format!("{FOLDER_FLAG}{}", p.display()))),
    )
    .collect();
    toml::Value::Array(vec![
        hook_entry("^apply_patch$", confine),
        hook_entry("^Bash$", commands.join(" ")),
    ])
}

/// The `PreToolUse` hook of a call with no fence, which writes no file
///
/// Its model's `apply_patch` cannot be turned off from a call, so the hook
/// refuses every one.
fn no_writes() -> toml::Value {
    let refuse = "echo 'kelpie: this call writes no files' >&2; exit 2";
    toml::Value::Array(vec![hook_entry("^apply_patch$", refuse.into())])
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
fn policy(call: &AgentCall, login: &Path, files: &Files) -> Policy {
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
            .chain(call.instructions.iter().cloned())
            .chain(call.plugin_dirs.iter().cloned())
            .chain(call.reach.read.iter().cloned()),
    );
    policy.hosts.push(MODEL_HOST.to_owned());
    // Codex verifies the model's certificate through macOS, as `gh` does (Facts).
    policy.verify_tls = true;
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
    /// The session's tokens so far, this turn's included
    spent: Spent,
}

// `codex exec --json` prints one event per line: the session it runs in,
// each item as it starts and ends, and the turn's end with its tokens or
// its failure. The answer is the last agent message. Its tokens are the
// session's so far, a resumed session's earlier turns included (Facts). A
// ChatGPT plan has no price per call, so kelpie records each one as unpriced.
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
    let failed = |detail: &str| AgentError::Failed(CODEX, detail.to_owned());
    // A turn that ended well is a success whatever retries came before it.
    if let Some(why) = failure.or(usage.is_none().then_some(error).flatten()) {
        return Err(failed(&why));
    }
    if !output.status.success() {
        return Err(failed(tail(&stderr)));
    }
    match (thread, text, usage) {
        (Some(thread), Some(text), Some(spent)) => Ok(Turn {
            thread,
            text,
            spent,
        }),
        _ => Err(AgentError::Unreadable(CODEX, tail(&stdout).to_owned())),
    }
}
