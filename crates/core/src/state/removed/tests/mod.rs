use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::ports::Timestamp;
use crate::state::{Finished, ProjectState, STATE_FILE, StateError, StateStore};
use crate::test::{a_work_item, a_work_item_from_before_comments, ticket};
use crate::work_item::{CallKind, Seconds, TimingPhase};

mod bot_rounds;
mod deep_round;
mod retro;
mod review_loop;
mod reviewers;
mod ruling_kinds;
mod shots;
mod timings;
mod unlabelled;
mod worker;

fn store_in(dir: &Path) -> StateStore {
    StateStore::new(dir.join(STATE_FILE))
}

// One open work item and one finished, as this build saves them in a
// version 2 file, then changed by `old` into what an older build saved.
fn saved_with(dir: &Path, old: impl FnOnce(&mut Value)) -> StateStore {
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item_from_before_comments());
    state.record_finished(Finished {
        issue: ticket(9),
        title: "Fix a thing".into(),
        pull_request: Some(91),
        merged: true,
        closed: false,
        at: Timestamp(100),
        wall: 50,
        seconds: Seconds::of(&[(TimingPhase::Worker, 40), (TimingPhase::Ci, 10)]),
        spend: None,
    });
    let mut value = serde_json::to_value(&state).unwrap();
    value["version"] = json!(2);
    old(&mut value);
    fs::write(dir.join(STATE_FILE), value.to_string()).unwrap();
    store_in(dir)
}

fn with_audit_state(value: &mut Value) {
    value["work_items"][0]["audit"] = json!({
        "passed": { "head": "abc", "inputs": 7 },
        "sent_back": 2,
    });
}

// The work item's 7 seconds and the finished one's 50, partly the check's.
fn with_audit_time(value: &mut Value) {
    let seconds = &mut value["work_items"][0]["timings"]["seconds"];
    seconds["ci"] = json!(1);
    seconds["audit"] = json!(2);
    let seconds = &mut value["history"][0]["seconds"];
    seconds["ci"] = json!(4);
    seconds["claude_round"] = json!(1);
    seconds["audit"] = json!(5);
}

fn with_audit_in_flight(value: &mut Value) {
    let item = &mut value["work_items"][0];
    item["review_call"] = json!({ "state": "running", "since": 11 });
    item["timings"]["call"] = json!("audit");
}

fn with_auditor_call(value: &mut Value) {
    value["work_items"][0]["calls"][0]["role"] = json!("auditor");
}

#[test]
fn a_work_items_whole_issue_check_state_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let state = saved_with(dir.path(), with_audit_state)
        .load()
        .unwrap()
        .unwrap();
    assert_eq!(state.work_items, [a_work_item_from_before_comments()]);
}

#[test]
fn the_checks_time_counts_as_a_claude_rounds_and_the_totals_hold() {
    let dir = tempfile::tempdir().unwrap();
    let state = saved_with(dir.path(), with_audit_time)
        .load()
        .unwrap()
        .unwrap();
    let open = &state.work_items[0].timings.as_ref().unwrap().seconds;
    assert_eq!(open.get(TimingPhase::Review), 2);
    assert_eq!(
        open.total(),
        7,
        "the work item's seconds since it was created"
    );
    let finished = &state.history[0].seconds;
    assert_eq!(finished.get(TimingPhase::Review), 6);
    assert_eq!(finished.total(), state.history[0].wall);
}

#[test]
fn a_check_in_flight_counts_as_a_claude_round() {
    let dir = tempfile::tempdir().unwrap();
    let state = saved_with(dir.path(), with_audit_in_flight)
        .load()
        .unwrap()
        .unwrap();
    let timings = state.work_items[0].timings.as_ref().unwrap();
    assert_eq!(timings.call, Some(CallKind::Claude));
}

#[test]
fn an_auditors_calls_count_as_a_reviewers_and_the_spend_holds() {
    let dir = tempfile::tempdir().unwrap();
    let state = saved_with(dir.path(), with_auditor_call)
        .load()
        .unwrap()
        .unwrap();
    let spend = state.work_items[0].spend();
    assert_eq!(spend.reviewer, a_work_item().spend().worker);
    assert_eq!(spend.worker.calls, 0);
}

#[test]
fn a_file_with_all_of_them_loads_and_saves_without_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        with_audit_state(value);
        with_audit_time(value);
        with_audit_in_flight(value);
        with_auditor_call(value);
    });
    let state = store.load().unwrap().unwrap();
    store.save(&state).unwrap();
    let saved = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    assert!(
        !saved.contains("audit"),
        "the check was saved again: {saved}"
    );
    assert_eq!(store.load().unwrap().unwrap(), state);
}

#[test]
fn a_pending_whole_issue_check_ruling_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["last_ruling"] = json!(4);
        value["rulings"] = json!([{
            "id": 4, "issue": 42, "question": "q", "pull_request": 51,
            "kind": { "kind": "audit", "head": "abc", "gaps": ["g"], "prompt": "p" },
        }]);
    });
    assert_eq!(
        store.load(),
        Err(StateError::RemovedRuling {
            path: dir.path().join(STATE_FILE),
            id: 4,
            kind: "audit".into(),
        })
    );
}

