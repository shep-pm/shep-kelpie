//! A state file saved while a work item kept its worker's model and effort

use std::fs;

use serde_json::{Value, json};

use super::{saved_with, store_in};
use crate::test::a_work_item;

// The work item as saved before agent files, its worker as `worker` held it.
fn with_worker(worker: Value) -> impl FnOnce(&mut Value) {
    move |value: &mut Value| {
        let item = value["work_items"][0].as_object_mut().unwrap();
        item.remove("agent");
        item.insert("worker".into(), worker);
    }
}

// The agent the work item saved with `worker` loads with.
fn agent_of(worker: Value) -> String {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), with_worker(worker));
    let state = store.load().unwrap().unwrap();
    state.work_items[0].agent.as_str().to_owned()
}

#[test]
fn the_default_worker_runs_on_sonnet_high() {
    let worker = json!({ "model": "claude-sonnet-5-5", "effort": "high" });
    assert_eq!(agent_of(worker), "sonnet-high");
}

#[test]
fn a_worker_label_for_opus_at_high_runs_on_opus_high() {
    let worker = json!({ "model": "claude-opus-5-5", "effort": "high" });
    assert_eq!(agent_of(worker), "opus-high");
}

#[test]
fn any_other_label_runs_on_the_agent_named_for_its_model_and_effort() {
    let cases = [
        ("claude-opus-5-5", "medium", "opus-medium"),
        ("claude-haiku-4-5-20251001", "low", "haiku-low"),
        ("claude-fable-5-1", "max", "fable-max"),
    ];
    for (model, effort, agent) in cases {
        let worker = json!({ "model": model, "effort": effort });
        assert_eq!(agent_of(worker), agent, "{model} at {effort}");
    }
}

#[test]
fn a_model_no_label_named_runs_on_the_agent_named_for_its_id() {
    let worker = json!({ "model": "claude-sonnet-6", "effort": "high" });
    assert_eq!(agent_of(worker), "claude-sonnet-6-high");
    let worker = json!({ "model": "GPT-5.1", "effort": "low" });
    assert_eq!(agent_of(worker), "gpt-5-1-low");
}

#[test]
fn an_item_given_to_the_local_worker_runs_on_the_agent_local() {
    let worker = json!({ "model": "qwen3.8:27b", "effort": "low", "local": true });
    assert_eq!(agent_of(worker), "local");
}

#[test]
fn the_file_saves_again_with_the_agent_and_without_the_worker() {
    let dir = tempfile::tempdir().unwrap();
    let worker = json!({ "model": "claude-opus-5-5", "effort": "high" });
    let store = saved_with(dir.path(), with_worker(worker));
    let state = store.load().unwrap().unwrap();
    assert_eq!(state.work_items, [a_work_item()]);
    store.save(&state).unwrap();
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("state.json")).unwrap()).unwrap();
    assert_eq!(saved["work_items"][0]["agent"], "opus-high");
    assert_eq!(saved["work_items"][0].get("worker"), None);
}

#[test]
fn a_version_one_files_work_item_runs_on_its_agent_too() {
    let dir = tempfile::tempdir().unwrap();
    let mut item = serde_json::to_value(a_work_item()).unwrap();
    let item_map = item.as_object_mut().unwrap();
    item_map.remove("agent");
    item_map.insert(
        "worker".into(),
        json!({ "model": "claude-sonnet-5-5", "effort": "high" }),
    );
    let old = json!({
        "version": 1, "run": "running", "since": 7, "work_item": item,
        "rulings": [], "leases": [],
    });
    fs::write(dir.path().join("state.json"), old.to_string()).unwrap();
    let state = store_in(dir.path()).load().unwrap().unwrap();
    assert_eq!(state.work_items[0].agent.as_str(), "sonnet-high");
}
