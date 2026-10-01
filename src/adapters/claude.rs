//! Headless Claude Code, one `claude -p` process per call

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

use serde::Deserialize;

use super::process::{Processes, RunError};
use super::srt::SandboxRuntime;
use crate::ports::{
    AgentCall, AgentError, AgentReply, Agents, Cost, Role, Sandbox, Session, SessionId, Usage,
};
use crate::preview::Tools;
use crate::settings::Harness;

/// This adapter's harness, as its errors name it
const CLAUDE: Harness = Harness::ClaudeCode;

#[cfg(test)]
mod escapes;
pub(crate) mod sandbox;
pub(crate) mod settings;

/// What `claude -p --resume` prints when the session has no transcript
const NO_SESSION: &str = "No conversation found with session ID";

/// Names a runner's child process in `shep describe`
pub trait LambLabels: Send + Sync + fmt::Debug {
    /// Labels the child `pid` with `label`
    fn label(&self, pid: u32, label: &str);
}

impl LambLabels for shep_channel::Shepherd {
    // A label kelpie builds is short and plain, so a refused one is dropped
    // rather than failing the call it names.
    fn label(&self, pid: u32, label: &str) {
        if let Ok(label) = shep_channel::LambLabel::new(label) {
            let _ = self.label_lamb(pid, label);
        }
    }
}

/// Headless Claude Code, one `claude -p` process per call, each inside the sandbox
///
/// Clones share their calls in flight, so one clone can stop them all.
#[derive(Debug, Clone)]
pub struct ClaudeCli {
    pub(super) processes: Processes,
    program: OsString,
    pub(super) lambs: Option<Arc<dyn LambLabels>>,
    pub(super) sandbox: Arc<dyn Sandbox>,
    home: PathBuf,
}

// The default home and tools are the maintainer's: `$HOME` and `~/.kelpie/tools`.
impl Default for ClaudeCli {
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        Self {
            processes: Processes::default(),
            program: "claude".into(),
            lambs: None,
            sandbox: Arc::new(SandboxRuntime::new(Tools::under(&home.join(".kelpie")))),
            home,
        }
    }
}

impl ClaudeCli {
    /// Labels each call's process with its issue and role, such as `#7 worker`
    pub fn labelling(lambs: Arc<dyn LambLabels>) -> Self {
        Self {
            lambs: Some(lambs),
            ..Self::default()
        }
    }

    /// Runs `program` in place of `claude`, as a stand-in script does
    #[cfg(test)]
    pub(crate) fn with_program(program: PathBuf) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }

    /// Runs each call inside `sandbox`, as Claude Code with its home at `home`
    pub fn sandboxed(self, sandbox: Arc<dyn Sandbox>, home: PathBuf) -> Self {
        Self {
            sandbox,
            home,
            ..self
        }
    }

    /// Ends every call in flight, and refuses new ones, as the runner stops
    ///
    /// A call ended this way returns [`AgentError::Stopped`].
    pub fn stop(&self) {
        self.processes.stop();
    }
}

impl Agents for ClaudeCli {
    fn prepare(&self, call: &AgentCall) -> Result<(), AgentError> {
        write_settings(call)?;
        sandbox::policy(call, &self.home).map(drop)
    }

    fn run(&self, call: &AgentCall) -> Result<AgentReply, AgentError> {
        // The bridges stay open until the call has ended.
        let (mut command, _bridges) = self.sandboxed_command(call)?;
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
            RunError::Io(e) => AgentError::Spawn(CLAUDE, e.to_string()),
            RunError::Stopped => AgentError::Stopped,
            RunError::TimedOut => AgentError::TimedOut(CLAUDE),
        })?;
        parse_result(&output, &call.session)
    }
}

/// Writes the call's settings file whole, from the call alone
pub(crate) fn write_settings(call: &AgentCall) -> Result<(), AgentError> {
    let text = serde_json::to_string_pretty(&settings::settings(call.tools, &call.reach))
        .expect("settings are JSON");
    let folder = call.settings.parent().unwrap_or(std::path::Path::new("/"));
    std::fs::create_dir_all(folder)
        .and_then(|()| std::fs::write(&call.settings, text))
        .map_err(|e| {
            AgentError::Setup(format!(
                "cannot write {}: {}",
                call.settings.display(),
                e.kind()
            ))
        })
}