#[test]
fn a_pending_local_model_spilled_ruling_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["last_ruling"] = json!(5);
        value["rulings"] = json!([{
            "id": 5, "issue": 42, "question": "q", "pull_request": 51,
            "kind": { "kind": "local-model-spilled", "review": {}, "reason": "qwen at 60%" },
        }]);
    });
    assert_eq!(
        store.load(),
        Err(StateError::RemovedRuling {
            path: dir.path().join(STATE_FILE),
            id: 5,
            kind: "local-model-spilled".into(),
        })
    );
}

// The folded shape the previous build saved: a `stuck` ruling with the kind
// as its reason.
#[test]
fn a_pending_stuck_ruling_for_a_spilled_model_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["last_ruling"] = json!(6);
        value["rulings"] = json!([{
            "id": 6, "issue": 42, "question": "q", "pull_request": 51,
            "kind": {
                "kind": "stuck", "reason": "local-model-spilled",
                "review": {}, "why": "qwen at 60%",
            },
        }]);
    });
    assert_eq!(
        store.load(),
        Err(StateError::RemovedRuling {
            path: dir.path().join(STATE_FILE),
            id: 6,
            kind: "local-model-spilled".into(),
        })
    );
}

// A file saved while the relay existed, with its count of clears and each
// ruling's relayed and resend fields.
#[test]
fn a_state_saved_while_the_relay_existed_loads_and_saves_without_its_fields() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let old = r#"{"version":2,"run":"running","since":7,"work_item":null,
        "relay_clears":4,
        "rulings":[{"id":1,"question":"q","pull_request":3,"kind":{"kind":"closed"},
                    "alerted":true,"relayed":true,"resend":true}],
        "leases":[]}"#;
    fs::write(dir.path().join(STATE_FILE), old).unwrap();
    let state = store.load().unwrap().unwrap();
    assert!(state.rulings[0].alerted);

    store.save(&state).unwrap();
    let saved = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    for gone in ["relay_clears", "relayed", "resend"] {
        assert!(!saved.contains(gone), "{gone} was saved again: {saved}");
    }
}

// A file saved while the planning call existed, with a plan under way.
#[test]
fn a_state_saved_with_plans_loads_and_saves_without_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let old = r#"{"version":2,"run":"running","since":7,"work_item":null,
        "plans":[{"issue":5,"stage":{"kind":"closing","failures":1}}],
        "rulings":[{"id":1,"question":"q","pull_request":3,"kind":{"kind":"closed"}}],
        "leases":[]}"#;
    fs::write(dir.path().join(STATE_FILE), old).unwrap();
    let state = store.load().unwrap().unwrap();
    assert_eq!(state.rulings.len(), 1);

    store.save(&state).unwrap();
    let saved = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    assert!(!saved.contains("plans"), "plans were saved again: {saved}");
}

#[test]
fn a_pending_ruling_of_a_removed_kind_is_refused_by_id_and_kind() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let kinds = [
        r#"{"kind":"split","why":"w","pieces":[]}"#,
        r#"{"kind":"split-stuck","reason":"r","opened":[]}"#,
        r#"{"kind":"close-stuck","reason":"r"}"#,
    ];
    for (kind, name) in kinds.iter().zip(["split", "split-stuck", "close-stuck"]) {
        let old = format!(
            r#"{{"version":2,"run":"running","since":7,"work_item":null,"last_ruling":4,
            "rulings":[{{"id":3,"question":"q","pull_request":null,"kind":{{"kind":"closed"}}}},
                       {{"id":4,"issue":5,"question":"q","pull_request":null,"kind":{kind}}}],
            "leases":[]}}"#
        );
        fs::write(dir.path().join(STATE_FILE), old).unwrap();
        let err = store.load().unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "state file {} holds ruling 4 of kind `{name}`, which this kelpie no longer \
                 has: answer or drop it on the old build first",
                dir.path().join(STATE_FILE).display()
            )
        );
    }
}

#[test]
fn other_unknown_fields_are_still_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let old = r#"{"version":2,"run":"running","since":7,"work_item":null,
        "rulings":[{"id":1,"question":"q","pull_request":3,"kind":{"kind":"closed"},
                    "relayed":false,"pigeon":true}],
        "leases":[]}"#;
    fs::write(dir.path().join(STATE_FILE), old).unwrap();
    let err = store.load().unwrap_err();
    assert!(err.to_string().contains("pigeon"), "{err}");
}

#[test]
fn a_file_saved_with_a_day_start_loads_and_drops_it() {
    for key in ["pacing", "codex_pacing"] {
        let dir = tempfile::tempdir().unwrap();
        let store = saved_with(dir.path(), |value| {
            value[key] = json!({
                "week_resets_at": 604_800,
                "day": 2,
                "week_used_pct": 15,
            });
        });
        let state = store.load().unwrap().unwrap();
        store.save(&state).unwrap();
        let saved = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        assert!(!saved.contains(key), "{key}: {saved}");
        assert_eq!(store.load().unwrap().unwrap(), state, "{key}");
    }
}

#[test]
fn a_saved_file_holds_no_day_start() {
    let value = serde_json::to_value(ProjectState::new()).unwrap();
    assert_eq!(value.get("pacing"), None);
    assert_eq!(value.get("codex_pacing"), None);
}
