//! Claude Code features no call kelpie makes uses
//!
//! Bundled skills, workflows, claude.ai connectors, the Artifact tool, plan
//! mode, and the cron, design and worktree tools each add to the first
//! turn's input, on every turn of every agent. Measured on Claude Code
//! 2.1.286 with a one-word prompt: 17,949 input tokens with an empty
//! settings file, 10,867 with these. A bare tool name in `deny` removes the
//! tool's definition; a scoped rule only blocks the call.
//!
//! A tool kelpie's code or instructions name is not here: `SendMessage`,
//! `Agent`, `Skill`, `ToolSearch` and the `Task*` tools stay. Nor is
//! `disableRemoteControl`: the relay launches with `--remote-control`.

use serde_json::{Value, json};

/// The tools no call needs, denied by bare name so their definitions go
const TRIM_DENY: [&str; 11] = [
    "EnterPlanMode",
    "ExitPlanMode",
    "DesignSync",
    "NotebookEdit",
    "CronCreate",
    "CronDelete",
    "CronList",
    "RemoteTrigger",
    "ScheduleWakeup",
    "EnterWorktree",
    "ExitWorktree",
];

/// `settings` with the trimmed features off, its own `deny` rules kept
pub(crate) fn trimmed(mut settings: Value) -> Value {
    for key in [
        "disableBundledSkills",
        "disableWorkflows",
        "disableClaudeAiConnectors",
        "disableArtifact",
    ] {
        settings[key] = json!(true);
    }
    let deny = &mut settings["permissions"]["deny"];
    let mut rules = deny.as_array().cloned().unwrap_or_default();
    for tool in TRIM_DENY {
        if !rules.iter().any(|r| r == tool) {
            rules.push(json!(tool));
        }
    }
    *deny = Value::Array(rules);
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_denied_tool_already_listed_is_not_listed_twice() {
        let s = trimmed(json!({ "permissions": { "deny": ["Bash", "ExitWorktree"] } }));
        let deny = s["permissions"]["deny"].as_array().unwrap();
        assert_eq!(deny.iter().filter(|r| *r == "ExitWorktree").count(), 1);
        assert_eq!(deny[0], "Bash");
    }

    #[test]
    fn the_other_permissions_are_kept() {
        let s = trimmed(json!({ "permissions": { "allow": ["Bash(x)"] } }));
        assert_eq!(s["permissions"]["allow"], json!(["Bash(x)"]));
    }
}
