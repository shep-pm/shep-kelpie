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
        allowed.extend(path.canonicalize().ok().filter(|c| c != path));
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

// `//` roots a rule at `/`: a single `/` is taken from the settings file.
// Deny rules take no exceptions, so a folder holding a readable one is denied
// entry by entry around it, as given and with its links resolved, since Claude
// Code matches the path its tool names. The sandbox still denies the folder whole.
fn read_denials(no_read: &[String], read: &[PathBuf]) -> Vec<String> {
    let mut spelled: Vec<PathBuf> = Vec::new();
    for path in read {
        spelled.push(path.clone());
        spelled.extend(path.canonicalize().ok().filter(|c| c != path));
    }
    let holds_one = |dir: &Path| spelled.iter().any(|r| r.starts_with(dir));
    no_read
        .iter()
        .flat_map(|rule| match rule.strip_suffix("/**").map(Path::new) {
            Some(dir) if holds_one(dir) => around_in_both(dir, &spelled),
            _ => vec![rule.clone()],
        })
        .collect()
}

// `around` for `dir` as given and where its links lead, each entry given
// first and then resolved where that differs.
fn around_in_both(dir: &Path, read: &[PathBuf]) -> Vec<String> {
    let Some(resolved) = dir.canonicalize().ok().filter(|c| c != dir) else {
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
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::hooks::Hooks;
    use crate::ports::{Fence, Guard};
    use crate::trim::deny_with_trim;

    #[test]
    fn a_compaction_hook_leaves_a_mark_in_the_scratch_folder_and_prints_the_prompt_back() {
        let dir = tempfile::tempdir().unwrap();
        let prompt = dir.path().join("settings.prompt.md");
        std::fs::write(&prompt, "do the thing\n").unwrap();
        let marks = compactions_file(&dir.path().join("settings.tmp"));
        std::fs::create_dir_all(marks.parent().unwrap()).unwrap();
        let mut value = json!({});
        compacting(
            &mut value,
            NonZeroU32::new(200_000).unwrap(),
            &prompt,
            &marks,
        );
        let hook = &value["hooks"]["SessionStart"][0];
        assert_eq!(hook["matcher"], "compact");
        let command = hook["hooks"][0]["command"].as_str().unwrap();
        for _ in 0..2 {
            let out = std::process::Command::new(crate::spawn::sh())
                .args(["-c", command])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), "do the thing\n");
        }
        assert_eq!(std::fs::read_to_string(&marks).unwrap(), "..");
    }

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

        let at = |p: &str| spellings(&shep.join(p));
        let folder = |p: &str| at(p).into_iter().map(|s| format!("{s}/**"));
        let expected: Vec<String> = at("dogs.toml")
            .into_iter()
            .chain(folder("kelpie/koji/worktrees/8"))
            .chain(folder("kelpie/rotom"))
            .chain(folder("run"))
            .chain(["~/.ssh/**".to_owned()])
            .collect();
        assert_eq!(rules, expected);
    }

    // `path` as given, then with its links resolved where that differs.
    fn spellings(path: &Path) -> Vec<String> {
        let resolved = path.canonicalize().ok().filter(|c| c != path);
        std::iter::once(path.to_owned())
            .chain(resolved)
            .map(|p| p.display().to_string())
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn a_denied_home_reached_through_a_link_is_denied_in_both_spellings() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        for folder in ["toolchain/bin", "other"] {
            std::fs::create_dir_all(real.join(folder)).unwrap();
        }
        std::fs::write(real.join("credentials"), "").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let no_read = [format!("{}/**", link.display())];

        let rules = read_denials(&no_read, &[link.join("toolchain")]);

        let folder = |p: &str| {
            spellings(&link.join(p))
                .into_iter()
                .map(|s| format!("{s}/**"))
        };
        let expected: Vec<String> = spellings(&link.join("credentials"))
            .into_iter()
            .chain(folder("other"))
            .collect();
        assert_eq!(rules, expected);
        assert_ne!(
            link.canonicalize().unwrap(),
            link,
            "the denied home is reached through a link"
        );
        assert!(rules.iter().all(|r| !r.contains("toolchain")));
    }

    #[cfg(unix)]
    #[test]
    fn a_readable_folder_spelled_through_the_link_or_not_is_denied_in_neither() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        for folder in ["a", "b", "c"] {
            std::fs::create_dir_all(real.join(folder)).unwrap();
        }
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let no_read = [format!("{}/**", link.display())];

        let rules = read_denials(&no_read, &[link.join("a"), real.join("b")]);

        let expected: Vec<String> = spellings(&link.join("c"))
            .into_iter()
            .map(|s| format!("{s}/**"))
            .collect();
        assert_eq!(rules, expected);
    }

    #[test]
    fn a_review_round_keeps_its_read_tools_and_reads_its_folders() {
        let bare = settings(Tools::Review, &Reach::default());
        assert_eq!(bare["permissions"]["deny"], deny_with_trim(&REVIEW_DENY));
        let extra = Reach {
            read: vec![PathBuf::from("/k/extra/7")],
            fence: None,
            ..Reach::default()
        };
        assert_eq!(
            settings(Tools::Review, &extra)["permissions"]["additionalDirectories"],
            json!(["/k/extra/7"])
        );
    }

    #[test]
    fn a_retro_may_call_a_skill_with_or_without_a_fence() {
        let fenced = Reach {
            read: Vec::new(),
            fence: Some(Box::new(Fence {
                write: Vec::new(),
                no_write: Vec::new(),
                no_read: Vec::new(),
                read: Vec::new(),
                hosts: Vec::new(),
                no_commands: Vec::new(),
                env: Default::default(),
                sockets: Vec::new(),
                guard: Guard {
                    kelpie: "/k/kelpie".into(),
                    worktree: "/k/wt".into(),
                    build: "/k/build".into(),
                    git_common_dir: "/k/git".into(),
                    base: "main".into(),
                    kelpie_pushes: false,
                    folders: Vec::new(),
                    hooks: Hooks::default(),
                },
                hooks: Vec::new(),
            })),
            ..Reach::default()
        };
        for reach in [Reach::default(), fenced] {
            let s = settings(Tools::Retro, &reach);
            assert_eq!(s["permissions"]["allow"], json!(["Skill"]));
        }
        let review = settings(Tools::Review, &Reach::default());
        assert!(review["permissions"].get("allow").is_none());
    }

    // Claude Code lets a call through when its guard outlasts the hook's
    // timeout, so with the project's hooks the guard's entry waits longer than
    // the guard spends on them, and without them it keeps the default.
    #[test]
    fn the_guard_waits_longer_than_it_spends_on_the_projects_hooks() {
        use crate::settings::HookPoint;
        let reach = reach_with;
        let guard_entry = |reach: &Reach| {
            let pre = settings(Tools::Work, reach)["hooks"]["PreToolUse"].clone();
            let bash = pre
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["matcher"] == GUARDED_TOOLS);
            bash.unwrap()["hooks"][0].clone()
        };
        let with = guard_entry(&reach(hooks_at(&[HookPoint::Commit])));
        assert_eq!(with["timeout"], GUARD_BUDGET.as_secs() + GUARD_MARGIN);
        assert!(
            guard_entry(&reach(Hooks::default()))
                .get("timeout")
                .is_none()
        );
    }

    #[test]
    fn the_guard_is_told_the_base_branch_only_when_it_is_not_the_default() {
        let guard_command = |base: &str| {
            let mut reach = reach_with(Hooks::default());
            reach.fence.as_mut().unwrap().guard.base = base.to_owned();
            let pre = settings(Tools::Work, &reach)["hooks"]["PreToolUse"].clone();
            let pre = pre.as_array().unwrap();
            let bash = pre.iter().find(|e| e["matcher"] == GUARDED_TOOLS).unwrap();
            bash["hooks"][0]["command"].as_str().unwrap().to_owned()
        };
        assert!(guard_command("v2").ends_with(" '--base=v2'"));
        assert!(!guard_command("main").contains("--base"));
    }

    // A fenced worker's reach with `hooks` as the project's own.
    fn reach_with(hooks: Hooks) -> Reach {
        Reach {
            read: Vec::new(),
            fence: Some(Box::new(Fence {
                write: Vec::new(),
                no_write: Vec::new(),
                no_read: Vec::new(),
                read: Vec::new(),
                hosts: Vec::new(),
                no_commands: Vec::new(),
                env: Default::default(),
                sockets: Vec::new(),
                guard: Guard {
                    kelpie: "/k/kelpie".into(),
                    worktree: "/k/wt".into(),
                    build: "/k/build".into(),
                    git_common_dir: "/k/git".into(),
                    base: "main".into(),
                    kelpie_pushes: false,
                    folders: Vec::new(),
                    hooks,
                },
                hooks: Vec::new(),
            })),
            ..Reach::default()
        }
    }

    fn hooks_at(points: &[crate::settings::HookPoint]) -> Hooks {
        use crate::settings::{NonBlank, ProjectHook};
        let list: Vec<ProjectHook> = points
            .iter()
            .map(|&point| ProjectHook {
                point,
                command: NonBlank::try_from("exit 0".to_owned()).unwrap(),
            })
            .collect();
        Hooks::new("koji", "acme/koji", Path::new("/"), &list)
    }

    fn commands_of(entries: &Value, matcher: &str) -> Vec<String> {
        let entries = entries.as_array().cloned().unwrap_or_default();
        (entries.iter())
            .filter(|e| e["matcher"] == matcher)
            .map(|e| e["hooks"][0]["command"].as_str().unwrap().to_owned())
            .collect()
    }

    // Claude Code runs the project's `write` hooks through `confine`, and its
    // `after_command` ones through a second guard entry on each command's result,
    // a success and a failure alike, since it fires a different event for each.
    #[test]
    fn the_projects_write_and_after_command_hooks_ride_on_confine_and_the_guard() {
        use crate::settings::HookPoint;
        let hooks = hooks_at(&[
            HookPoint::Write,
            HookPoint::AfterCommand,
            HookPoint::Command,
        ]);
        let flag = shell_quote(&hooks.flag().unwrap());

        let s = settings(Tools::Work, &reach_with(hooks));

        let confine = commands_of(&s["hooks"]["PreToolUse"], FILE_TOOLS);
        assert_eq!(confine.len(), 1);
        assert!(confine[0].ends_with(&flag), "{}", confine[0]);
        for event in ["PostToolUse", "PostToolUseFailure"] {
            let after = commands_of(&s["hooks"][event], "Bash");
            assert_eq!(after.len(), 1, "{event}");
            assert!(after[0].contains("guard"), "{event}: {}", after[0]);
            assert!(after[0].contains(&flag), "{event}: {}", after[0]);
            assert!(after[0].ends_with(" --after"), "{event}: {}", after[0]);
            let entry = &s["hooks"][event][0]["hooks"][0];
            assert_eq!(entry["timeout"], GUARD_BUDGET.as_secs() + GUARD_MARGIN);
        }
        let pre = &s["hooks"]["PreToolUse"][0]["hooks"][0];
        assert_eq!(pre["timeout"], GUARD_BUDGET.as_secs() + GUARD_MARGIN);
    }

    #[test]
    fn no_write_or_after_command_hook_adds_no_flag_to_confine_and_no_post_entry() {
        use crate::settings::HookPoint;
        let s = settings(Tools::Work, &reach_with(hooks_at(&[HookPoint::Commit])));

        let confine = commands_of(&s["hooks"]["PreToolUse"], FILE_TOOLS);
        assert!(!confine[0].contains("--hooks="), "{}", confine[0]);
        // The only post entry is the pause hook, which has no matcher.
        for event in ["PostToolUse", "PostToolUseFailure"] {
            assert!(
                commands_of(&s["hooks"][event], "Bash").is_empty(),
                "{event}"
            );
            assert_eq!(s["hooks"][event].as_array().unwrap().len(), 1, "{event}");
        }
    }

    #[test]
    fn an_answer_reads_nothing_unless_its_sandbox_lists_a_folder() {
        let none = settings(Tools::Answer, &Reach::default());
        assert_eq!(none["permissions"]["deny"], deny_with_trim(&NO_TOOLS));
        let extra = Reach {
            read: vec![PathBuf::from("/k/extra/7")],
            fence: None,
            ..Reach::default()
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
            // It may edit its notes file only: `Edit(<notes>)` allows it and
            // `confine --append` holds it to appending (`Write` rides the `Edit` rule).
            // `MultiEdit` has no deny rule: Claude Code warns it matches no known
            // tool, and the `--tools` allow-list already leaves it out.
            Tools::Pm => &["Edit", "Write", "MultiEdit"],
            // Known gap: a reviewer's session is read-only by design (Read, Grep and Glob),
            // but its own deny list holds `Agent`, `Task` and `Bash` only. Every settings
            // file denies `NotebookEdit` through `trimmed`, so that one is asserted. Kelpie's
            // sandbox holds the writes of the other three, so no deny is asserted for them yet.
            Tools::Review => &["Edit", "Write", "MultiEdit"],
            Tools::Answer | Tools::Retro => &[],
        }
    }

    // Every role, walked by a match with no catch-all: a new `Tools` variant fails to
    // compile here until it is chained in, so the test below cannot skip it.
    fn next_role(tools: Tools) -> Option<Tools> {
        match tools {
            Tools::Work => Some(Tools::Review),
            Tools::Review => Some(Tools::Answer),
            Tools::Answer => Some(Tools::Pm),
            Tools::Pm => Some(Tools::Retro),
            Tools::Retro => None,
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
            Tools::Pm,
            Tools::Retro,
        ] {
            let s = settings(tools, &Reach::default());
            for key in [
                "disableBundledSkills",
                "disableWorkflows",
                "disableClaudeAiConnectors",
                "disableArtifact",
                "disableRemoteControl",
            ] {
                assert_eq!(s[key], true, "{tools:?} {key}");
            }
            assert_eq!(s["enableArtifact"], false, "{tools:?}");
            let own: &[&str] = match tools {
                Tools::Work => &WORK_DENY,
                Tools::Review => &REVIEW_DENY,
                Tools::Answer => &NO_TOOLS,
                Tools::Pm => &PM_DENY,
                Tools::Retro => &RETRO_DENY,
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
