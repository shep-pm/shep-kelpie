//! `[app.dogs.kelpie.repo_setup]` narrows the MCP servers of the repo's plugins and of its
//! `.mcp.json` on the default branch, on the worker's and the reviewers' calls and the project
//! manager's retro

use std::fs;

use serde_json::json;

use crate::ports::{Role, Tools};
use crate::runner::{CHECKS_SETTLE, StepReport, step};
use crate::state::{Finished, ProjectState, StateStore};
use crate::test::{Rig, Scripted, Seen, ticket};
use crate::work_item::Seconds;

// Lands the plugin `style` of the marketplace `house` with an MCP server `search` in its
// `.mcp.json`, and the repo's own `.mcp.json` with a server `docs`
fn land_servers(rig: &Rig) {
    let market = r#"{"name":"house","plugins":[{"name":"style","source":"./style"}]}"#;
    rig.land("markets/house/.claude-plugin/marketplace.json", market);
    rig.land(
        "markets/house/style/skills/style/SKILL.md",
        "---\nname: style\n---\nRules.\n",
    );
    let plugin = json!({ "mcpServers": { "search": {
        "command": "${CLAUDE_PLUGIN_ROOT}/bin/search",
        "args": ["--stdio"],
    } } });
    rig.land("markets/house/style/.mcp.json", &plugin.to_string());
    rig.land("markets/house/style/bin/search", "#!/bin/sh\n");
    let settings = r#"{
        "extraKnownMarketplaces": {"house": {"source": {"source": "directory", "path": "markets/house"}}},
        "enabledPlugins": {"style@house": true}
    }"#;
    rig.land(".claude/settings.json", settings);
    let own = json!({ "mcpServers": { "docs": { "type": "http", "url": "https://docs.example.com/mcp" } } });
    rig.land(".mcp.json", &own.to_string());
}

// Names the project manager in `rig`'s settings, before its runner is opened
fn name_pm(rig: &Rig) {
    rig.edit_settings(|s| {
        let listed = "[app.dogs.kelpie.agents]\n";
        assert!(s.contains(listed), "the example's agents table moved");
        s.replace(listed, "[app.dogs.kelpie.agents]\npm = \"pm\"\n")
    });
}

// Writes a state file that already holds nine finished work items, so the next to finish starts
// the retro
fn nine_finished(rig: &Rig) {
    let mut state = ProjectState::new();
    state.history = (101..110)
        .map(|issue| Finished {
            issue: ticket(issue),
            title: format!("Issue {issue}"),
            pull_request: Some(issue + 60),
            merged: true,
            closed: false,
            at: crate::ports::Timestamp(Rig::EPOCH - 1000 + issue),
            wall: 600,
            seconds: Seconds::default(),
            spend: None,
        })
        .collect();
    let file = rig.paths().state;
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    StateStore::new(file).save(&state).unwrap();
}

// A repo with those servers, under `keys` of `repo_setup`, whose pull request was reviewed and
// merged, and whose project manager ran the retro that merge started
fn settled(keys: &str) -> Rig {
    let (rig, runner, _) = Rig::parked_set("koji", |rig| {
        land_servers(rig);
        name_pm(rig);
        rig.retro_on();
        nine_finished(rig);
        rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.repo_setup]\n{keys}\n"));
    });
    rig.claude
        .script_for(Role::Pm, [Scripted::Say("## Waits\n\nNone.\n")]);
    rig.ask(&runner, "rule", Some("71 yes"));
    for _ in 0..6 {
        rig.clock.advance(CHECKS_SETTLE);
        if matches!(step(&runner).unwrap(), Some(StepReport::Finished { .. })) {
            step(&runner).unwrap();
            break;
        }
    }
    assert!(!retros(&rig).is_empty(), "no retro ran");
    rig
}

// `role`'s calls, a retro left out
fn seen(rig: &Rig, role: Role) -> Vec<Seen> {
    let all = rig.claude.all_seen().into_iter();
    let calls: Vec<_> = all
        .filter(|s| s.call.role == role && s.call.tools != Tools::Retro)
        .collect();
    assert!(!calls.is_empty(), "{role:?} made no call");
    calls
}

// The project manager's retro calls
fn retros(rig: &Rig) -> Vec<Seen> {
    let all = rig.claude.all_seen().into_iter();
    all.filter(|s| s.call.tools == Tools::Retro).collect()
}

