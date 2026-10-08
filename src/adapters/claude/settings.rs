//! Claude Code's settings file, built from a call's tools and reach
//!
//! Every call runs inside kelpie's sandbox, which holds the whole process.
//! These settings are the second layer: deny rules, and for a fenced session
//! a hook that runs `kelpie confine` on the file tools, `kelpie guard` on
//! every command, then any hook the project adds.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::guard::{FOLDER_FLAG, NAME_FLAG, RECORD_FLAG};
use crate::ports::{Fence, PM_NOTES, Reach, Tools};
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

/// What the issue writer's file tools never read, named outright: gh's
/// token, Claude Code's own files and the shell's history
const ISSUES_NO_READ: [&str; 6] = [
    "~/.config/gh/**",
    "~/.claude.json",
    "~/.claude/**",
    "~/.zsh_history",
    "~/.bash_history",
    "~/.zsh_sessions/**",
];

/// The tools the issue writer never uses: sub-agents, the file writers, the
/// web, and the rest a worker is denied. Its commands are its guard's.
const ISSUES_DENY: [&str; 13] = [
    "Agent",
    "Task",
    "Edit",
    "Write",
    "MultiEdit",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
    "Monitor",
    "RemoteTrigger",
    "Workflow",
    "EnterWorktree",
    "ExitWorktree",
];

/// The only tools the project manager's session has, as `--tools` takes them
pub(crate) const PM_TOOLS: &str = "Read,Glob,Grep,Edit,Write";

/// What the project manager never uses, denied as well in case `--tools`
/// lets one through: commands, sub-agents, skills, messages, the web, and
/// the file tool whose writes the append check cannot read
const PM_DENY: [&str; 10] = [
    "Agent",
    "Task",
    "Bash",
    "BashOutput",
    "KillShell",
    "Skill",
    "SendMessage",
    "WebFetch",
    "WebSearch",
    "NotebookEdit",
];

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

/// The Read rules that keep a call with no fence, working in `cwd`, out of
/// `unfenced` but for `cwd` and the folders it reads
pub(crate) fn unfenced_reads(cwd: &Path, reach: &Reach, unfenced: &[String]) -> Vec<String> {
    if reach.fence.is_some() {
        return Vec::new();
    }
    let own: Vec<PathBuf> = std::iter::once(cwd.to_owned())
        .chain(reach.read.iter().cloned())
        .collect();
    read_denials(unfenced, &own)
        .iter()
        .map(|p| read_rule(p))
        .collect()
}

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
    if let (Tools::Issues | Tools::Pm, Some(fence)) = (tools, &reach.fence) {
        deny.extend(reads_only(&fence.guard.worktree, &reach.read));
    }
    let Some(fence) = &reach.fence else {
        let mut permissions = json!({ "deny": deny });
        if !reach.read.is_empty() {
            // Read outside the working folder is refused under `-p` unless the folder is added.
            permissions["additionalDirectories"] = json!(reach.read);
        }
        return trimmed(json!({ "permissions": permissions }));
    };
    let mut permissions = json!({ "deny": deny });
    if !reach.read.is_empty() {
        permissions["additionalDirectories"] = json!(reach.read);
    }
    let hooks = match tools {
        // Its session asks no one, so a command its guard allows must not wait on a prompt.
        Tools::Issues => {
            permissions["allow"] = json!(["Bash"]);
            hooks(fence)
        }
        // Headless, an edit nothing allows is refused, so these are its only writes.
        Tools::Pm => {
            let notes = fence.guard.worktree.join(PM_NOTES).display().to_string();
            // Claude Code matches a file rule against the Edit tool alone and applies it to
            // `Write` too, and warns about a `Write(path)` rule.
            permissions["allow"] = json!([rule("Edit", &notes)]);
            pm_hooks(fence)
        }
        Tools::Work | Tools::Review | Tools::Answer => hooks(fence),
    };
    trimmed(json!({
        // Kelpie's sandbox holds the whole process, and on macOS Claude Code's
        // own cannot start inside it: every command would be refused.
        "sandbox": { "enabled": false },
        "permissions": permissions,
        // A project's own settings could otherwise switch every hook off,
        // `confine` and the guard with them. This file outranks them.
        "disableAllHooks": false,
        "hooks": hooks,
        "env": env(fence),
    }))
}

// The issue writer's file tools read its checkout, and `also`, and nothing
// else: every other entry from `/` down is denied, and the secrets named
// outright, whichever folder its checkout is in. Its commands read no file.
// The project manager's read its own folder the same way.
fn reads_only(checkout: &Path, also: &[PathBuf]) -> Vec<String> {
    // Each as given and with its links resolved, since `around` walks a link
    // such as macOS's `/var` as a folder of its own.
    let mut allowed: Vec<PathBuf> = Vec::new();
    for path in std::iter::once(checkout).chain(also.iter().map(PathBuf::as_path)) {
        allowed.push(path.to_owned());
        allowed.extend(path.canonicalize().ok().filter(|c| c != path));
    }
    let outside = around(Path::new("/"), &allowed);
    (ISSUES_NO_READ.iter().map(|p| (*p).to_owned()))
        .chain(outside)
        .map(|p| read_rule(&p))
        .collect()
}

fn tool_denies(tools: Tools, reach: &Reach) -> impl Iterator<Item = &'static str> {
    let denied: &'static [&'static str] = match tools {
        Tools::Work => &WORK_DENY,
        Tools::Review => &REVIEW_DENY,
        Tools::Answer => &NO_TOOLS,
        Tools::Issues => &ISSUES_DENY,
        Tools::Pm => &PM_DENY,
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
    rule("Read", path)
}

