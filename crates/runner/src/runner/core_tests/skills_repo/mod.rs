use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::ports::{AgentCall, Role};
use crate::runner::step;
use crate::test::{Rig, Scripted, Synced, git};

mod frontmatter;
mod mcp;
mod narrow;
mod own;
mod parts;
mod rest;

// The files of a marketplace named `market` holding each of `plugins` with one skill, by path
// from the marketplace
fn files(market: &str, plugins: &[(&str, &str)]) -> Vec<(String, String)> {
    let listed: Vec<_> = plugins
        .iter()
        .map(|(name, _)| json!({ "name": name, "source": format!("./plugins/{name}") }))
        .collect();
    let file = json!({ "name": market, "plugins": listed });
    let mut files = vec![(
        ".claude-plugin/marketplace.json".to_owned(),
        file.to_string(),
    )];
    for (name, skill) in plugins {
        let manifest = json!({ "name": name, "version": "1.2.0" });
        files.push((
            format!("plugins/{name}/.claude-plugin/plugin.json"),
            manifest.to_string(),
        ));
        files.push((
            format!("plugins/{name}/skills/{skill}/SKILL.md"),
            "---\nname: style\n---\nRules.\n".to_owned(),
        ));
    }
    files
}

// A `directory` marketplace named `market` landed on the default branch, and its path from the
// checkout
fn marketplace(rig: &Rig, market: &str, plugins: &[(&str, &str)]) -> PathBuf {
    let relative = Path::new("markets").join(market);
    for (file, text) in files(market, plugins) {
        rig.land(&relative.join(file).display().to_string(), &text);
    }
    relative
}

// The same marketplace in a folder of the rig's home, for the git port to serve
fn served(rig: &Rig, market: &str, plugins: &[(&str, &str)]) -> PathBuf {
    let dir = rig.home.path().join("served").join(market);
    for (file, text) in files(market, plugins) {
        fs::create_dir_all(dir.join(&file).parent().unwrap()).unwrap();
        fs::write(dir.join(file), text).unwrap();
    }
    dir
}

#[cfg(unix)]
// Lands `file` as a link to `target` on the default branch, after some other file landed
fn land_link(rig: &Rig, file: &str, target: &str) {
    let other = rig.home.path().join("other");
    let at = other.join(file);
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, &at).unwrap();
    git(&other, &["add", file]);
    git(&other, &["commit", "--quiet", "-m", "a link"]);
    git(&other, &["push", "--quiet", "origin", "main"]);
}

// Lands `file` holding `bytes` on the default branch, as `Rig::land` does for text, for a file no
// harness reads as text
fn land_bytes(rig: &Rig, file: &str, bytes: &[u8]) {
    let other = rig.home.path().join("other");
    let at = other.join(file);
    fs::create_dir_all(at.parent().unwrap()).unwrap();
    fs::write(&at, bytes).unwrap();
    git(&other, &["add", file]);
    git(&other, &["commit", "--quiet", "-m", "landed elsewhere"]);
    git(&other, &["push", "--quiet", "origin", "main"]);
}

// What a runner's checkout learns of the default branch when anything fetches it
fn fetch(rig: &Rig) {
    git(&rig.repo(), &["fetch", "--quiet", "origin", "main"]);
}

// A marketplace read from a folder of the default branch
fn directory(dir: &Path) -> serde_json::Value {
    json!({ "source": "directory", "path": dir })
}

// A marketplace on GitHub
fn github(repo: &str) -> serde_json::Value {
    json!({ "source": "github", "repo": repo })
}

// Lands the repo's `.claude/settings.json` on origin's `main`: each of `enabled`
// as `plugin@market` with its flag, and each marketplace in `markets` by its source
fn land_settings(rig: &Rig, enabled: &[(&str, bool)], markets: &[(&str, serde_json::Value)]) {
    let known: serde_json::Map<_, _> = markets
        .iter()
        .map(|(name, source)| ((*name).to_owned(), json!({ "source": source })))
        .collect();
    let enabled: serde_json::Map<_, _> = enabled
        .iter()
        .map(|(plugin, on)| ((*plugin).to_owned(), json!(on)))
        .collect();
    let text = json!({ "extraKnownMarketplaces": known, "enabledPlugins": enabled });
    rig.land(".claude/settings.json", &text.to_string());
}

