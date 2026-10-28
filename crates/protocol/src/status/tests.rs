use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::*;

/// The status a runner answered, recorded from `main`
///
/// Its temp folder, build stamp, session ids, lock holds and the head the rig
/// committed are recorded as markers, since no two runs make the same ones.
pub(super) const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/status.json");

pub(super) fn recorded() -> Value {
    let text = std::fs::read_to_string(FIXTURE).expect("the recorded status");
    serde_json::from_str(&text).expect("the recorded status is JSON")
}

/// The recorded status, read
pub(crate) fn recorded_status() -> Status {
    serde_json::from_value(recorded()).expect("the recorded status reads")
}

#[test]
fn the_recorded_status_round_trips() {
    let shown = recorded();
    let status: Status = serde_json::from_value(shown.clone()).expect("the status reads");
    assert_eq!(serde_json::to_value(&status).unwrap(), shown);
}

#[test]
fn empty_optional_fields_are_left_out() {
    let mut status: Status = serde_json::from_value(recorded()).expect("the status reads");
    status.contributions = Vec::new();
    status.bumps = Vec::new();
    status.triage = None;
    status.standing = Vec::new();
    status.red_workflows = Vec::new();
    status.pm_answers = BTreeMap::new();
    status.gpu = None;
    status.disk_free_bytes = None;
    status.local_leases = BTreeMap::new();
    status.pm = None;
    status.build = None;
    status.commit = None;
    status.draining = None;
    status.run = None;
    status.forge_held_until = None;
    status.settings_refused = None;
    status.lock = BTreeMap::new();

    let shown = serde_json::to_value(&status).unwrap();
    for key in [
        "contributions",
        "bumps",
        "triage",
        "standing",
        "red_workflows",
        "pm_answers",
        "gpu",
        "disk_free_bytes",
        "local_leases",
        "pm",
        "build",
        "commit",
        "draining",
        "run",
        "forge_held_until",
        "settings_refused",
        "lock",
    ] {
        assert_eq!(shown.get(key), None, "{key}: {shown}");
    }

    // A field with no skip rule is there even when it holds nothing.
    for key in [
        "adopted",
        "leases",
        "order",
        "skipped",
        "working",
        "waiting_for_slot",
    ] {
        assert_eq!(shown[key], json!([]), "{key}");
    }
    assert!(
        shown["work_item"].is_object(),
        "the status still names its first item"
    );
}

#[test]
fn a_ruling_shows_the_number_its_answer_names_it_by() {
    let status: Status = serde_json::from_value(recorded()).expect("the status reads");
    let ruling = &status.rulings[0];
    assert_eq!(ruling.id, ruling.ruling.number());
    let shown = serde_json::to_value(ruling).unwrap();
    assert_eq!(shown["id"], "71", "{shown}");
    assert_eq!(shown["pull_request"], 71);
    assert_eq!(
        ruling.ruling.issue.map(|t| t.to_string()).as_deref(),
        Some("7")
    );
}

#[test]
fn a_status_from_a_build_with_the_panel_still_reads() {
    let mut old = recorded();
    old["panel"] = json!({"decide": "panel", "voted": 3, "pm_alone": {"rebase": 2}});
    let status: Status = serde_json::from_value(old).expect("the old status reads");
    assert!(status.pm_answers.is_empty());
    let role: CallRole = serde_json::from_value(json!("panel")).unwrap();
    assert_eq!(role, CallRole::Attached("panel".into()));
}
