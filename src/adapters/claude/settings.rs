//! Claude Code's settings file, built from a call's tools and reach
//!
//! Every call runs inside kelpie's sandbox, which holds the whole process.
//! These settings are the second layer: deny rules, and for a fenced session
//! a hook that runs `kelpie confine` on the file tools, `kelpie guard` on
//! every command, then any hook the project adds.

use std::path::Path;

use serde_json::{Value, json};

use crate::ports::{Fence, Reach, Tools};
use crate::settings::HookEvent;

/// The tools that write files without going through Bash
const FILE_TOOLS: &str = "Edit|Write|MultiEdit|NotebookEdit";

// Tools no worker needs that reach past its fence: `Monitor` runs a command
// or WebSocket no hook judges; `RemoteTrigger` starts cloud agents on the
// maintainer's account with none of this file; `Workflow` agents escape the
// worker's pacing; and kelpie, not the worker, owns its worktree.
const WORK_DENY: [&str; 5] = [
    "Monitor",
    "RemoteTrigger",
    "Workflow",
    "EnterWorktree",
    "ExitWorktree",
];

/// The tools a review round never uses: sub-agents, under either name, and commands
const REVIEW_DENY: [&str; 3] = ["Agent", "Task", "Bash"];

/// Every tool name a Claude Code call can reach, denied outright to an answer
pub(crate) const NO_TOOLS: [&str; 11] = [
    "Bash",
    "Read",
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "Task",
];

// What `kelpie guard` judges: every command, and a subagent's isolation.
const GUARDED_TOOLS: &str = "Bash|Agent|Task";

// Every Playwright MCP tool, which `kelpie browse-guard` holds to the preview
const PLAYWRIGHT_TOOLS: &str = "mcp__playwright__.*";

// The Playwright MCP server runs outside the sandbox. These tools read a local
// file (or run code that could), so none is the worker's.
pub(crate) const PLAYWRIGHT_DENY: [&str; 4] = [
    "mcp__playwright__browser_run_code_unsafe",
    "mcp__playwright__browser_file_upload",
    "mcp__playwright__browser_drop",
    "mcp__playwright__browser_set_storage_state",
];

/// The settings file's contents for a call with these tools and sandbox
pub(crate) fn settings(tools: Tools, reach: &Reach) -> Value {
    let mut deny: Vec<String> = Vec::new();
    if let Some(fence) = &reach.fence {
        deny.extend(fence.no_read.iter().map(|p| read_rule(p)));
        deny.extend(fence.no_commands.iter().map(|c| format!("Bash({c})")));
    }
    deny.extend(tool_denies(tools, reach).map(str::to_owned));
    let Some(fence) = &reach.fence else {
        let mut permissions = json!({ "deny": deny });
        if !reach.read.is_empty() {
            // Read outside the working folder is refused under `-p` unless the folder is added.
            permissions["additionalDirectories"] = json!(reach.read);
        }
        return json!({ "permissions": permissions });
    };
    if fence.preview.is_some() {
        deny.extend(PLAYWRIGHT_DENY.iter().map(|&r| r.to_owned()));
    }
    let mut permissions = json!({ "deny": deny });
    if !reach.read.is_empty() {
        permissions["additionalDirectories"] = json!(reach.read);
    }
    json!({
        // Kelpie's sandbox holds the whole process, and on macOS Claude Code's
        // own cannot start inside it: every command would be refused.
        "sandbox": { "enabled": false },
        "permissions": permissions,
        // A project's own settings could otherwise switch every hook off,
        // `confine` and the guard with them. This file outranks them.
        "disableAllHooks": false,
        "hooks": hooks(fence),
        "env": env(fence),
    })
}

