//! A work item's kind, tier and slack, which a version 90 file has none of

use std::fs;

use crate::agents::Tier;
use crate::state::tests::store_in;
use crate::state::*;
use crate::test::a_work_item;

#[test]
fn a_version_90_work_item_loads_with_no_kind_tier_or_slack_and_each_is_kept_once_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item());
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 90.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    let item = &loaded.work_items[0];
    assert_eq!((&item.kind, item.tier, item.slack), (&None, None, None));
    store.save(&loaded).unwrap();
    let saved = read_back(dir.path());
    assert_eq!(saved["version"], VERSION);
    for field in ["kind", "tier", "slack"] {
        assert_eq!(saved["work_items"][0].get(field), None, "{saved}");
    }

    let mut state = loaded;
    state.work_items[0].kind = Some("fix".into());
    state.work_items[0].tier = Some(Tier::Heavy);
    state.work_items[0].slack = Some(0);
    store.save(&state).unwrap();
    let saved = read_back(dir.path());
    let item = &saved["work_items"][0];
    assert_eq!(
        (&item["kind"], &item["tier"], &item["slack"]),
        (
            &serde_json::json!("fix"),
            &serde_json::json!("heavy"),
            &serde_json::json!(0)
        )
    );
    assert_eq!(store.load().unwrap(), Some(state));
}

fn read_back(dir: &std::path::Path) -> serde_json::Value {
    let text = fs::read_to_string(dir.join(STATE_FILE)).unwrap();
    serde_json::from_str(&text).unwrap()
}
