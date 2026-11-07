//! What the project manager reads of every project's settings: a copy of
//! each table the shepherd holds, written into its folder as it wakes

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::{Map, Value, json};

use super::{answered, pm_seen, step_until, with_pm};
use crate::runner::{Runner, StepReport};
use crate::test::{Rig, Scripted, project_table};

const NOTHING: &str = "{\"pick\": null}";

type Tables = BTreeMap<String, Map<String, Value>>;

/// A project's table as shep hands it, with `keys` set over the example's
fn table(rig: &Rig, keys: Value) -> Map<String, Value> {
    let entry = std::fs::read_to_string(rig.settings_file()).unwrap();
    let mut table = project_table(&entry);
    for (key, value) in keys.as_object().unwrap() {
        table.insert(key.clone(), value.clone());
    }
    table
}

fn peers(runner: &Mutex<Runner>, tables: &Tables) {
    runner.lock().unwrap().set_peer_tables(tables.clone());
}

/// Tells the project manager something and waits for its answer, which is its wake
fn wake(rig: &Rig, runner: &Mutex<Runner>) {
    rig.ask(runner, "tell", Some("how are we set up?"));
    rig.claude.script([Scripted::Say(NOTHING)]);
    answered(Some(step_until(runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    assert_eq!(pm_seen(rig).last().unwrap().call.cwd, rig.paths().pm);
}

fn copy(rig: &Rig, project: &str) -> Option<String> {
    std::fs::read_to_string(rig.paths().pm.join(format!("settings/{project}.toml"))).ok()
}

fn board_of(rig: &Rig) -> String {
    std::fs::read_to_string(rig.paths().pm.join("board.md")).unwrap()
}

#[test]
fn each_projects_settings_are_in_the_folder() {
    let (rig, runner) = with_pm("acme");
    let git = |checkout| json!({ "git": { "checkout": checkout, "merging": "ask" } });
    let own = table(&rig, git("/srv/acme"));
    let other = table(&rig, git("/srv/koji"));
    peers(
        &runner,
        &Tables::from([("acme".into(), own.clone()), ("koji".into(), other.clone())]),
    );
    wake(&rig, &runner);

    for (name, table) in [("acme", own), ("koji", other)] {
        let text = copy(&rig, name).unwrap_or_else(|| panic!("no copy of {name}"));
        let read: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(serde_json::to_value(read).unwrap(), Value::Object(table));
    }
    let board = board_of(&rig);
    assert!(board.contains("`settings/<project>.toml`"), "{board}");
}

#[test]
fn a_project_gone_leaves_no_file() {
    let (rig, runner) = with_pm("acme");
    let one = table(&rig, json!({}));
    peers(
        &runner,
        &Tables::from([("acme".into(), one.clone()), ("koji".into(), one.clone())]),
    );
    wake(&rig, &runner);
    assert!(copy(&rig, "koji").is_some());

    peers(&runner, &Tables::from([("acme".into(), one)]));
    wake(&rig, &runner);
    assert!(copy(&rig, "acme").is_some());
    assert_eq!(copy(&rig, "koji"), None);
}

#[test]
fn kelpies_own_section_is_written_nowhere() {
    let (rig, runner) = with_pm("acme");
    assert!(Rig::WEBHOOK_URL.contains(Rig::WEBHOOK_SECRET));
    peers(
        &runner,
        &Tables::from([("acme".into(), table(&rig, json!({})))]),
    );
    wake(&rig, &runner);

    let mut files = vec![rig.paths().pm];
    while let Some(path) = files.pop() {
        if path.is_dir() {
            files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            assert!(!text.contains(Rig::WEBHOOK_SECRET), "{}", path.display());
        }
    }
}

#[test]
fn a_secret_in_a_table_is_not_in_its_copy() {
    let (rig, runner) = with_pm("acme");
    let leaky = table(
        &rig,
        json!({ "extra": { "api_token": "tk_live_1", "env": { "KEY": "v4lue" } } }),
    );
    peers(&runner, &Tables::from([("koji".into(), leaky)]));
    wake(&rig, &runner);

    let text = copy(&rig, "koji").unwrap();
    assert!(
        !text.contains("tk_live_1") && !text.contains("v4lue"),
        "{text}"
    );
    assert!(text.contains("KEY"), "{text}");
}

#[test]
fn a_path_naming_this_machine_is_shown_as_the_board_shows_it() {
    let (rig, runner) = with_pm("acme");
    let mine = rig.home.path().join("code/koji");
    let other = table(
        &rig,
        json!({ "git": { "checkout": mine.display().to_string(), "merging": "auto" } }),
    );
    peers(&runner, &Tables::from([("koji".into(), other)]));
    wake(&rig, &runner);

    let text = copy(&rig, "koji").unwrap();
    assert!(!text.contains("code/koji"), "{text}");
    assert!(text.contains("(withheld: it names "), "{text}");
    assert!(text.contains("merging = \"auto\""), "{text}");
}

#[test]
fn an_edit_to_the_copy_changes_no_setting() {
    let (rig, runner) = with_pm("acme");
    let own = table(&rig, json!({}));
    peers(&runner, &Tables::from([("acme".into(), own)]));
    wake(&rig, &runner);
    let before = runner.lock().unwrap().settings().clone();
    let file = rig.paths().pm.join("settings/acme.toml");
    let original = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, "not even toml [[\n").unwrap();

    wake(&rig, &runner);

    assert_eq!(runner.lock().unwrap().settings(), &before);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
}

#[test]
fn a_copy_that_cannot_be_written_says_so_and_the_wake_goes_on() {
    let (rig, runner) = with_pm("acme");
    peers(
        &runner,
        &Tables::from([("..".into(), table(&rig, json!({})))]),
    );
    runner.lock().unwrap().take_notes();
    wake(&rig, &runner);

    let board = board_of(&rig);
    assert!(board.contains("no settings for `..`"), "{board}");
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes.iter().any(|n| n.contains("settings of `..`")),
        "{notes:?}"
    );
}
