use std::fs;

use super::super::*;
use super::store_in;
use crate::ports::Timestamp;

// A version 59 file has no `ruled` on a contribution, so it loads with no
// answer to the merge ruling; one is kept once set, and left out while it is
// not.
#[test]
fn an_older_state_file_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 59.into();
    old["contributions"] = serde_json::json!([
        { "pull_request": 12, "author": "newcomer", "head": "2a8f1e6", "seen": 1 }
    ]);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.contributions[0].ruled, None);
    store.save(&loaded).unwrap();
    let saved = read_back(dir.path());
    assert_eq!(saved["version"], VERSION);
    assert_eq!(saved["contributions"][0].get("ruled"), None);

    let mut state = loaded;
    state.contributions[0].ruled = Some(Ruled {
        head: "2a8f1e6".into(),
        verdict: Verdict::No {
            note: Some("Please add a test.".into()),
        },
    });
    store.save(&state).unwrap();
    let saved = read_back(dir.path());
    assert_eq!(
        saved["contributions"][0]["ruled"],
        serde_json::json!({ "head": "2a8f1e6", "verdict": { "no": { "note": "Please add a test." } } })
    );
    assert_eq!(store.load().unwrap(), Some(state));
}

#[test]
fn the_merge_ruling_is_saved_by_its_reason() {
    let kind = RulingKind::Contribution(Held::Merge {
        head: "2a8f1e6".into(),
        rework_off: false,
    });
    let value = serde_json::to_value(&kind).unwrap();
    assert_eq!(
        value,
        serde_json::json!({ "kind": "contribution", "reason": "merge", "head": "2a8f1e6" })
    );
    assert_eq!(serde_json::from_value::<RulingKind>(value).unwrap(), kind);
}

#[test]
fn a_merge_ruling_saved_without_rework_loads_and_one_saved_before_it_loads_with_rework() {
    let off = RulingKind::Contribution(Held::Merge {
        head: "2a8f1e6".into(),
        rework_off: true,
    });
    let value = serde_json::to_value(&off).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "kind": "contribution", "reason": "merge", "head": "2a8f1e6", "rework_off": true
        })
    );
    assert_eq!(serde_json::from_value::<RulingKind>(value).unwrap(), off);

    let before =
        serde_json::json!({ "kind": "contribution", "reason": "merge", "head": "2a8f1e6" });
    assert_eq!(
        serde_json::from_value::<RulingKind>(before).unwrap(),
        RulingKind::Contribution(Held::Merge {
            head: "2a8f1e6".into(),
            rework_off: false,
        })
    );
}

// A push is a new head: the answer to the ruling on the old one is gone
#[test]
fn a_push_drops_the_answer_to_the_merge_ruling() {
    let mut held = Contribution::new(12, "newcomer".into(), "2a8f1e6".into(), Timestamp(1));
    held.ruled = Some(Ruled {
        head: "2a8f1e6".into(),
        verdict: Verdict::Yes {
            merged: false,
            thanked: false,
        },
    });

    let pushed = held.pushed("9c0d4b3".into(), Timestamp(5));

    assert_eq!(pushed.ruled, None);
}

fn read_back(dir: &std::path::Path) -> serde_json::Value {
    let text = fs::read_to_string(dir.join(STATE_FILE)).unwrap();
    serde_json::from_str(&text).unwrap()
}
