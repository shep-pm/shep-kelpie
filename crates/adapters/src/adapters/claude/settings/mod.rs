//! Claude Code's settings file, built from a call's tools and reach
//!
//! Every call runs inside kelpie's sandbox, which holds the whole process.
//! These settings are the second layer: deny rules, and for a fenced session
//! a hook that runs `kelpie confine` on the file tools, `kelpie guard` on
//! every command, then any hook the project adds.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::guard::{AFTER_FLAG, FOLDER_FLAG, PAUSE_FLAG};
use crate::hooks::GUARD_BUDGET;
use crate::ports::{AgentCall, Fence, Guard, PM_NOTES, Reach, Tools};
use crate::profile::GH_TOKEN_VAR;
use crate::settings::{HookEvent, HookPoint};
use crate::spawn::shell_quote;
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

/// What the project manager's file tools never read, named outright: gh's
/// token, Claude Code's own files and the shell's history
const SECRETS_NO_READ: [&str; 6] = [
    "~/.config/gh/**",
    "~/.claude.json",
    "~/.claude/**",
    "~/.zsh_history",
    "~/.bash_history",
    "~/.zsh_sessions/**",
];

/// The tools a batch retro never uses: commands, sub-agents, every file
/// writer, the web, and the rest a worker is denied. It reads its folders
/// and replies with the report, which kelpie saves.
const RETRO_DENY: [&str; 14] = [
    "Agent",
    "Task",
    "Bash",
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

/// The only tools a worker's session starts with, as `--tools` takes them
///
/// `Skill` stays because a vendored skill calls another (`tdd` calls
/// `codebase-design`). With the five `disable*` settings, a trivial call's
/// context is 8,265 tokens, from 18,385.
pub(crate) const WORK_TOOLS: &str = "Bash,Read,Edit,Write,Glob,Grep,Skill";

/// The only tools the project manager's session has, as `--tools` takes them
pub(crate) const PM_TOOLS: &str = "Read,Glob,Grep,Edit,Write";

/// The only tools a batch retro has, as `--tools` takes them: it reads its
/// folders and calls the skills the retro skill names
pub(crate) const RETRO_TOOLS: &str = "Read,Glob,Grep,Skill";

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

// Seconds past the guard's budget for hooks that Claude Code waits for the guard.
const GUARD_MARGIN: u64 = 10;

/// The Read rules that keep a call with no fence out of `unfenced` but for its
/// own folder, the folders it is granted, and the plugins it is given
pub(crate) fn unfenced_reads(call: &AgentCall, unfenced: &[String]) -> Vec<String> {
    if call.reach.fence.is_some() {
        return Vec::new();
    }
    let own: Vec<PathBuf> = (std::iter::once(call.cwd.clone()))
        .chain(call.reach.read.iter().cloned())
        .chain(call.plugin_dirs.iter().cloned())
        .collect();
    read_denials(unfenced, &own)
        .iter()
        .map(|p| read_rule(p))
        .collect()
}

/// The settings file's contents for a call with these tools and sandbox
pub fn settings(tools: Tools, reach: &Reach) -> Value {
    let mut deny: Vec<String> = Vec::new();
    if let Some(fence) = &reach.fence {
        // What the call reads back inside a folder its fence denies: the fence's
        // own carve-outs, and every folder the call is granted.
        let carve: Vec<PathBuf> = (fence.read.iter())
            .chain(reach.read.iter())
            .cloned()
            .collect();
        deny.extend(
            read_denials(&fence.no_read, &carve)
                .iter()
                .map(|p| read_rule(p)),
        );
        deny.extend(fence.no_commands.iter().map(|c| format!("Bash({c})")));
    }
    deny.extend(tool_denies(tools, reach).map(str::to_owned));
    if let (Tools::Pm, Some(fence)) = (tools, &reach.fence) {
        deny.extend(reads_only(&fence.guard.worktree, &reach.read));
    }
    // Headless, a tool nothing allows is refused, and the retro skill calls another.
    let allow = (tools == Tools::Retro).then(|| json!(["Skill"]));
    let Some(fence) = &reach.fence else {
        let mut permissions = json!({ "deny": deny });
        if let Some(allow) = allow {
            permissions["allow"] = allow;
        }
        if !reach.read.is_empty() {
            // Read outside the working folder is refused under `-p` unless the folder is added.
            permissions["additionalDirectories"] = json!(reach.read);
        }
        return trimmed(json!({ "permissions": permissions }));
    };
    let mut permissions = json!({ "deny": deny });
    if let Some(allow) = allow {
        permissions["allow"] = allow;
    }
    if !reach.read.is_empty() {
        permissions["additionalDirectories"] = json!(reach.read);
    }
    let mut hooks = match tools {
        // Headless, an edit nothing allows is refused, so these are its only writes.
        Tools::Pm => {
            let notes = fence.guard.worktree.join(PM_NOTES).display().to_string();
            // Claude Code matches a file rule against the Edit tool alone and applies it to
            // `Write` too, and warns about a `Write(path)` rule.
            permissions["allow"] = json!([rule("Edit", &notes)]);
            pm_hooks(fence)
        }
        // Its session asks no one, so a write nothing allows is refused. Only a reviewer
        // given a folder to write has one, and `confine` holds its tools to that folder.
        Tools::Review if !fence.write.is_empty() => {
            let folders = (fence.write.iter())
                .map(|p| rule("Write", &format!("{}/**", p.display())))
                .collect::<Vec<_>>();
            permissions["allow"] = json!(folders);
            hooks(fence)
        }
        Tools::Work | Tools::Review | Tools::Answer | Tools::Retro => hooks(fence),
    };
    if tools == Tools::Work {
        pausing(&mut hooks, &fence.guard);
    }
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

/// Where the hook that hands the prompt back leaves one mark for each
/// compaction, in the call's `scratch` folder
pub(crate) fn compactions_file(scratch: &Path) -> PathBuf {
    scratch.join("compactions")
}

/// Has Claude Code compact the session at `window` tokens of context, and
/// print `prompt_file` into it after it does, leaving a mark in `marks` first
///
/// A compaction in the middle of a turn summarizes the turn's own prompt, and
/// early instructions may not survive the summary. Kelpie reads the reply's
/// `thread <n>: fixed` lines when the turn ends, so the prompt comes back
/// through a `SessionStart` hook that fires for a compaction alone.
pub(crate) fn compacting(
    settings: &mut Value,
    window: NonZeroU32,
    prompt_file: &Path,
    marks: &Path,
) {
    settings["autoCompactWindow"] = window.get().into();
    let command = format!(
        "printf . >> {}; cat {}",
        shell_quote(&marks.to_string_lossy()),
        shell_quote(&prompt_file.to_string_lossy())
    );
    let start = &mut settings["hooks"]["SessionStart"];
    let mut entries = start.as_array().cloned().unwrap_or_default();
    entries.push(entry(Some("compact"), &command));
    *start = entries.into();
}

// The project manager's file tools read its own folder and `also`, the
// worktrees and the checkout, and nothing else: every other entry from `/`
// down is denied, and the secrets named outright, wherever its folder is.
fn reads_only(checkout: &Path, also: &[PathBuf]) -> Vec<String> {
    // Each as given and with its links resolved, since `around` walks a link
    // such as macOS's `/var` as a folder of its own.
    let mut allowed: Vec<PathBuf> = Vec::new();
    for path in std::iter::once(checkout).chain(also.iter().map(PathBuf::as_path)) {
        allowed.push(path.to_owned());
        allowed.extend(crate::canonical(path).ok().filter(|c| c != path));
    }
    let outside = around(Path::new("/"), &allowed);
    (SECRETS_NO_READ.iter().map(|p| (*p).to_owned()))
        .chain(outside)
        .map(|p| read_rule(&p))
        .collect()
}

fn tool_denies(tools: Tools, reach: &Reach) -> impl Iterator<Item = &'static str> {
    let denied: &'static [&'static str] = match tools {
        Tools::Work => &WORK_DENY,
        Tools::Review => &REVIEW_DENY,
        Tools::Answer => &NO_TOOLS,
        Tools::Pm => &PM_DENY,
        Tools::Retro => &RETRO_DENY,
    };
    // An answer that may read a folder keeps Read and nothing else.
    let reads = tools == Tools::Answer && !reach.read.is_empty();
    denied
        .iter()
        .copied()
        .filter(move |t| !(reads && *t == "Read"))
}

