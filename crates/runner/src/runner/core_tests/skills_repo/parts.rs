//! A repo plugin's agents, commands and hooks load beside its skills

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use serde_json::json;

use super::{call_of, directory, land_settings, marketplace};
use crate::confine::Verdict;
use crate::guard::{self, Checkout};
use crate::ports::Role;
use crate::runner::{Runner, step};
use crate::test::Rig;

const PLUGIN: &str = "markets/house/plugins/style";

// A runner on a repo that turns on `style@house`, a plugin with a skill, an agent, a command and
// a hook, with a worker's first turn run
fn with_parts() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    let dir = marketplace(&rig, "house", &[("style", "style")]);
    rig.land(
        &format!("{PLUGIN}/agents/checker.md"),
        "---\nname: checker\n---\nCheck.\n",
    );
    rig.land(&format!("{PLUGIN}/commands/lint.md"), "Lint it.\n");
    rig.land(
        &format!("{PLUGIN}/docs/rules.md"),
        "IR-47: one file per rule.\n",
    );
    let hooks = json!({ "hooks": { "PreToolUse": [{
        "matcher": "Bash",
        "hooks": [{
            "type": "command",
            "command": "sh \"${CLAUDE_PLUGIN_ROOT}/hooks/note.sh\"",
        }],
    }] } });
    rig.land(&format!("{PLUGIN}/hooks/hooks.json"), &hooks.to_string());
    rig.land(&format!("{PLUGIN}/hooks/note.sh"), "echo ran > \"$NOTE\"\n");
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", directory(&dir))],
    );
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    (rig, runner)
}

#[test]
fn a_plugins_agent_command_and_hook_reach_the_workers_plugin_copy() {
    let (rig, _runner) = with_parts();
    let plugin = rig.paths().skills.join("repo/style");
    assert!(call_of(&rig, Role::Worker).plugin_dirs.contains(&plugin));
    assert!(plugin.join("skills/style/SKILL.md").is_file());
    assert!(plugin.join("agents/checker.md").is_file());
    assert!(plugin.join("commands/lint.md").is_file());
    assert!(plugin.join("hooks/hooks.json").is_file());
}

// The commands of `hooks`, a `{ event: [{ matcher?, hooks: [{ command }] }] }` object, that Claude
// Code runs before a `Bash` call
fn bash_hooks(hooks: &serde_json::Value) -> Vec<String> {
    hooks["PreToolUse"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry["matcher"]
                .as_str()
                .is_none_or(|m| m.split('|').any(|tool| tool == "Bash"))
        })
        .flat_map(|entry| entry["hooks"].as_array().into_iter().flatten())
        .filter_map(|hook| hook["command"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn a_plugin_hook_runs_and_kelpies_guard_still_blocks_what_it_blocks() {
    let (rig, _runner) = with_parts();
    let plugin = rig.paths().skills.join("repo/style");
    let plugins: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(plugin.join("hooks/hooks.json")).unwrap())
            .unwrap();
    let worker = rig.claude.seen().into_iter().next().unwrap();
    let kelpies = bash_hooks(&worker.settings["hooks"]);
    let plugins = bash_hooks(&plugins["hooks"]);
    assert_eq!(plugins.len(), 1, "{plugins:?}");

    // Claude Code runs every hook that matches a tool call, and the call goes ahead only if none
    // refuses. Each is run here for a merge, which only the project manager does.
    let cwd = &worker.call.cwd;
    let merge = json!({
        "tool_name": "Bash",
        "cwd": cwd,
        "tool_input": { "command": "gh pr merge 5" },
    })
    .to_string();
    let note = rig.home.path().join("note");
    let ran = Command::new(crate::spawn::sh())
        .args(["-c", &plugins[0]])
        .env("CLAUDE_PLUGIN_ROOT", &plugin)
        .env("NOTE", &note)
        .status()
        .unwrap();
    assert!(ran.success());
    assert_eq!(fs::read_to_string(note).unwrap(), "ran\n");

    let guards: Vec<_> = kelpies.iter().filter(|c| c.contains("'guard'")).collect();
    assert_eq!(guards.len(), 1, "{kelpies:?}");
    // `'kelpie' 'guard' '<git dir>' '<worktree>' '--folder=<folder>'...`
    let words: Vec<String> = guards[0]
        .trim_matches('\'')
        .split("' '")
        .map(str::to_owned)
        .collect();
    let (git_dir, worktree) = (Path::new(&words[2]), Path::new(&words[3]));
    let checkout = || Checkout {
        git_common_dir: git_dir,
        worktree,
        base: "main",
        kelpie_pushes: false,
    };
    let local = || guard::local_paths(None, &words[4..]).unwrap();
    let verdict = guard::judge(merge.as_bytes(), None, local(), checkout());
    assert!(matches!(verdict, Verdict::Refuse(_)), "{verdict:?}");
    let harmless = merge.replace("gh pr merge 5", "echo hi");
    let verdict = guard::judge(harmless.as_bytes(), None, local(), checkout());
    assert_eq!(verdict, Verdict::Allow);

    // The plugin's hook is in its own folder, never in kelpie's settings file.
    assert!(!worker.settings.to_string().contains("note.sh"));
}