fn call_of(rig: &Rig, role: Role) -> AgentCall {
    rig.claude
        .all_calls()
        .into_iter()
        .find(|c| c.role == role)
        .expect("the call ran")
}

#[test]
fn a_plugin_the_repo_turns_on_reaches_the_workers_call() {
    let (rig, _runner, _) = Rig::with_pull_request_set("shep", |rig| {
        let dir = marketplace(rig, "house", &[("style", "style")]);
        land_settings(rig, &[("style@house", true)], &[("house", directory(&dir))]);
    });
    let plugin = rig.paths().skills.join("repo/style");
    let worker = call_of(&rig, Role::Worker);
    assert!(
        worker.plugin_dirs.contains(&plugin),
        "{:?}",
        worker.plugin_dirs
    );
    assert!(plugin.join("skills/style/SKILL.md").is_file());
    let manifest = fs::read_to_string(plugin.join(".claude-plugin/plugin.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&manifest).unwrap()["name"],
        "style"
    );
}

#[test]
fn status_lists_the_plugin_with_its_skills() {
    let (rig, runner, _) = Rig::with_pull_request_set("shep", |rig| {
        let dir = marketplace(rig, "house", &[("style", "style")]);
        land_settings(rig, &[("style@house", true)], &[("house", directory(&dir))]);
    });
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["plugins"],
        json!([{ "plugin": "style@house", "skills": ["style"] }])
    );
}

#[test]
fn a_reviewer_gets_the_plugin_as_well() {
    let (rig, _runner, _) = Rig::with_pull_request_set("shep", |rig| {
        let dir = marketplace(rig, "house", &[("style", "style")]);
        land_settings(rig, &[("style@house", true)], &[("house", directory(&dir))]);
    });
    let plugin = rig.paths().skills.join("repo/style");
    let reviewer = call_of(&rig, Role::Reviewer);
    assert_eq!(reviewer.plugin_dirs, [plugin]);
}

#[test]
fn the_project_manager_gets_the_plugin_as_well() {
    let rig = Rig::new("koji");
    rig.edit_settings(|s| {
        let listed = "[app.dogs.kelpie.agents]\n";
        assert!(s.contains(listed), "the example's agents table moved");
        s.replace(listed, "[app.dogs.kelpie.agents]\npm = \"pm\"\n")
    });
    let dir = marketplace(&rig, "house", &[("style", "style")]);
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", directory(&dir))],
    );
    let runner = rig.open().unwrap();
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude
        .script([Scripted::Say("{\"pick\": 12, \"hold\": []}")]);
    step(&runner).unwrap();
    let plugin = rig.paths().skills.join("repo/style");
    let issue = rig.paths().skills.join("kelpie-issue");
    let pm = call_of(&rig, Role::Pm);
    assert_eq!(pm.plugin_dirs, [issue, plugin]);
}

#[test]
fn a_plugin_the_repo_sets_to_false_does_not_load() {
    let (rig, runner, _) = Rig::with_pull_request_set("shep", |rig| {
        let dir = marketplace(rig, "house", &[("style", "style"), ("quiet", "quiet")]);
        let enabled = [("style@house", true), ("quiet@house", false)];
        land_settings(rig, &enabled, &[("house", directory(&dir))]);
    });
    let skills = rig.paths().skills;
    let worker = call_of(&rig, Role::Worker);
    assert!(worker.plugin_dirs.contains(&skills.join("repo/style")));
    assert!(!worker.plugin_dirs.contains(&skills.join("repo/quiet")));
    assert!(!skills.join("repo/quiet").exists());
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["plugins"].as_array().unwrap().len(), 1);
}