// `//` roots a rule at `/`: a single `/` is taken from the settings file. A
// folder holding a readable path is denied entry by entry around it. Every deny
// is named as given and with its links resolved, as Claude Code matches the
// path its tool names.
fn read_denials(no_read: &[String], read: &[PathBuf]) -> Vec<String> {
    let mut spelled: Vec<PathBuf> = Vec::new();
    for path in read {
        spelled.push(path.clone());
        spelled.extend(crate::canonical(path).ok().filter(|c| c != path));
    }
    let holds_one = |dir: &Path| spelled.iter().any(|r| r.starts_with(dir));
    no_read
        .iter()
        .flat_map(|rule| {
            let (path, folder) = match rule.strip_suffix("/**") {
                Some(dir) => (Path::new(dir), true),
                None => (Path::new(rule.as_str()), false),
            };
            let resolved = crate::canonical(path).ok().filter(|c| c != path);
            let holds = |dir: &Path| folder && holds_one(dir);
            if holds(path) || resolved.as_deref().is_some_and(holds) {
                return around_in_both(path, &spelled);
            }
            let mut rules = vec![rule.clone()];
            if let Some(resolved) = resolved {
                let suffix = if folder { "/**" } else { "" };
                rules.push(format!("{}{suffix}", resolved.display()));
            }
            rules
        })
        .collect()
}

