//! The part of a repo plugin that is none of the four, and the root a skill names

use std::fs;
use std::path::Path;

use super::{directory, fetch, land_settings, marketplace};
use crate::runner::step;
use crate::test::{Rig, assert_reads};

// A plugin whose `docs/` is none of its part folders, with a skill that names a file under the
// plugin's root
fn with_docs(rig: &Rig) {
    let dir = marketplace(rig, "house", &[("style", "style")]);
    rig.land(
        "markets/house/plugins/style/docs/rules.md",
        "IR-47: one file per rule.\n",
    );
    rig.land(
        "markets/house/plugins/style/skills/style/SKILL.md",
        "---\nname: style\n---\nRead `${CLAUDE_PLUGIN_ROOT}/docs/rules.md` first.\n",
    );
    land_settings(rig, &[("style@house", true)], &[("house", directory(&dir))]);
}

#[test]
fn a_plugins_folder_that_is_none_of_the_parts_is_in_the_copy() {
    let rig = Rig::new("shep");
    with_docs(&rig);
    rig.open().unwrap();
    let plugin = rig.paths().skills.join("repo/style");
    assert!(plugin.join("docs/rules.md").is_file());
}

#[test]
fn a_merge_that_changes_a_file_outside_the_parts_is_copied_over() {
    let rig = Rig::new("shep");
    with_docs(&rig);
    let runner = rig.open().unwrap();
    let copy = rig.paths().skills.join("repo/style/docs/rules.md");
    assert_eq!(
        fs::read_to_string(&copy).unwrap(),
        "IR-47: one file per rule.\n"
    );

    rig.land(
        "markets/house/plugins/style/docs/rules.md",
        "IR-47: one file per rule, and one rule per file.\n",
    );
    fetch(&rig);
    let line = runner.lock().unwrap().reread_plugins();

    assert!(line.is_some(), "new text is a change");
    assert_eq!(
        fs::read_to_string(&copy).unwrap(),
        "IR-47: one file per rule, and one rule per file.\n"
    );
}

#[test]
fn a_worker_reads_a_file_of_the_plugin_it_is_given() {
    let rig = Rig::new("shep");
    with_docs(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    let worker = rig.claude.seen().into_iter().next().unwrap();
    assert_reads(
        &worker,
        &rig.paths().skills.join("repo/style/docs/rules.md"),
    );
}

#[test]
fn a_skill_that_names_the_plugins_root_names_the_copy() {
    let rig = Rig::new("shep");
    with_docs(&rig);
    rig.open().unwrap();
    let plugin = rig.paths().skills.join("repo/style");
    let skill = fs::read_to_string(plugin.join("skills/style/SKILL.md")).unwrap();
    assert!(!skill.contains("${CLAUDE_PLUGIN_ROOT}"), "{skill}");
    // The path the skill now names is a file in the copy, which a worker can read.
    let named = skill
        .split('`')
        .nth(1)
        .unwrap_or_else(|| panic!("the skill names no path: {skill}"));
    assert!(Path::new(named).is_file(), "{named}");
    assert_eq!(
        fs::read_to_string(named).unwrap(),
        "IR-47: one file per rule.\n"
    );
}

// A plugin whose skill names a file at the plugin's own root, beside a `.mcp.json` there
fn at_the_root(rig: &Rig) {
    let dir = marketplace(rig, "house", &[("style", "style")]);
    rig.land(
        "markets/house/plugins/style/RULES.md",
        "IR-47: one file per rule.\n",
    );
    rig.land(
        "markets/house/plugins/style/.mcp.json",
        r#"{"mcpServers":{"docs":{"command":"docs"}}}"#,
    );
    rig.land(
        "markets/house/plugins/style/skills/style/SKILL.md",
        "---\nname: style\n---\nRead `${CLAUDE_PLUGIN_ROOT}/RULES.md` first.\n",
    );
    land_settings(rig, &[("style@house", true)], &[("house", directory(&dir))]);
}

#[test]
fn a_file_at_the_plugins_root_is_in_the_copy_where_a_skill_names_it() {
    let rig = Rig::new("shep");
    at_the_root(&rig);
    rig.open().unwrap();
    let plugin = rig.paths().skills.join("repo/style");
    let skill = fs::read_to_string(plugin.join("skills/style/SKILL.md")).unwrap();
    let named = skill
        .split('`')
        .nth(1)
        .unwrap_or_else(|| panic!("the skill names no path: {skill}"));
    assert!(Path::new(named).is_file(), "{named}");
    assert_eq!(
        fs::read_to_string(named).unwrap(),
        "IR-47: one file per rule.\n"
    );
}

#[test]
fn a_worker_reads_a_file_at_the_plugins_root() {
    let rig = Rig::new("shep");
    at_the_root(&rig);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    let worker = rig.claude.seen().into_iter().next().unwrap();
    let rules = rig.paths().skills.join("repo/style/RULES.md");
    assert!(
        rules.is_file(),
        "{} is copied for the worker to read",
        rules.display()
    );
    assert_reads(&worker, &rules);
}

#[test]
fn a_plugins_mcp_file_is_copied_where_a_server_runs_it_and_not_beside_the_copy() {
    let rig = Rig::new("shep");
    at_the_root(&rig);
    rig.open().unwrap();
    let plugin = rig.paths().skills.join("repo/style");
    // The copy's manifest is trimmed of `mcpServers` and kelpie hands the servers
    // to the call itself, so a second copy beside it is one the harness could
    // load on its own.
    assert!(!plugin.join(".mcp.json").exists());
    assert!(plugin.join("mcp-root/.mcp.json").is_file());
}