#[test]
fn a_plugin_a_workers_branch_turns_on_is_not_loaded() {
    let rig = Rig::new("shep");
    let dir = marketplace(&rig, "house", &[("style", "style")]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    // The worker's branch turns the plugin on for itself.
    let branch = format!(
        "{{\"extraKnownMarketplaces\":{{\"house\":{{\"source\":{{\"source\":\"directory\",\
         \"path\":{:?}}}}}}},\"enabledPlugins\":{{\"style@house\":true}}}}",
        dir.display().to_string()
    );
    let branch: &'static str = Box::leak(branch.into_boxed_str());
    rig.claude
        .script([Scripted::Push(".claude/settings.json", branch)]);
    step(&runner).unwrap();
    // A runner that rereads its settings still reads the default branch.
    let (settings, kelpie) = (rig.settings(), rig.kelpie_settings());
    runner.lock().unwrap().reread(settings, kelpie).unwrap();
    step(&runner).unwrap();
    for call in rig.claude.all_calls() {
        assert!(call.plugin_dirs.iter().all(|p| !p.ends_with("repo/style")));
    }
    assert!(!rig.paths().skills.join("repo").join("style").exists());
    assert!(rig.ask(&runner, "status", None).get("plugins").is_none());
}

#[test]
fn a_plugin_the_repo_turns_on_after_the_runner_started_loads_when_it_rereads() {
    let rig = Rig::new("shep");
    let dir = marketplace(&rig, "house", &[("style", "style")]);
    let runner = rig.open().unwrap();
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", directory(&dir))],
    );
    let (settings, kelpie) = (rig.settings(), rig.kelpie_settings());
    let line = runner.lock().unwrap().reread(settings, kelpie).unwrap();
    assert!(line.is_some(), "a new plugin is a change");
    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    let worker = call_of(&rig, Role::Worker);
    assert!(
        worker
            .plugin_dirs
            .contains(&rig.paths().skills.join("repo/style"))
    );
}

// What `status` says of the only plugin the repo turns on
fn skipped(rig: &Rig, runner: &std::sync::Mutex<crate::runner::Runner>) -> serde_json::Value {
    let status = rig.ask(runner, "status", None);
    let plugins = status["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 1, "{plugins:?}");
    assert_eq!(plugins[0]["skills"], json!([]));
    plugins[0].clone()
}

#[test]
fn a_plugin_the_marketplace_lacks_is_skipped_with_its_reason_and_the_run_goes_on() {
    let (rig, runner, _) = Rig::with_pull_request_set("shep", |rig| {
        let dir = marketplace(rig, "house", &[("style", "style")]);
        land_settings(
            rig,
            &[("absent@house", true)],
            &[("house", directory(&dir))],
        );
    });
    let plugin = skipped(&rig, &runner);
    assert_eq!(plugin["plugin"], "absent@house");
    assert_eq!(plugin["skipped"], "the marketplace lists no plugin absent");
    let worker = call_of(&rig, Role::Worker);
    assert_eq!(
        worker.plugin_dirs,
        [
            rig.paths().skills.join("mattpocock"),
            rig.paths().skills.join("kelpie-wording"),
            rig.paths().skills.join("kelpie-issue")
        ]
    );
}

#[test]
fn a_plugin_with_no_marketplace_entry_is_skipped() {
    let (rig, runner, _) = Rig::with_pull_request_set("shep", |rig| {
        land_settings(rig, &[("style@nowhere", true)], &[]);
    });
    let plugin = skipped(&rig, &runner);
    assert_eq!(
        plugin["skipped"],
        ".claude/settings.json has no extraKnownMarketplaces entry for nowhere"
    );
}

#[test]
fn kelpies_own_plugin_names_are_refused() {
    let (rig, runner, _) = Rig::with_pull_request_set("shep", |rig| {
        let dir = marketplace(rig, "house", &[("mattpocock", "tdd"), ("kelpie-pr", "pr")]);
        let enabled = [("mattpocock@house", true), ("kelpie-pr@house", true)];
        land_settings(rig, &enabled, &[("house", directory(&dir))]);
    });
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["plugins"],
        json!([
            {
                "plugin": "kelpie-pr@house",
                "skills": [],
                "skipped": "it is named kelpie-pr, which is kelpie's own"
            },
            {
                "plugin": "mattpocock@house",
                "skills": [],
                "skipped": "it is named mattpocock, which is kelpie's own"
            },
        ])
    );
    // Kelpie's own plugins are the only ones the worker gets.
    let worker = call_of(&rig, Role::Worker);
    assert_eq!(
        worker.plugin_dirs,
        [
            rig.paths().skills.join("mattpocock"),
            rig.paths().skills.join("kelpie-wording"),
            rig.paths().skills.join("kelpie-issue")
        ]
    );
    assert!(!rig.paths().skills.join("repo").join("mattpocock").exists());
}

