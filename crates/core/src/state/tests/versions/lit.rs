//! A work item's light tag, which nothing writes and nothing reads

use std::fs;

use crate::state::tests::store_in;
use crate::state::*;
use crate::test::a_work_item;

// A version 97 file has `pm_light` on its work item. The tag is dropped on the
// load, so the file saves again with none and loads as the work item stands.
#[test]
fn a_saved_light_tag_is_dropped_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut item = serde_json::to_value(a_work_item()).unwrap();
    assert_eq!(item.get("pm_light"), None, "nothing writes the tag: {item}");
    item["pm_light"] = serde_json::json!(true);
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 97.into();
    old["work_items"] = serde_json::json!([item]);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.work_items, [a_work_item()]);
    store.save(&loaded).unwrap();
    let saved = read_back(dir.path());
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["work_items"][0].get("pm_light"), None, "{saved}");
    assert_eq!(store.load().unwrap(), Some(loaded));
}

#[test]
fn a_work_item_saves_with_no_light_tag() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item());

    store.save(&state).unwrap();

    let saved = read_back(dir.path());
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["work_items"][0].get("pm_light"), None, "{saved}");
    assert_eq!(store.load().unwrap(), Some(state));
}

fn read_back(dir: &std::path::Path) -> serde_json::Value {
    let text = fs::read_to_string(dir.join(STATE_FILE)).unwrap();
    serde_json::from_str(&text).unwrap()
}