fn tool_denies(tools: Tools, reach: &Reach) -> impl Iterator<Item = &'static str> {
    let denied: &'static [&'static str] = match tools {
        Tools::Work => &WORK_DENY,
        Tools::Review => &REVIEW_DENY,
        Tools::Answer => &NO_TOOLS,
    };
    // An answer that may read a folder keeps Read and nothing else.
    let reads = tools == Tools::Answer && !reach.read.is_empty();
    denied
        .iter()
        .copied()
        .filter(move |t| !(reads && *t == "Read"))
}

// `//` roots a rule at `/`: a single `/` is taken from the settings file.
fn read_rule(path: &str) -> String {
    match path.starts_with('/') {
        true => format!("Read(/{path})"),
        false => format!("Read({path})"),
    }
}

// The project's own variables come last, so one it names wins.
fn env(fence: &Fence) -> Value {
    let mut env = json!({});
    if fence.preview.is_some() {
        // Node's fetch ignores the sandbox's proxy without it.
        env["NODE_USE_ENV_PROXY"] = "1".into();
    }
    for (name, value) in &fence.env {
        env[name.as_str()] = json!(value);
    }
    env
}

fn hooks(fence: &Fence) -> Value {
    let guard = &fence.guard;
    let kelpie = guard.kelpie.as_path();
    let confine = [kelpie, Path::new("confine"), &guard.worktree, &guard.build]
        .map(|p| shell_quote(&p.to_string_lossy()))
        .join(" ");
    let mut pre = vec![entry(Some(FILE_TOOLS), &confine)];
    if let Some(domains) = &fence.preview {
        let browse = [kelpie.to_string_lossy().as_ref(), "browse-guard"]
            .into_iter()
            .chain(domains.iter().map(String::as_str))
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ");
        pre.push(entry(Some(PLAYWRIGHT_TOOLS), &browse));
    }
    let commands = [
        kelpie,
        Path::new("guard"),
        &guard.git_common_dir,
        &guard.worktree,
    ]
    .map(|p| shell_quote(&p.to_string_lossy()));
    pre.push(entry(Some(GUARDED_TOOLS), &commands.join(" ")));
    let mut post = Vec::new();
    for hook in &fence.hooks {
        let e = entry(
            hook.matcher.as_ref().map(|m| m.as_str()),
            hook.command.as_str(),
        );
        match hook.event {
            HookEvent::PreToolUse => pre.push(e),
            HookEvent::PostToolUse => post.push(e),
        }
    }
    let mut hooks = json!({ "PreToolUse": pre });
    if !post.is_empty() {
        hooks["PostToolUse"] = post.into();
    }
    hooks
}

fn entry(matcher: Option<&str>, command: &str) -> Value {
    let mut entry = json!({ "hooks": [{ "type": "command", "command": command }] });
    if let Some(matcher) = matcher {
        entry["matcher"] = matcher.into();
    }
    entry
}

/// `s` as one word to a POSIX shell
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn a_review_round_keeps_its_read_tools_and_reads_its_folders() {
        let bare = settings(Tools::Review, &Reach::default());
        assert_eq!(
            bare,
            json!({ "permissions": { "deny": ["Agent", "Task", "Bash"] } })
        );
        let shots = Reach {
            read: vec![PathBuf::from("/k/shots/7")],
            fence: None,
        };
        assert_eq!(
            settings(Tools::Review, &shots)["permissions"]["additionalDirectories"],
            json!(["/k/shots/7"])
        );
    }

    #[test]
    fn an_answer_reads_nothing_unless_its_sandbox_lists_a_folder() {
        let none = settings(Tools::Answer, &Reach::default());
        assert_eq!(none, json!({ "permissions": { "deny": NO_TOOLS } }));
        let shot = Reach {
            read: vec![PathBuf::from("/k/shots/7")],
            fence: None,
        };
        let deny = settings(Tools::Answer, &shot)["permissions"]["deny"].clone();
        assert_eq!(deny.as_array().unwrap().len(), NO_TOOLS.len() - 1);
        assert!(!deny.to_string().contains("\"Read\""), "{deny}");
    }
}