const HOUSE: &str = "https://github.com/shep-pm/house.git";

#[test]
fn a_github_marketplace_is_fetched_into_kelpies_home_through_git() {
    let rig = Rig::new("shep");
    let served = served(&rig, "served", &[("style", "style")]);
    rig.git.serve(HOUSE, &served);
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", github("shep-pm/house"))],
    );
    let runner = rig.open().unwrap();
    let into = rig.paths().kelpie_home.join("marketplaces/house");
    assert_eq!(
        rig.git.asked(),
        [Synced {
            url: HOUSE.to_owned(),
            reference: None,
            into: into.clone()
        }]
    );
    assert!(into.join(".claude-plugin/marketplace.json").is_file());
    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    let worker = call_of(&rig, Role::Worker);
    assert!(
        worker
            .plugin_dirs
            .contains(&rig.paths().skills.join("repo/style"))
    );
}

#[test]
fn a_git_marketplace_is_fetched_at_its_ref() {
    let rig = Rig::new("shep");
    let served = served(&rig, "served", &[("style", "style")]);
    rig.git
        .serve("ssh://git@example.invalid/house.git", &served);
    let source =
        json!({ "source": "git", "url": "ssh://git@example.invalid/house.git", "ref": "v2" });
    land_settings(&rig, &[("style@house", true)], &[("house", source)]);
    rig.open().unwrap();
    let [synced] = rig.git.asked().try_into().unwrap();
    assert_eq!(synced.reference.as_deref(), Some("v2"));
}

#[test]
fn a_failed_fetch_keeps_the_last_copy() {
    let rig = Rig::new("shep");
    let served = served(&rig, "served", &[("style", "style")]);
    rig.git.serve(HOUSE, &served);
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", github("shep-pm/house"))],
    );
    let runner = rig.open().unwrap();
    rig.git.go_down();
    // A runner that starts with the network down, and one that rereads, both keep the copy.
    let (settings, kelpie) = (rig.settings(), rig.kelpie_settings());
    runner.lock().unwrap().reread(settings, kelpie).unwrap();
    for runner in [&runner, &rig.open().unwrap()] {
        let status = rig.ask(runner, "status", None);
        let plugin = &status["plugins"][0];
        assert_eq!(plugin["skills"], json!(["style"]));
        assert_eq!(plugin["skipped"], json!(null));
        assert_eq!(
            plugin["stale"],
            "fetching the house marketplace failed: could not resolve host"
        );
    }
    rig.ask(&runner, "add", Some("7"));
    step(&runner).unwrap();
    let worker = call_of(&rig, Role::Worker);
    assert!(
        worker
            .plugin_dirs
            .contains(&rig.paths().skills.join("repo/style"))
    );
}

#[test]
fn a_failed_fetch_with_no_copy_skips_the_plugin() {
    let rig = Rig::new("shep");
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", github("shep-pm/house"))],
    );
    rig.git.go_down();
    let runner = rig.open().unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["plugins"],
        json!([{
            "plugin": "style@house",
            "skills": [],
            "skipped": "fetching the house marketplace failed: could not resolve host"
        }])
    );
}

#[test]
fn an_address_git_would_take_for_an_option_or_a_program_is_refused() {
    let rig = Rig::new("shep");
    let option = json!({ "source": "git", "url": "--upload-pack=touch x" });
    let program = json!({ "source": "git", "url": "ext::sh -c touch% x" });
    land_settings(
        &rig,
        &[("a@option", true), ("b@program", true)],
        &[("option", option), ("program", program)],
    );
    rig.open().unwrap();
    assert_eq!(rig.git.asked(), []);
}

