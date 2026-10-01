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
    // `disableArtifact` is deprecated in 2.1.286 in favour of this.
    settings["enableArtifact"] = json!(false);
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

/// What a role's `deny` list must be: its own rules, then every trimmed tool it lacks
#[cfg(test)]
pub(crate) fn deny_with_trim(own: &[&str]) -> Value {
    let mut rules: Vec<&str> = own.to_vec();
    rules.extend(TRIM_DENY.iter().filter(|t| !own.contains(t)));
    json!(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_artifact_tool_is_off_under_both_keys() {
        let s = trimmed(json!({}));
        assert_eq!(s["disableArtifact"], true);
        assert_eq!(s["enableArtifact"], false);
    }

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
