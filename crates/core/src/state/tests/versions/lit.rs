//! A work item's light tag, which a version 96 file has none of

use std::fs;

use crate::state::tests::store_in;
use crate::state::*;
use crate::test::a_work_item;

#[test]
fn an_older_work_item_loads_untagged_and_keeps_the_tag_once_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item());
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 96.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert!(!loaded.work_items[0].pm_light);
    store.save(&loaded).unwrap();
    let saved = read_back(dir.path());
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["work_items"][0].get("pm_light"), None, "{saved}");

    let mut state = loaded;
    state.work_items[0].pm_light = true;
    store.save(&state).unwrap();
    let saved = read_back(dir.path());
    assert_eq!(saved["work_items"][0]["pm_light"], true);
    assert_eq!(store.load().unwrap(), Some(state));
}

fn read_back(dir: &std::path::Path) -> serde_json::Value {
    let text = fs::read_to_string(dir.join(STATE_FILE)).unwrap();
    serde_json::from_str(&text).unwrap()
}