#[test]
fn inspecting_lists_what_a_runner_would_load_and_why_it_would_not() {
    let (rig, _runner, _) = Rig::with_pull_request_set("shep", |rig| {
        let dir = marketplace(rig, "house", &[("style", "style")]);
        land_settings(
            rig,
            &[
                ("style@house", true),
                ("absent@house", true),
                ("far@away", true),
            ],
            &[("house", directory(&dir)), ("away", github("acme/away"))],
        );
    });

    let setup = crate::settings::RepoSetup::default();
    let plugins = crate::skills::inspect(&rig.repo(), "main", &rig.paths().kelpie_home, &setup);

    let seen: Vec<_> = plugins
        .iter()
        .map(|p| (p.plugin.as_str(), p.skills.clone(), p.skipped.as_deref()))
        .collect();
    assert_eq!(
        seen,
        vec![
            (
                "absent@house",
                vec![],
                Some("the marketplace lists no plugin absent")
            ),
            (
                "far@away",
                vec![],
                Some(
                    "fetching the away marketplace failed: it is not fetched yet, which a runner does as it starts"
                )
            ),
            ("style@house", vec!["style".to_owned()], None),
        ]
    );
}

#[test]
fn a_directory_that_is_not_inside_the_checkout_is_refused() {
    let rig = Rig::new("shep");
    let far = served(&rig, "far", &[("style", "style")]);
    land_settings(
        &rig,
        &[("style@absolute", true), ("style@dots", true)],
        &[
            ("absolute", directory(&far)),
            ("dots", directory(Path::new("../../served/far"))),
        ],
    );

    let runner = rig.open().unwrap();

    let status = rig.ask(&runner, "status", None);
    let reasons: Vec<_> = status["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["skipped"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(reasons.len(), 2, "{}", status["plugins"]);
    for reason in reasons {
        assert!(
            reason.ends_with("it is not a folder inside the checkout"),
            "{reason}"
        );
    }
    assert!(!rig.paths().skills.join("repo/style").exists());
}

#[test]
fn a_directory_only_the_working_tree_holds_is_not_read() {
    let rig = Rig::new("shep");
    // The checkout may be on another branch, or behind the default branch.
    let on_disk = rig.repo().join("markets/house/.claude-plugin");
    fs::create_dir_all(&on_disk).unwrap();
    let listed = json!({ "plugins": [{ "name": "style", "source": "./style" }] });
    fs::write(on_disk.join("marketplace.json"), listed.to_string()).unwrap();
    let skill = rig.repo().join("markets/house/style/skills/style");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "Rules.\n").unwrap();
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", directory(Path::new("markets/house")))],
    );

    let runner = rig.open().unwrap();

    let plugin = skipped(&rig, &runner);
    assert_eq!(
        plugin["skipped"],
        "its directory markets/house cannot be read from the default branch: it holds no files there"
    );
}

#[test]
fn a_plugin_folder_that_leaves_its_marketplace_is_refused() {
    let rig = Rig::new("shep");
    let listed = json!({ "plugins": [{ "name": "style", "source": "../outside" }] });
    rig.land(
        "markets/house/.claude-plugin/marketplace.json",
        &listed.to_string(),
    );
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", directory(Path::new("markets/house")))],
    );

    let runner = rig.open().unwrap();

    let plugin = skipped(&rig, &runner);
    assert_eq!(
        plugin["skipped"],
        "its source ../outside leaves the marketplace"
    );
}

#[test]
fn two_plugins_of_one_name_from_two_marketplaces_load_only_the_first() {
    let rig = Rig::new("shep");
    let one = marketplace(&rig, "one", &[("style", "first")]);
    let two = marketplace(&rig, "two", &[("style", "second")]);
    land_settings(
        &rig,
        &[("style@one", true), ("style@two", true)],
        &[("one", directory(&one)), ("two", directory(&two))],
    );

    let runner = rig.open().unwrap();

    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["plugins"],
        json!([
            { "plugin": "style@one", "skills": ["first"] },
            {
                "plugin": "style@two",
                "skills": [],
                "skipped": "style@one already supplies a plugin named style"
            },
        ])
    );
    let skills = rig.paths().skills.join("repo/style/skills");
    assert!(skills.join("first/SKILL.md").is_file());
    assert!(!skills.join("second").exists());
}

