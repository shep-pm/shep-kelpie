//! A work item's review weight, which a version 95 file has none of

use std::fs;

use crate::state::tests::store_in;
use crate::state::*;
use crate::test::a_work_item;
use crate::work_item::{Lighter, ReviewWeight};

#[test]
fn a_version_95_work_item_loads_with_no_weight_and_keeps_one_once_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item());
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 95.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.work_items[0].weight, None);
    store.save(&loaded).unwrap();
    let saved = read_back(dir.path());
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["work_items"][0].get("weight"), None, "{saved}");

    let mut state = loaded;
    state.work_items[0].weight = Some(ReviewWeight::Light(Lighter::Docs));
    store.save(&state).unwrap();
    let saved = read_back(dir.path());
    assert_eq!(saved["work_items"][0]["weight"], serde_json::json!({"light": "docs"}));
    assert_eq!(store.load().unwrap(), Some(state));
}

fn read_back(dir: &std::path::Path) -> serde_json::Value {
    let text = fs::read_to_string(dir.join(STATE_FILE)).unwrap();
    serde_json::from_str(&text).unwrap()
}