// The servers a call was given, by name
fn servers(call: &Seen) -> Vec<String> {
    call.call.reach.mcp_servers.keys().cloned().collect()
}

// The rules a call's settings allow
fn allowed(call: &Seen) -> Vec<String> {
    call.settings["permissions"]["allow"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|rule| rule.as_str().map(str::to_owned))
        .collect()
}

#[test]
fn a_call_that_reads_only_gets_no_server_it_could_write_with() {
    let rig = settled("");
    for retro in retros(&rig) {
        assert!(servers(&retro).is_empty(), "{:?}", servers(&retro));
        assert!(!allowed(&retro).iter().any(|rule| rule.starts_with("mcp__")));
    }
    for role in [Role::Worker, Role::Reviewer] {
        for call in seen(&rig, role) {
            assert_eq!(servers(&call), ["docs", "plugin_style_search"], "{role:?}");
        }
    }
}

#[test]
fn own_false_leaves_the_default_branchs_server_out_and_keeps_the_plugins() {
    let rig = settled("own = false");
    for role in [Role::Worker, Role::Reviewer] {
        for call in seen(&rig, role) {
            assert_eq!(servers(&call), ["plugin_style_search"], "{role:?}");
            assert!(!allowed(&call).contains(&"mcp__docs".to_owned()));
        }
    }
}

#[test]
fn plugins_false_leaves_the_plugins_server_out_and_keeps_the_default_branchs() {
    let rig = settled("plugins = false");
    for role in [Role::Worker, Role::Reviewer] {
        for call in seen(&rig, role) {
            assert_eq!(servers(&call), ["docs"], "{role:?}");
        }
    }
}

#[test]
fn parts_without_mcp_leaves_both_out() {
    let rig = settled("parts = [\"skills\", \"agents\", \"commands\", \"hooks\"]");
    for role in [Role::Worker, Role::Reviewer] {
        for call in seen(&rig, role) {
            assert!(servers(&call).is_empty(), "{role:?}");
            assert!(!allowed(&call).iter().any(|rule| rule.starts_with("mcp__")));
        }
    }
    assert!(!rig.paths().skills.join("repo/style/mcp-root").exists());
}

#[test]
fn a_reviewer_the_roles_leave_out_gets_neither_and_a_worker_gets_both() {
    let rig = settled("roles = [\"worker\"]");
    for call in seen(&rig, Role::Reviewer) {
        assert!(servers(&call).is_empty());
    }
    let works: Vec<_> = (seen(&rig, Role::Worker).into_iter())
        .filter(|c| c.call.tools == Tools::Work)
        .collect();
    assert!(!works.is_empty());
    for call in works {
        assert_eq!(servers(&call), ["docs", "plugin_style_search"]);
    }
}

#[test]
fn roles_worker_leaves_every_other_calls_without_the_plugins() {
    let rig = settled("roles = [\"worker\"]");
    let plugin = rig.paths().skills.join("repo/style");
    for role in [Role::Worker, Role::Reviewer] {
        for call in seen(&rig, role) {
            let gets = call.call.plugin_dirs.contains(&plugin);
            assert_eq!(
                gets,
                role == Role::Worker,
                "{role:?}: {:?}",
                call.call.plugin_dirs
            );
        }
    }
    for retro in retros(&rig) {
        assert!(!retro.call.plugin_dirs.contains(&plugin));
    }
}

#[test]
fn a_server_a_workers_branch_adds_to_mcp_json_is_not_loaded() {
    let rig = Rig::new("shep");
    land_servers(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    let sneaky = r#"{"mcpServers":{"sneaky":{"command":"sh"}}}"#;
    rig.claude.script([Scripted::Push(".mcp.json", sneaky)]);
    step(&runner).unwrap();
    // A runner that rereads its settings still reads the default branch.
    let (settings, kelpie) = (rig.settings(), rig.kelpie_settings());
    runner.lock().unwrap().reread(settings, kelpie).unwrap();
    step(&runner).unwrap();
    for call in rig.claude.all_seen() {
        assert!(!servers(&call).contains(&"sneaky".to_owned()));
    }
    assert_eq!(
        servers(&rig.claude.all_seen()[0]),
        ["docs", "plugin_style_search"]
    );
}