#[cfg(unix)]
#[test]
fn a_link_in_a_plugin_is_neither_listed_nor_copied() {
    let rig = Rig::new("shep");
    let dir = marketplace(&rig, "house", &[("style", "style")]);
    // A skill that is a link to a folder, and a file in a skill that is a link to a file.
    land_link(&rig, "markets/house/plugins/style/skills/linked", "style");
    land_link(
        &rig,
        "markets/house/plugins/style/skills/style/notes.md",
        "SKILL.md",
    );
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", directory(&dir))],
    );

    let runner = rig.open().unwrap();

    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["plugins"],
        json!([{ "plugin": "style@house", "skills": ["style"] }])
    );
    let copied = rig.paths().skills.join("repo/style/skills");
    assert!(copied.join("style/SKILL.md").is_file());
    assert!(!copied.join("linked").exists());
    assert!(!copied.join("style/notes.md").exists());
}

#[test]
fn a_reread_that_fails_leaves_the_plugins_the_runner_is_using() {
    let rig = Rig::new("shep");
    let dir = marketplace(&rig, "house", &[("style", "style")]);
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", directory(&dir))],
    );
    let runner = rig.open().unwrap();
    // The repo turns the plugin off, and the settings the reread is given do not hold.
    land_settings(
        &rig,
        &[("style@house", false)],
        &[("house", directory(&dir))],
    );
    let (mut settings, kelpie) = (rig.settings(), rig.kelpie_settings());
    settings.worker.instructions_file = Some(rig.home.path().join("missing.md"));

    let failed = runner.lock().unwrap().reread(settings, kelpie);

    assert!(failed.is_err());
    assert!(
        rig.paths()
            .skills
            .join("repo/style/skills/style/SKILL.md")
            .is_file()
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["plugins"],
        json!([{ "plugin": "style@house", "skills": ["style"] }])
    );
}

#[test]
fn plugins_of_one_name_are_taken_in_the_order_the_repo_lists_them() {
    let rig = Rig::new("shep");
    marketplace(&rig, "zeta", &[("style", "from-zeta")]);
    marketplace(&rig, "alpha", &[("style", "from-alpha")]);
    // Listed zeta first, which is not alphabetical.
    let settings = r#"{
        "extraKnownMarketplaces": {
            "zeta": { "source": { "source": "directory", "path": "markets/zeta" } },
            "alpha": { "source": { "source": "directory", "path": "markets/alpha" } }
        },
        "enabledPlugins": { "style@zeta": true, "style@alpha": true }
    }"#;
    rig.land(".claude/settings.json", settings);

    let runner = rig.open().unwrap();

    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["plugins"],
        json!([
            { "plugin": "style@zeta", "skills": ["from-zeta"] },
            {
                "plugin": "style@alpha",
                "skills": [],
                "skipped": "style@zeta already supplies a plugin named style"
            },
        ])
    );
}

#[test]
fn a_merge_that_turns_a_plugin_on_loads_without_a_settings_change() {
    let rig = Rig::new("shep");
    let dir = marketplace(&rig, "house", &[("style", "style")]);
    let runner = rig.open().unwrap();
    assert_eq!(runner.lock().unwrap().reread_plugins(), None);

    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", directory(&dir))],
    );
    fetch(&rig);
    let line = runner.lock().unwrap().reread_plugins();

    assert!(line.is_some(), "a new plugin is a change");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["plugins"],
        json!([{ "plugin": "style@house", "skills": ["style"] }])
    );
    // Nothing moved since, so there is nothing to read again.
    assert_eq!(runner.lock().unwrap().reread_plugins(), None);
}

#[test]
fn a_merge_that_changes_a_skills_text_alone_is_copied_over() {
    let rig = Rig::new("shep");
    let dir = marketplace(&rig, "house", &[("style", "style")]);
    land_settings(
        &rig,
        &[("style@house", true)],
        &[("house", directory(&dir))],
    );
    let runner = rig.open().unwrap();
    let copy = rig.paths().skills.join("repo/style/skills/style/SKILL.md");
    assert!(fs::read_to_string(&copy).unwrap().contains("Rules."));

    rig.land(
        "markets/house/plugins/style/skills/style/SKILL.md",
        "New rules.\n",
    );
    fetch(&rig);
    let line = runner.lock().unwrap().reread_plugins();

    assert!(line.is_some(), "new text is a change");
    assert_eq!(fs::read_to_string(&copy).unwrap(), "New rules.\n");
}
