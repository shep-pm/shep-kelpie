//! A work item's review plan, which a version 99 file has none of

use std::fs;
use std::num::NonZeroU32;

use crate::state::tests::store_in;
use crate::state::*;
use crate::test::a_work_item;
use crate::work_item::ReviewPlan;

#[test]
fn a_version_99_work_item_loads_with_no_plan_and_keeps_one_once_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item());
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 99.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.work_items[0].review_plan, None);
    store.save(&loaded).unwrap();
    let saved = read_back(dir.path());
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["work_items"][0].get("review_plan"), None, "{saved}");

    let mut state = loaded;
    state.work_items[0].review_plan = Some(ReviewPlan {
        reads: NonZeroU32::new(2).unwrap(),
        about: Some("the rollback path".to_owned()),
    });
    store.save(&state).unwrap();
    let saved = read_back(dir.path());
    assert_eq!(
        saved["work_items"][0]["review_plan"],
        serde_json::json!({"reads": 2, "about": "the rollback path"})
    );
    assert_eq!(store.load().unwrap(), Some(state));
}

fn read_back(dir: &std::path::Path) -> serde_json::Value {
    let text = fs::read_to_string(dir.join(STATE_FILE)).unwrap();
    serde_json::from_str(&text).unwrap()
}