// `--setting-sources project` keeps the project's CLAUDE.md and skills and
// drops the maintainer's own hooks, plugins and skills. It also drops the
// worktree's `settings.local.json`, which nothing kelpie runs needs.
fn argv(call: &AgentCall, mcp_config: Option<&Path>) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "-p".into(),
        call.prompt.as_str().into(),
        "--model".into(),
        call.model.as_str().into(),
        "--effort".into(),
        call.effort.as_str().into(),
        "--setting-sources".into(),
        "project".into(),
        "--settings".into(),
        call.settings.clone().into(),
        "--output-format".into(),
        "json".into(),
    ];
    if call.role == Role::Worker {
        argv.extend(["--permission-mode".into(), "bypassPermissions".into()]);
    }
    if let Some(config) = mcp_config {
        argv.extend(["--mcp-config".into(), config.into()]);
    }
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
    Ok(AgentReply {
        session_id: SessionId(r.session_id),
        text: r.result,
        usage: Usage {
            input: u.input_tokens,
            cache_write: u.cache_creation_input_tokens,
            cache_read: u.cache_read_input_tokens,
            output: u.output_tokens,
        },
        session_cost: Some(cost),
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::ExitStatus;

    use super::*;
    use crate::ports::{Reach, Tools};
    use crate::settings::Effort;
    use crate::test::OpenSandbox;

    // Recorded from Claude Code 2.1.283 on Haiku: a call asked to say ok.
    const RESULT: &str = include_str!("../../fixtures/claude-p-result.json");
    // Recorded from Claude Code 2.1.283 on Haiku: a new session, then the
    // same session resumed for a second call.
    const FRESH: &str = include_str!("../../fixtures/claude-p-fresh.json");
    const RESUMED: &str = include_str!("../../fixtures/claude-p-resumed.json");
    // Recorded: `--resume` onto a session killed before it wrote a transcript.
    const NO_SESSION_STDERR: &str = include_str!("../../fixtures/claude-p-no-session.stderr");

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    fn resume(id: &str) -> Session {
        Session::Resume(SessionId(id.into()))
    }

    fn fresh() -> Session {
        Session::New(SessionId("7e812e8a-3bf1-42a7-bddf-0ab283b7372a".into()))
    }

    fn call(role: Role, session: Session) -> AgentCall {
        AgentCall {
            harness: crate::settings::AgentHarness::ClaudeCode,
            role,
            issue: 6,
            model: "claude-sonnet-5".into(),
            effort: Effort::Medium,
            session,
            cwd: PathBuf::from("/tmp"),
            settings: PathBuf::from("/k/worker/settings.json"),
            instructions: Some(PathBuf::from("/k/worker/instructions.md")),
            prompt: "implement #6".into(),
            timeout: None,
            mcp_config: None,
            plugin_dirs: Vec::new(),
            tools: Tools::Work,
            reach: Reach::default(),
            lease: None,
        }
    }

    fn strings(call: &AgentCall) -> Vec<String> {
        argv(call, call.mcp_config.as_deref())
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    #[test]
    fn a_new_worker_session_carries_the_whole_profile() {
        assert_eq!(
            strings(&call(Role::Worker, fresh())),
            [
                "-p",
                "implement #6",
                "--model",
                "claude-sonnet-5",
                "--effort",
                "medium",
                "--setting-sources",
                "project",
                "--settings",
                "/k/worker/settings.json",
                "--output-format",
                "json",
                "--permission-mode",
                "bypassPermissions",
                "--session-id",
                "7e812e8a-3bf1-42a7-bddf-0ab283b7372a",
                "--append-system-prompt-file",
                "/k/worker/instructions.md",
            ]
        );
    }

    #[test]
    fn a_resumed_session_names_it_and_keeps_its_instructions() {
        let argv = strings(&call(Role::Worker, resume("abc")));
        assert_eq!(argv[argv.len() - 2..], ["--resume", "abc"]);
        assert!(!argv.iter().any(|a| a == "--append-system-prompt-file"));
        assert!(!argv.iter().any(|a| a == "--session-id"));
    }

    #[test]
    fn a_call_with_mcp_servers_names_their_config() {
        let mut with = call(Role::Worker, fresh());
        with.mcp_config = Some(PathBuf::from("/k/worker/mcp.json"));
        let argv = strings(&with);
        let at = argv.iter().position(|a| a == "--mcp-config").unwrap();
        assert_eq!(argv[at + 1], "/k/worker/mcp.json");
        let argv = strings(&call(Role::Worker, fresh()));
        assert!(!argv.iter().any(|a| a == "--mcp-config"));
    }

    #[test]
    fn each_plugin_folder_is_its_own_plugin_dir() {
        let mut with = call(Role::Reviewer, fresh());
        with.plugin_dirs = vec!["/k/skills/mattpocock".into(), "/k/skills/review".into()];
        let argv = strings(&with);
        let dirs: Vec<&str> = argv
            .windows(2)
            .filter(|pair| pair[0] == "--plugin-dir")
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(dirs, ["/k/skills/mattpocock", "/k/skills/review"]);
        let argv = strings(&call(Role::Judge, fresh()));
        assert!(!argv.iter().any(|a| a == "--plugin-dir"));
    }

    #[test]
    fn only_a_worker_bypasses_permissions() {
        for role in [Role::Reviewer, Role::Judge] {
            let argv = strings(&call(role, fresh()));
            assert!(!argv.iter().any(|a| a == "--permission-mode"), "{role:?}");
        }
    }

    #[test]
    fn prepare_writes_the_settings_file_the_call_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut review = call(Role::Reviewer, fresh());
        review.settings = dir.path().join("worker/review-settings.json");
        review.tools = Tools::Review;
        review.reach.read = vec![dir.path().join("shots")];
        ClaudeCli::default().prepare(&review).unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&review.settings).unwrap()).unwrap();
        assert_eq!(
            written,
            serde_json::json!({ "permissions": {
                "deny": ["Agent", "Task", "Bash"],
                "additionalDirectories": [dir.path().join("shots")],
            } })
        );

        std::fs::create_dir_all(dir.path().join("taken/settings.json")).unwrap();
        review.settings = dir.path().join("taken/settings.json");
        let err = ClaudeCli::default().prepare(&review).unwrap_err();
        assert!(
            matches!(&err, AgentError::Setup(reason) if reason.starts_with("cannot write ")),
            "{err:?}"
        );
    }

    #[test]
    fn a_success_result_is_read() {
        let reply = parse_result(&output(0, RESULT, ""), &fresh()).unwrap();
        assert_eq!(reply.session_id.0, "f83901c5-6d39-421b-b709-7828b56ba237");
        assert_eq!(reply.text, "ok");
        assert_eq!(
            reply.usage,
            Usage {
                input: 10,
                cache_write: 8003,
                cache_read: 13673,
                output: 53,
            }
        );
        assert_eq!(reply.session_cost, Some(Cost(17_648_300)));
    }

    #[test]
    fn a_resumed_call_reports_its_own_usage_and_the_session_cost_so_far() {
        let first = parse_result(&output(0, FRESH, ""), &fresh()).unwrap();
        let second = parse_result(&output(0, RESUMED, ""), &resume(&first.session_id.0)).unwrap();
        assert_eq!(first.session_id, second.session_id);
        assert_eq!(
            (first.usage, first.session_cost),
            (
                Usage {
                    input: 10,
                    cache_write: 8984,
                    cache_read: 13673,
                    output: 148,
                },
                Some(Cost(20_085_300)),
            )
        );
        assert_eq!(
            (second.usage, second.session_cost),
            (
                Usage {
                    input: 10,
                    cache_write: 193,
                    cache_read: 22657,
                    output: 34,
                },
                Some(Cost(22_917_000)),
            )
        );
    }

    #[test]
    fn resuming_a_session_that_never_started_is_named() {
        let err = parse_result(&output(1, "", NO_SESSION_STDERR), &resume("0e2c")).unwrap_err();
        assert_eq!(err, AgentError::NoSession(CLAUDE, SessionId("0e2c".into())));
    }

    #[test]
    fn the_same_stderr_for_a_new_session_is_a_plain_failure() {
        let err = parse_result(&output(1, "", NO_SESSION_STDERR), &fresh()).unwrap_err();
        assert!(matches!(err, AgentError::Failed(CLAUDE, _)), "{err:?}");
    }

    #[test]
    fn an_error_result_is_a_failure_carrying_its_text() {
        let text = RESULT.replace("\"is_error\":false", "\"is_error\":true");
        let err = parse_result(&output(1, &text, ""), &fresh()).unwrap_err();
        assert_eq!(err, AgentError::Failed(CLAUDE, "ok".into()));
    }

    #[test]
    fn a_result_without_its_cost_is_unreadable() {
        let text = RESULT.replace("\"total_cost_usd\":0.0176483,", "");
        let err = parse_result(&output(0, &text, ""), &fresh()).unwrap_err();
        assert!(matches!(err, AgentError::Unreadable(CLAUDE, _)), "{err:?}");
    }

    #[test]
    fn no_json_and_a_failed_exit_reports_stderr() {
        let err = parse_result(&output(1, "", "not logged in"), &fresh()).unwrap_err();
        assert_eq!(err, AgentError::Failed(CLAUDE, "not logged in".into()));
    }

    #[test]
    fn no_json_and_a_clean_exit_is_unreadable() {
        let err = parse_result(&output(0, "hello", ""), &fresh()).unwrap_err();
        assert_eq!(err, AgentError::Unreadable(CLAUDE, "hello".into()));
    }

    #[test]
    fn a_cost_is_kept_to_the_billionth_and_refuses_nonsense() {
        assert_eq!(Cost::from_usd(0.16096939999999998), Some(Cost(160_969_400)));
        assert_eq!(Cost::from_usd(0.0), Some(Cost(0)));
        assert_eq!(Cost::from_usd(-0.01), None);
        assert_eq!(Cost::from_usd(f64::NAN), None);
        assert_eq!(Cost(22_917_000).usd(), 0.022917);
    }

    #[derive(Debug, Default)]
    struct Recorded(std::sync::Mutex<Vec<(u32, String)>>);

    impl LambLabels for Recorded {
        fn label(&self, pid: u32, label: &str) {
            self.0.lock().unwrap().push((pid, label.to_owned()));
        }
    }

    #[test]
    fn a_call_s_lamb_carries_its_issue_and_role() {
        let dir = tempfile::tempdir().unwrap();
        let (claude, pid_file) = (dir.path().join("claude"), dir.path().join("pid"));
        std::fs::write(dir.path().join("result.json"), RESULT).unwrap();
        let script = format!(
            "#!/bin/sh\necho $$ > {}\ncat {}/result.json\n",
            pid_file.display(),
            dir.path().display()
        );
        crate::test::write_script(&claude, &script);
        let lambs = Arc::new(Recorded::default());
        let cli = ClaudeCli {
            program: claude.into(),
            ..ClaudeCli::labelling(Arc::clone(&lambs) as Arc<dyn LambLabels>)
        }
        .sandboxed(Arc::new(OpenSandbox::default()), dir.path().join("home"));
        for role in [Role::Worker, Role::Reviewer, Role::Judge] {
            let mut call = call(role, fresh());
            call.cwd = dir.path().to_owned();
            call.settings = dir.path().join("settings.json");
            cli.prepare(&call).unwrap();
            cli.run(&call).unwrap();
        }
        let labels = lambs.0.lock().unwrap().clone();
        let names: Vec<&str> = labels.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(names, ["#6 worker", "#6 reviewer", "#6 judge"]);
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(labels[2].0, pid, "the label is not on the process that ran");
    }
}
