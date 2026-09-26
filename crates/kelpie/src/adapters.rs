//! The real ports: the system clock, `gh` and the `claude` command line

use std::ffi::OsString;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::ports::{
    Claude, ClaudeCall, ClaudeError, ClaudeReply, Clock, Forge, ForgeError, SessionId, Timestamp,
    Visibility,
};
use crate::settings::ForgeSlug;

/// The machine's wall clock
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Timestamp(since_epoch.as_secs())
    }
}

/// GitHub, through the `gh` command line
#[derive(Debug, Clone, Copy, Default)]
pub struct Gh;

impl Forge for Gh {
    fn visibility(&self, repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        let output = Command::new("gh")
            .args(["repo", "view", repo.as_str(), "--json", "visibility"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| ForgeError::Spawn(e.kind()))?;
        if !output.status.success() {
            return Err(ForgeError::Failed(
                String::from_utf8_lossy(&output.stderr).into(),
            ));
        }
        parse_visibility(&output.stdout)
    }
}

fn parse_visibility(stdout: &[u8]) -> Result<Visibility, ForgeError> {
    #[derive(Deserialize)]
    struct View {
        visibility: String,
    }
    let unreadable = || ForgeError::Unreadable(String::from_utf8_lossy(stdout).into());
    let view: View = serde_json::from_slice(stdout).map_err(|_| unreadable())?;
    match view.visibility.as_str() {
        "PUBLIC" => Ok(Visibility::Public),
        "PRIVATE" => Ok(Visibility::Private),
        "INTERNAL" => Ok(Visibility::Internal),
        _ => Err(unreadable()),
    }
}

/// Headless Claude Code, one `claude -p` process per call
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeCli;

impl Claude for ClaudeCli {
    fn run(&self, call: &ClaudeCall) -> Result<ClaudeReply, ClaudeError> {
        // A `claude -p` with an open stdin waits on it before starting.
        let output = Command::new("claude")
            .args(argv(call))
            .current_dir(&call.cwd)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| ClaudeError::Spawn(e.kind()))?;
        parse_result(&output)
    }
}

fn argv(call: &ClaudeCall) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "-p".into(),
        call.prompt.as_str().into(),
        "--model".into(),
        call.model.as_str().into(),
        "--effort".into(),
        call.effort.as_str().into(),
        "--output-format".into(),
        "json".into(),
    ];
    if let Some(session) = &call.resume {
        argv.extend(["--resume".into(), session.0.as_str().into()]);
    }
    argv
}

// `claude -p` exits 1 on an error result but still prints its JSON, so the
// JSON is read before the exit status.
fn parse_result(output: &Output) -> Result<ClaudeReply, ClaudeError> {
    #[derive(Deserialize)]
    struct Result_ {
        is_error: bool,
        session_id: String,
        #[serde(default)]
        result: String,
    }
    match serde_json::from_slice::<Result_>(&output.stdout) {
        Ok(r) if !r.is_error && output.status.success() => Ok(ClaudeReply {
            session_id: SessionId(r.session_id),
            text: r.result,
        }),
        Ok(r) => Err(ClaudeError::Failed(r.result)),
        Err(_) if !output.status.success() => Err(ClaudeError::Failed(
            String::from_utf8_lossy(&output.stderr).into(),
        )),
        Err(_) => Err(ClaudeError::Unreadable(
            String::from_utf8_lossy(&output.stdout).into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::ExitStatus;

    use super::*;
    use crate::ports::Role;
    use crate::settings::Effort;

    // Recorded from Claude Code 2.1.283: `claude -p` on Haiku, asked to say ok.
    const RESULT: &str = include_str!("../fixtures/claude-p-result.json");

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    fn call(resume: Option<&str>) -> ClaudeCall {
        ClaudeCall {
            role: Role::Worker,
            model: "claude-sonnet-5".into(),
            effort: Effort::Medium,
            resume: resume.map(|s| SessionId(s.into())),
            cwd: PathBuf::from("/tmp"),
            prompt: "implement #6".into(),
        }
    }

    #[test]
    fn a_fresh_call_passes_model_effort_and_json_output() {
        let argv: Vec<_> = argv(&call(None))
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect();
        assert_eq!(
            argv,
            [
                "-p",
                "implement #6",
                "--model",
                "claude-sonnet-5",
                "--effort",
                "medium",
                "--output-format",
                "json",
            ]
        );
    }

    #[test]
    fn a_resumed_call_names_its_session() {
        let argv = argv(&call(Some("abc")));
        assert_eq!(
            argv[argv.len() - 2..],
            ["--resume".into(), OsString::from("abc")]
        );
    }

    #[test]
    fn a_success_result_is_read() {
        let reply = parse_result(&output(0, RESULT, "")).unwrap();
        assert_eq!(reply.session_id.0, "f83901c5-6d39-421b-b709-7828b56ba237");
        assert_eq!(reply.text, "ok");
    }

    #[test]
    fn an_error_result_is_a_failure_carrying_its_text() {
        let text = RESULT.replace("\"is_error\":false", "\"is_error\":true");
        let err = parse_result(&output(1, &text, "")).unwrap_err();
        assert_eq!(err, ClaudeError::Failed("ok".into()));
    }

    #[test]
    fn no_json_and_a_failed_exit_reports_stderr() {
        let err = parse_result(&output(1, "", "not logged in")).unwrap_err();
        assert_eq!(err, ClaudeError::Failed("not logged in".into()));
    }

    #[test]
    fn no_json_and_a_clean_exit_is_unreadable() {
        let err = parse_result(&output(0, "hello", "")).unwrap_err();
        assert_eq!(err, ClaudeError::Unreadable("hello".into()));
    }

    #[test]
    fn gh_visibility_is_read() {
        let read = |s: &str| parse_visibility(s.as_bytes());
        assert_eq!(read(r#"{"visibility":"PUBLIC"}"#), Ok(Visibility::Public));
        assert_eq!(read(r#"{"visibility":"PRIVATE"}"#), Ok(Visibility::Private));
        assert_eq!(
            read(r#"{"visibility":"INTERNAL"}"#),
            Ok(Visibility::Internal)
        );
        assert!(matches!(
            read(r#"{"visibility":"SECRET"}"#),
            Err(ForgeError::Unreadable(_))
        ));
    }
}
