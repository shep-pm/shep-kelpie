//! `[app.dogs.kelpie.repo_setup]`: only `extra` has a say in the plugins the repo turns on

use std::sync::Mutex;

use serde_json::json;

use super::{call_of, directory, land_settings, marketplace};
use crate::ports::{AgentCall, Reach, Role, Tools};
use crate::runner::{Runner, step};
use crate::test::Rig;

// A runner whose `repo_setup` table holds `keys`, on a repo whose `.claude/settings.json` sets
// `style@house` and `quiet@house` to `enabled`, with a worker's first turn run
fn setting(keys: &str, enabled: [bool; 2]) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.repo_setup]\n{keys}\n"));
    let dir = marketplace(&rig, "house", &[("style", "style"), ("quiet", "quiet")]);
    let enabled = [("style@house", enabled[0]), ("quiet@house", enabled[1])];
    land_settings(&rig, &enabled, &[("house", directory(&dir))]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    (rig, runner)
}

// The same, with the repo turning both plugins on
fn narrowed(keys: &str) -> (Rig, Mutex<Runner>) {
    setting(keys, [true, true])
}

#[test]
fn a_plugin_the_repo_turns_on_loads_whatever_the_old_keys_say() {
    // Every key that used to narrow is read by nothing now.
    let old = "plugins = false\nown = false\nroles = [\"reviewer\"]\nparts = [\"skills\"]";
    let (rig, runner) = narrowed(old);
    let skills = rig.paths().skills;
    let worker = call_of(&rig, Role::Worker);
    assert!(worker.plugin_dirs.contains(&skills.join("repo/style")));
    assert!(worker.plugin_dirs.contains(&skills.join("repo/quiet")));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["plugins"],
        json!([
            { "plugin": "quiet@house", "skills": ["quiet"] },
            { "plugin": "style@house", "skills": ["style"] },
        ])
    );
}

#[test]
fn a_plugin_the_repo_turns_off_does_not_load() {
    // `skip` names the plugin the repo turns on, which loads whatever it says.
    let (rig, runner) = setting("skip = [\"style@house\"]", [true, false]);
    let skills = rig.paths().skills;
    let worker = call_of(&rig, Role::Worker);
    assert!(worker.plugin_dirs.contains(&skills.join("repo/style")));
    assert!(!worker.plugin_dirs.contains(&skills.join("repo/quiet")));
    assert!(!skills.join("repo/quiet").exists());
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["plugins"],
        json!([{ "plugin": "style@house", "skills": ["style"] }])
    );
}

#[test]
fn the_representative_gets_no_repo_plugin_and_no_repo_server() {
    let rig = Rig::new("shep");
    let dir = marketplace(&rig, "house", &[("style", "style")]);
    let server = json!({ "mcpServers": { "search": { "command": "search" } } });
    rig.land("markets/house/plugins/style/.mcp.json", &server.to_string());
    let own =
        json!({ "mcpServers": { "docs": { "type": "http", "url": "https://d.example/mcp" } } });
    rig.land(".mcp.json", &own.to_string());
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", directory(&dir))],
    );
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    let worker = call_of(&rig, Role::Worker);
    assert!(!worker.reach.mcp_servers.is_empty(), "the repo has servers");
    // Only a worker, a reviewer and the project manager get them, whatever `repo_setup` holds.
    for role in [Role::Representative, Role::Triager] {
        let call = AgentCall {
            role,
            tools: Tools::Answer,
            issue: None,
            plugin_dirs: Vec::new(),
            reach: Reach::default(),
            ..worker.clone()
        };
        let call = runner.lock().unwrap().prepared(call).unwrap();
        assert!(
            call.plugin_dirs.is_empty(),
            "{role:?}: {:?}",
            call.plugin_dirs
        );
        assert!(call.reach.mcp_servers.is_empty(), "{role:?}");
    }
}

// A runner on a repo that turns on `style@house`, whose `repo_setup` adds `quiet@lab` from a
// marketplace of the checkout the repo's settings do not name
fn adding() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    let lab = marketplace(&rig, "lab", &[("quiet", "quiet")]);
    let extra = format!(
        "extra = {{ \"quiet@lab\" = {:?} }}",
        format!("directory:{}", lab.display())
    );
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.repo_setup]\n{extra}\n"));
    let house = marketplace(&rig, "house", &[("style", "style")]);
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", directory(&house))],
    );
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    (rig, runner)
}

#[test]
fn extra_loads_a_plugin_the_repo_does_not_name() {
    let (rig, runner) = adding();
    let skills = rig.paths().skills;
    let worker = call_of(&rig, Role::Worker);
    assert!(worker.plugin_dirs.contains(&skills.join("repo/style")));
    assert!(worker.plugin_dirs.contains(&skills.join("repo/quiet")));
    assert!(skills.join("repo/quiet/skills/quiet/SKILL.md").is_file());
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["plugins"],
        json!([
            { "plugin": "style@house", "skills": ["style"] },
            { "plugin": "quiet@lab", "skills": ["quiet"] },
        ])
    );
}

#[test]
fn a_changed_repo_setup_loads_when_the_runner_rereads() {
    let (rig, runner) = narrowed("");
    let lab = marketplace(&rig, "lab", &[("loud", "loud")]);
    let extra = format!(
        "extra = {{ \"loud@lab\" = \"directory:{}\" }}\n",
        lab.display()
    );
    rig.edit_settings(|s| {
        s.replace(
            "\n[app.dogs.kelpie.repo_setup]\n",
            &format!("\n[app.dogs.kelpie.repo_setup]\n{extra}"),
        )
    });
    let (settings, kelpie) = (rig.settings(), rig.kelpie_settings());
    let line = runner.lock().unwrap().reread(settings, kelpie).unwrap();
    assert!(line.is_some_and(|l| l.contains("repo_setup")));
    assert!(rig.paths().skills.join("repo/loud").exists());
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["plugins"][2]["plugin"], "loud@lab");
}
