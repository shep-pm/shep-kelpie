//! The issue check's kept results in the state file

use super::*;

// A version 88 file has no `checks`, so every open issue is new to the check.
#[test]
fn a_version_88_file_loads_with_no_checks_and_keeps_those_it_gains() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 88.into();
    assert!(old.get("checks").is_none(), "{old}");
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded.checks, []);
    loaded.checks = vec![
        IssueCheck {
            issue: 4,
            key: 17,
            failures: Vec::new(),
            triage: CheckTriage::None,
            ready_seen: false,
        },
        IssueCheck {
            issue: 5,
            key: u64::MAX,
            failures: vec!["no priority label".to_owned()],
            triage: CheckTriage::Ran,
            ready_seen: false,
        },
    ];
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!(
        saved["checks"][0],
        serde_json::json!({ "issue": 4, "key": 17 })
    );
    assert_eq!(saved["checks"][1]["triage"], "ran");
    assert_eq!(store.load().unwrap().unwrap().checks, loaded.checks);
}

// A version 101 file kept no `ready_seen`, so a passed check is read as an
// issue that carried `ready-for-agent`, and a failed one is not.
#[test]
fn an_older_file_counts_each_passed_check_as_ready_seen() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 101.into();
    old["checks"] = serde_json::json!([
        { "issue": 4, "key": 17 },
        { "issue": 5, "key": 18, "failures": ["no priority label"], "triage": "owed" },
    ]);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");

    assert_eq!(
        loaded
            .checks
            .iter()
            .map(|c| c.ready_seen)
            .collect::<Vec<_>>(),
        [true, false]
    );
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["checks"][0]["ready_seen"], true);
    assert!(saved["checks"][1].get("ready_seen").is_none(), "{saved}");
    assert_eq!(store.load().unwrap().unwrap().checks, loaded.checks);
}
