//! Claude Code's settings file, built from a call's tools and reach
//!
//! Every call runs inside kelpie's sandbox, which holds the whole process.
//! These settings are the second layer: deny rules, and for a fenced session
//! a hook that runs `kelpie confine` on the file tools, `kelpie guard` on
//! every command, then any hook the project adds.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::guard::{FOLDER_FLAG, NAME_FLAG};
use crate::ports::{Fence, Reach, Tools};
use crate::settings::HookEvent;
use crate::trim::trimmed;

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
pub(crate) const NO_TOOLS: [&str; 12] = [
    "Agent",
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
        deny.extend(
            read_denials(&fence.no_read, &fence.read)
                .iter()
                .map(|p| read_rule(p)),
        );
        deny.extend(fence.no_commands.iter().map(|c| format!("Bash({c})")));
    }
    deny.extend(tool_denies(tools, reach).map(str::to_owned));
    let Some(fence) = &reach.fence else {
        let mut permissions = json!({ "deny": deny });
        if !reach.read.is_empty() {
            // Read outside the working folder is refused under `-p` unless the folder is added.
            permissions["additionalDirectories"] = json!(reach.read);
        }
        return trimmed(json!({ "permissions": permissions }));
    };
    if fence.preview.is_some() {
        deny.extend(PLAYWRIGHT_DENY.iter().map(|&r| r.to_owned()));
    }
    let mut permissions = json!({ "deny": deny });
    if !reach.read.is_empty() {
        permissions["additionalDirectories"] = json!(reach.read);
    }
    trimmed(json!({
        // Kelpie's sandbox holds the whole process, and on macOS Claude Code's
        // own cannot start inside it: every command would be refused.
        "sandbox": { "enabled": false },
        "permissions": permissions,
        // A project's own settings could otherwise switch every hook off,
        // `confine` and the guard with them. This file outranks them.
        "disableAllHooks": false,
        "hooks": hooks(fence),
        "env": env(fence),
    }))
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
// Claude Code's deny rules take no exceptions, so a folder holding one the
// session may read is denied entry by entry around it. The sandbox still
// denies the whole folder, entries made later included.
fn read_denials(no_read: &[String], read: &[PathBuf]) -> Vec<String> {
    let holds_one = |dir: &Path| read.iter().any(|r| r.starts_with(dir));
    no_read
        .iter()
        .flat_map(|rule| match rule.strip_suffix("/**").map(Path::new) {
            Some(dir) if holds_one(dir) => around(dir, read),
            _ => vec![rule.clone()],
        })
        .collect()
}

// A rule for each entry of `dir` that is not, and holds none of, `read`.
fn around(dir: &Path, read: &[PathBuf]) -> Vec<String> {
    // A folder it cannot list is denied whole, as the sandbox denies it.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![format!("{}/**", dir.display())];
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut rules = Vec::new();
    for path in paths {
        if read.iter().any(|r| path.starts_with(r)) {
            continue;
        }
        if read.iter().any(|r| r.starts_with(&path)) {
            rules.extend(around(&path, read));
        } else if path.is_dir() {
            rules.push(format!("{}/**", path.display()));
        } else {
            rules.push(path.display().to_string());
        }
    }
    rules
}

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
    // What the guard keeps off the forge besides the home folder, as the forge port does.
    let folders = guard
        .folders
        .iter()
        .map(|p| shell_quote(&format!("{FOLDER_FLAG}{}", p.display())));
    let names = guard
        .private_names
        .iter()
        .map(|n| shell_quote(&format!("{NAME_FLAG}{n}")));
    let commands: Vec<String> = commands.into_iter().chain(folders).chain(names).collect();
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
    use crate::trim::deny_with_trim;

    #[test]
    fn a_denied_folder_holding_a_readable_one_is_denied_around_it() {
        let dir = tempfile::tempdir().unwrap();
        let shep = dir.path();
        let own = shep.join("kelpie/koji/worktrees/7");
        for folder in ["run", "kelpie/rotom", "kelpie/koji/worktrees/8"] {
            std::fs::create_dir_all(shep.join(folder)).unwrap();
        }
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(shep.join("dogs.toml"), "").unwrap();
        let no_read = [format!("{}/**", shep.display()), "~/.ssh/**".to_owned()];

        let rules = read_denials(&no_read, &[own]);

        let at = |p: &str| shep.join(p).display().to_string();
        let expected = [
            at("dogs.toml"),
            format!("{}/**", at("kelpie/koji/worktrees/8")),
            format!("{}/**", at("kelpie/rotom")),
            format!("{}/**", at("run")),
            "~/.ssh/**".to_owned(),
        ];
        assert_eq!(rules, expected);
    }

    #[test]
    fn a_review_round_keeps_its_read_tools_and_reads_its_folders() {
        let bare = settings(Tools::Review, &Reach::default());
        assert_eq!(bare["permissions"]["deny"], deny_with_trim(&REVIEW_DENY));
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
        assert_eq!(none["permissions"]["deny"], deny_with_trim(&NO_TOOLS));
        let shot = Reach {
            read: vec![PathBuf::from("/k/shots/7")],
            fence: None,
        };
        let deny = settings(Tools::Answer, &shot)["permissions"]["deny"].clone();
        let without_read: Vec<&str> = NO_TOOLS.into_iter().filter(|t| *t != "Read").collect();
        assert_eq!(deny, deny_with_trim(&without_read));
    }

    #[test]
    fn every_role_drops_the_features_it_never_uses() {
        // A worker's fenced settings are pinned in `profile`'s tests.
        for tools in [Tools::Work, Tools::Review, Tools::Answer] {
            let s = settings(tools, &Reach::default());
            for key in [
                "disableBundledSkills",
                "disableWorkflows",
                "disableClaudeAiConnectors",
                "disableArtifact",
            ] {
                assert_eq!(s[key], true, "{tools:?} {key}");
            }
            assert!(s.get("disableRemoteControl").is_none(), "{tools:?}");
            assert_eq!(s["enableArtifact"], false, "{tools:?}");
            let own: &[&str] = match tools {
                Tools::Work => &WORK_DENY,
                Tools::Review => &REVIEW_DENY,
                Tools::Answer => &NO_TOOLS,
            };
            assert_eq!(s["permissions"]["deny"], deny_with_trim(own), "{tools:?}");
            // What kelpie's own calls use stays, unless the role denies it itself.
            let deny = s["permissions"]["deny"].to_string();
            for kept in ["SendMessage", "Agent", "Skill", "ToolSearch", "TaskCreate"] {
                if !own.contains(&kept) {
                    assert!(!deny.contains(&format!("\"{kept}\"")), "{tools:?} {kept}");
                }
            }
        }
    }
}
