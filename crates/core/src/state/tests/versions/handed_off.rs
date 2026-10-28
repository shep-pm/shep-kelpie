//! The agents a work item left and its handoff note, which a version 91 file has none of

use std::fs;

use crate::settings::AgentName;
use crate::state::tests::store_in;
use crate::state::*;
use crate::test::a_work_item;

#[test]
fn a_version_91_work_item_loads_having_left_no_agent_and_each_is_kept_once_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item());
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 91.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    let item = &loaded.work_items[0];
    assert_eq!((&item.left, &item.handoff), (&Vec::new(), &None));
    store.save(&loaded).unwrap();
    let saved = read_back(dir.path());
    assert_eq!(saved["version"], VERSION);
    for field in ["left", "handoff"] {
        assert_eq!(saved["work_items"][0].get(field), None, "{saved}");
    }

    let mut state = loaded;
    state.work_items[0].left = vec![AgentName::kelpies("sonnet-high")];
    state.work_items[0].handoff = Some("a note".to_owned());
    store.save(&state).unwrap();
    let saved = read_back(dir.path());
    let item = &saved["work_items"][0];
    assert_eq!(
        (&item["left"], &item["handoff"]),
        (
            &serde_json::json!(["sonnet-high"]),
            &serde_json::json!("a note")
        )
    );
    assert_eq!(store.load().unwrap(), Some(state));
}

fn read_back(dir: &std::path::Path) -> serde_json::Value {
    let text = fs::read_to_string(dir.join(STATE_FILE)).unwrap();
    serde_json::from_str(&text).unwrap()
}
