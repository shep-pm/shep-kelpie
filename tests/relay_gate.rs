//! The relay's PreToolUse hook, run as its settings write it, against the
//! real binary: kelpie's two commands pass, and `shep` does not.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::json;
use shep_kelpie::relay::BarePath;

const KELPIE: &str = env!("CARGO_BIN_EXE_shep-kelpie");

// Claude Code runs a hook's command through the shell, with the call on stdin.
fn hook(tool: &str, command: &str) -> Output {
    hook_run_as(KELPIE, tool, command)
}

fn hook_run_as(kelpie_path: &str, tool: &str, command: &str) -> Output {
    let program = BarePath::of(Path::new(kelpie_path)).unwrap();
    let settings = shep_kelpie::relay::settings(Path::new("/k/shep"), program);
    let line = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut child = Command::new("sh")
        .args(["-c", &line])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let call = json!({ "tool_name": tool, "tool_input": { "command": command } });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(call.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn the_relays_own_commands_pass() {
    for command in [
        format!("{KELPIE} relay-yes shep 3"),
        format!("{KELPIE} relay-answer shep '3 no rename it'"),
    ] {
        let output = hook("Bash", &command);
        assert!(output.status.success(), "{command}: {output:?}");
    }
}

#[test]
fn shep_is_refused_with_the_reason() {
    let output = hook("Bash", "shep daemon reload");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Tell the maintainer what failed"),
        "{stderr}"
    );
}

// Claude Code lets a call through on any hook exit but 2.
#[test]
fn a_kelpie_that_cannot_run_still_refuses() {
    let gone = tempfile::tempdir().unwrap().path().join("kelpie");
    let gone = gone.to_str().unwrap();
    let output = hook_run_as(gone, "Bash", &format!("{gone} relay-yes shep 3"));
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}