// `around` for `dir` as given and where its links lead, each entry given
// first and then resolved where that differs.
fn around_in_both(dir: &Path, read: &[PathBuf]) -> Vec<String> {
    let Some(resolved) = crate::canonical(dir).ok().filter(|c| c != dir) else {
        return around(dir, read);
    };
    // A read path spelled one way under the folder is also named the other.
    let mut read = read.to_vec();
    for path in read.clone() {
        for (from, to) in [(dir, resolved.as_path()), (resolved.as_path(), dir)] {
            read.extend(path.strip_prefix(from).ok().map(|rest| to.join(rest)));
        }
    }
    let mut rules: Vec<String> = Vec::new();
    let mut add = |rule: Option<String>| {
        if let Some(rule) = rule.filter(|r| !rules.contains(r)) {
            rules.push(rule);
        }
    };
    for rule in around(dir, &read) {
        add(Some(rule.clone()));
        add(respelled(&rule, dir, &resolved));
    }
    for rule in around(&resolved, &read) {
        add(respelled(&rule, &resolved, dir));
        add(Some(rule));
    }
    rules
}

// `rule` with the folder `from` it starts with named `to`.
fn respelled(rule: &str, from: &Path, to: &Path) -> Option<String> {
    let rest = rule.strip_prefix(from.to_str()?)?;
    Some(format!("{}{rest}", to.display()))
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

// The project's own variables come last, so one it names wins. The maintainer's
// login is not here: this file stays on disk, and the command carries it.
fn env(fence: &Fence) -> Value {
    let mut env = json!({});
    for (name, value) in fence.env.iter().filter(|(name, _)| *name != GH_TOKEN_VAR) {
        env[name.as_str()] = json!(value);
    }
    env
}

fn hooks(fence: &Fence) -> Value {
    let guard = &fence.guard;
    let kelpie = guard.kelpie.as_path();
    let mut confine = [kelpie, Path::new("confine"), &guard.worktree, &guard.build]
        .map(|p| shell_quote(&p.to_string_lossy()))
        .join(" ");
    // The project's `write` hooks, which `confine` runs on each file a call writes.
    let writes = guard.hooks.has(HookPoint::Write);
    if let Some(flag) = guard.hooks.flag().filter(|_| writes) {
        confine = format!("{confine} {}", shell_quote(&flag));
    }
    let mut confined = entry(Some(FILE_TOOLS), &confine);
    if writes {
        confined["hooks"][0]["timeout"] = (GUARD_BUDGET.as_secs() + GUARD_MARGIN).into();
    }
    let mut pre = vec![confined];
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
    // The project's own hooks, which the guard runs on a commit, a push and a post.
    let hooks = guard.hooks.flag().map(|flag| shell_quote(&flag));
    let base = guard.base_flag().map(|flag| shell_quote(&flag));
    let pushes = guard.kelpie_pushes_flag().map(shell_quote);
    let commands: Vec<String> = (commands.into_iter())
        .chain(folders)
        .chain(hooks)
        .chain(base)
        .chain(pushes)
        .collect();
    let mut guarded = entry(Some(GUARDED_TOOLS), &commands.join(" "));
    // Claude Code lets a call through when its hook outlasts the hook's
    // timeout, so the project's hooks get a timeout past the guard's own budget
    // for them, which refuses the call first.
    if !guard.hooks.is_empty() {
        guarded["hooks"][0]["timeout"] = (GUARD_BUDGET.as_secs() + GUARD_MARGIN).into();
    }
    pre.push(guarded);
    let mut post = Vec::new();
    // The project's `after_command` hooks, run on a command's result. Claude Code
    // fires one event for a command that succeeded and another for one that failed.
    let mut failed = Vec::new();
    if guard.hooks.has(HookPoint::AfterCommand) {
        let after = format!("{} {AFTER_FLAG}", commands.join(" "));
        let mut e = entry(Some("Bash"), &after);
        e["hooks"][0]["timeout"] = (GUARD_BUDGET.as_secs() + GUARD_MARGIN).into();
        post.push(e.clone());
        failed.push(e);
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
    if !failed.is_empty() {
        hooks["PostToolUseFailure"] = failed.into();
    }
    hooks
}

// The hook that ends a worker's turn at its next tool result while a pause
// is set, after a tool call that succeeded and after one that failed.
fn pausing(hooks: &mut Value, guard: &Guard) {
    let words = [
        guard.kelpie.to_string_lossy().into_owned(),
        "guard".to_owned(),
        guard.git_common_dir.to_string_lossy().into_owned(),
        guard.worktree.to_string_lossy().into_owned(),
        format!("{PAUSE_FLAG}{}", guard.build.display()),
    ];
    let command = words.map(|w| shell_quote(&w)).join(" ");
    for event in ["PostToolUse", "PostToolUseFailure"] {
        let mut entries = hooks[event].as_array().cloned().unwrap_or_default();
        entries.push(entry(None, &command));
        hooks[event] = entries.into();
    }
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

#[cfg(test)]
mod tests;