// `tool`'s rule on `path`.
fn rule(tool: &str, path: &str) -> String {
    match path.starts_with('/') {
        true => format!("{tool}(/{path})"),
        false => format!("{tool}({path})"),
    }
}

// The project's own variables come last, so one it names wins.
fn env(fence: &Fence) -> Value {
    let mut env = json!({});
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
    let issues = (guard.issues.iter())
        .flat_map(|rules| rules.flags())
        .map(|flag| shell_quote(&flag));
    let commands: Vec<String> = (commands.into_iter())
        .chain(folders)
        .chain(names)
        .chain(issues)
        .collect();
    pre.push(entry(Some(GUARDED_TOOLS), &commands.join(" ")));
    let mut post = Vec::new();
    // The issue writer's ledger of what it filed and read, kept after each command.
    if guard.issues.is_some() {
        let record = format!("{} {RECORD_FLAG}", commands.join(" "));
        post.push(entry(Some("Bash"), &record));
    }
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

// The project manager's one hook: its file tools may only add to the end
// of its notes.
fn pm_hooks(fence: &Fence) -> Value {
    let notes = fence.guard.worktree.join(PM_NOTES);
    let command = [
        fence.guard.kelpie.as_path(),
        Path::new("confine"),
        Path::new(crate::confine::APPEND),
        &notes,
    ]
    .map(|p| shell_quote(&p.to_string_lossy()))
    .join(" ");
    json!({ "PreToolUse": [entry(Some(FILE_TOOLS), &command)] })
}

fn entry(matcher: Option<&str>, command: &str) -> Value {
    let mut entry = json!({ "hooks": [{ "type": "command", "command": command }] });
    if let Some(matcher) = matcher {
        entry["matcher"] = matcher.into();
    }
    entry
}

/// `s` as one word to a POSIX shell
pub(crate) fn shell_quote(s: &str) -> String {
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
        let extra = Reach {
            read: vec![PathBuf::from("/k/extra/7")],
            fence: None,
        };
        assert_eq!(
            settings(Tools::Review, &extra)["permissions"]["additionalDirectories"],
            json!(["/k/extra/7"])
        );
    }

    #[test]
    fn an_answer_reads_nothing_unless_its_sandbox_lists_a_folder() {
        let none = settings(Tools::Answer, &Reach::default());
        assert_eq!(none["permissions"]["deny"], deny_with_trim(&NO_TOOLS));
        let extra = Reach {
            read: vec![PathBuf::from("/k/extra/7")],
            fence: None,
        };
        let deny = settings(Tools::Answer, &extra)["permissions"]["deny"].clone();
        let without_read: Vec<&str> = NO_TOOLS.into_iter().filter(|t| *t != "Read").collect();
        assert_eq!(deny, deny_with_trim(&without_read));
    }

    // The file tools each role's settings may leave undenied, with the reason.
    // A match with no catch-all, so a new role cannot join without a decision.
    fn file_tools_left_open(tools: Tools) -> &'static [&'static str] {
        match tools {
            // It builds the change, so it writes files, held to its folders by `confine`.
            Tools::Work => &["Edit", "Write", "MultiEdit", "NotebookEdit"],
            // It may edit its notes file and nothing else, which `Edit(<notes>)` allows
            // and `confine --append` holds to appending (`Write` rides the `Edit` rule).
            // `MultiEdit` was left off its deny list on purpose in #357: Claude Code warns
            // that a rule for it matches no known tool, and its `--tools` allow-list
            // already leaves the tool out. `NotebookEdit` stays denied.
            Tools::Pm => &["Edit", "Write", "MultiEdit"],
            // Known gap: a reviewer's session is read-only by design (Read, Grep and Glob),
            // but its own deny list holds `Agent`, `Task` and `Bash` only. Every settings
            // file denies `NotebookEdit` through `trimmed`, so that one is asserted. Kelpie's
            // sandbox holds the writes of the other three, so no deny is asserted for them yet.
            Tools::Review => &["Edit", "Write", "MultiEdit"],
            Tools::Answer | Tools::Issues => &[],
        }
    }

    // Every role, walked by a match with no catch-all: a new `Tools` variant fails to
    // compile here until it is chained in, so the test below cannot skip it.
    fn next_role(tools: Tools) -> Option<Tools> {
        match tools {
            Tools::Work => Some(Tools::Review),
            Tools::Review => Some(Tools::Answer),
            Tools::Answer => Some(Tools::Issues),
            Tools::Issues => Some(Tools::Pm),
            Tools::Pm => None,
        }
    }

    #[test]
    fn every_role_denies_every_file_tool_it_does_not_leave_open() {
        let file_tools: Vec<&str> = FILE_TOOLS.split('|').collect();
        assert!(file_tools.contains(&"MultiEdit"), "{file_tools:?}");
        let mut role = Some(Tools::Work);
        while let Some(tools) = role {
            role = next_role(tools);
            let s = settings(tools, &Reach::default());
            let denied: Vec<&str> = s["permissions"]["deny"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let open = file_tools_left_open(tools);
            for tool in &file_tools {
                if !open.contains(tool) {
                    assert!(denied.contains(tool), "{tools:?} does not deny {tool}");
                }
            }
        }
    }

    #[test]
    fn every_role_drops_the_features_it_never_uses() {
        // A worker's fenced settings are pinned in `profile`'s tests.
        for tools in [
            Tools::Work,
            Tools::Review,
            Tools::Answer,
            Tools::Issues,
            Tools::Pm,
        ] {
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
                Tools::Issues => &ISSUES_DENY,
                Tools::Pm => &PM_DENY,
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
