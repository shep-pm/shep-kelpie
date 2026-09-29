//! `kelpie relay-gate <kelpie>`: the PreToolUse hook that holds the relay to
//! its two kelpie commands
//!
//! Permission rules alone cannot refuse "everything else": a command no rule
//! names falls to a prompt on the maintainer's phone, and `dontAsk` refuses
//! the `ask` rule on `relay-yes` along with it. So the gate refuses every
//! other tool call outright, and lets the two commands on to the rules.

use std::io::Read;

use serde::Deserialize;

use crate::confine::Verdict;
use crate::runner::ProjectName;

/// The tools the relay needs besides Bash: finding and sending a push
const PASSED: [&str; 2] = ["ToolSearch", "PushNotification"];

/// Judges the tool call in `input` against the relay's two commands, each
/// run as `kelpie`
pub fn judge(input: impl Read, kelpie: &str) -> Verdict {
    #[derive(Deserialize)]
    struct Call {
        tool_name: String,
        #[serde(default)]
        tool_input: ToolInput,
    }
    #[derive(Deserialize, Default)]
    struct ToolInput {
        command: Option<String>,
    }
    let call: Call = match serde_json::from_reader(input) {
        Ok(call) => call,
        Err(e) => return Verdict::Refuse(format!("kelpie cannot read this tool call: {e}")),
    };
    let passed = PASSED.contains(&call.tool_name.as_str())
        || call.tool_name == "Bash"
            && call
                .tool_input
                .command
                .is_some_and(|c| is_relay_command(&c, kelpie));
    if passed {
        return Verdict::Allow;
    }
    Verdict::Refuse(format!(
        "the relay runs only `{kelpie} relay-yes` and `{kelpie} relay-answer`, \
         exactly as its instructions write them. Tell the maintainer what failed, \
         word for word, and wait for them."
    ))
}

// `<kelpie> relay-yes <project> <id>`, or `<kelpie> relay-answer <project>`
// and one single-quoted word, so nothing can ride along after it.
fn is_relay_command(command: &str, kelpie: &str) -> bool {
    let Some(rest) = command.trim().strip_prefix(kelpie) else {
        return false;
    };
    if let Some(id) = rest.strip_prefix(" relay-yes ").and_then(after_project) {
        return !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
    }
    rest.strip_prefix(" relay-answer ")
        .and_then(after_project)
        .is_some_and(is_single_quoted)
}

// What follows a valid project name and one space.
fn after_project(args: &str) -> Option<&str> {
    let (project, last) = args.split_once(' ')?;
    ProjectName::try_from(project).is_ok().then_some(last)
}

// Quoted runs, and the `\'` a quote inside them is written as, with nothing
// the shell would read as more than one word.
fn is_single_quoted(mut word: &str) -> bool {
    let mut quoted = false;
    while !word.is_empty() {
        if let Some(inner) = word.strip_prefix('\'') {
            let Some(end) = inner.find('\'') else {
                return false;
            };
            word = &inner[end + 1..];
            quoted = true;
        } else if let Some(rest) = word.strip_prefix(r"\'") {
            word = rest;
        } else {
            return false;
        }
    }
    quoted
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const KELPIE: &str = "/k/bin/kelpie";

    fn bash(command: &str) -> Verdict {
        let call = json!({ "tool_name": "Bash", "tool_input": { "command": command } });
        judge(call.to_string().as_bytes(), KELPIE)
    }

    fn refused(verdict: &Verdict) -> bool {
        matches!(verdict, Verdict::Refuse(_))
    }

    #[test]
    fn the_two_kelpie_commands_go_on_to_the_permission_rules() {
        for command in [
            "/k/bin/kelpie relay-yes kelpie-scratch 6",
            "/k/bin/kelpie relay-answer kelpie-scratch '6 answer #'",
            "/k/bin/kelpie relay-answer shep '3 no rename it; keep the tests && all'",
            r"/k/bin/kelpie relay-answer shep '3 answer don'\''t'",
        ] {
            assert_eq!(bash(command), Verdict::Allow, "{command}");
        }
    }

    // What the relay ran on #80, and what a relay could try next.
    #[test]
    fn shep_and_every_other_command_are_refused() {
        for command in [
            "shep trigger kelpie-scratch rule '6 yes'",
            "shep daemon reload",
            "kelpie relay-yes kelpie-scratch 6",
            "echo hello",
            "/k/bin/kelpie runner shep",
            "/k/bin/kelpie relay-yes shep 3 && shep daemon reload",
            "/k/bin/kelpie relay-yes shep 3; echo",
            "/k/bin/kelpie relay-yes shep abc",
            "/k/bin/kelpie relay-yes shep",
            "/k/bin/kelpie relay-yes shep $(echo 3)",
            "/k/bin/kelpie relay-answer shep '3 no x' && shep daemon reload",
            "/k/bin/kelpie relay-answer shep '3 no x'; echo",
            "/k/bin/kelpie relay-answer shep \"3 no x\"",
            "/k/bin/kelpie relay-answer shep '3 no x",
            "/k/bin/kelpie relay-answer ../shep '3 no x'",
            "/k/bin/kelpie-other relay-yes shep 3",
        ] {
            assert!(refused(&bash(command)), "{command}");
        }
    }

    #[test]
    fn a_refusal_tells_the_relay_to_report_and_not_work_around_it() {
        let Verdict::Refuse(why) = bash("shep daemon reload") else {
            panic!("shep was let through");
        };
        assert!(why.contains("/k/bin/kelpie relay-yes"), "{why}");
        assert!(why.contains("Tell the maintainer what failed"), "{why}");
    }

    #[test]
    fn a_push_and_finding_its_tool_go_through() {
        for tool in PASSED {
            let call = json!({ "tool_name": tool, "tool_input": {} });
            assert_eq!(judge(call.to_string().as_bytes(), KELPIE), Verdict::Allow);
        }
    }

    #[test]
    fn every_other_tool_is_refused() {
        for tool in ["Write", "Edit", "Read", "WebFetch", "Agent", "SendMessage"] {
            let call = json!({ "tool_name": tool, "tool_input": { "file_path": "/tmp/x" } });
            assert!(
                refused(&judge(call.to_string().as_bytes(), KELPIE)),
                "{tool}"
            );
        }
    }

    #[test]
    fn an_unreadable_call_is_refused() {
        assert!(refused(&judge(&b"not json"[..], KELPIE)));
    }
}
