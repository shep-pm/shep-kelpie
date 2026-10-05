//! A state file saved while kelpie took shots of a work item's UI

use std::fs;

use serde_json::{Value, json};

use super::saved_with;
use crate::ports::Timestamp;
use crate::state::{Notice, RulingKind};
use crate::test::a_work_item;
use crate::work_item::{CallKind, ReviewCallState, TimingPhase};

// The last run, as kelpie kept it, and the pull request's shots comment.
fn with_shots_record(value: &mut Value) {
    let item = &mut value["work_items"][0];
    item["shots"] = json!({
        "head": "c0ffee",
        "run": {
            "shots": [{
                "route": "/",
                "viewport": "mobile",
                "scheme": "dark",
                "file": "/k/koji/shots/7/c0ffee/root-mobile-dark.png",
                "status": 200,
                "problems": [],
            }],
            "problems": [],
            "failed": null,
        },
        "posted": false,
        "retry_at": 60,
    });
    item["shots_comment"] = json!(9000);
}

// The work item's 7 seconds and the finished one's 50, partly the shots'.
fn with_shots_time(value: &mut Value) {
    let seconds = &mut value["work_items"][0]["timings"]["seconds"];
    seconds["worker"] = json!(1);
    seconds["shots"] = json!(3);
    let seconds = &mut value["history"][0]["seconds"];
    seconds["worker"] = json!(30);
    seconds["shots"] = json!(8);
    seconds["other"] = json!(2);
}

fn with_shots_in_flight(value: &mut Value) {
    let item = &mut value["work_items"][0];
    item["review_call"] = json!({ "state": "running", "since": 11 });
    item["timings"]["call"] = json!("shots");
}

// A merge ruling and a notice whose head's shots failed.
fn with_shots_failed(value: &mut Value) {
    value["last_ruling"] = json!(4);
    value["rulings"] = json!([{
        "id": 4, "issue": 42, "question": "q", "pull_request": 51,
        "kind": { "kind": "merge", "head": "c0ffee", "shots_failed": true },
    }]);
    value["notices"] = json!([{
        "issue": 9, "pull_request": 91, "head": "beef", "shots_failed": true,
    }]);
}

#[test]
fn a_work_items_shots_and_their_comment_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let state = saved_with(dir.path(), with_shots_record)
        .load()
        .unwrap()
        .unwrap();
    assert_eq!(state.work_items, [a_work_item()]);
}

#[test]
fn the_shots_time_counts_as_other_and_the_totals_hold() {
    let dir = tempfile::tempdir().unwrap();
    let state = saved_with(dir.path(), with_shots_time)
        .load()
        .unwrap()
        .unwrap();
    let open = &state.work_items[0].timings.as_ref().unwrap().seconds;
    assert_eq!(open.get(TimingPhase::Other), 3);
    assert_eq!(
        open.total(),
        7,
        "the work item's seconds since it was created"
    );
    let finished = &state.history[0].seconds;
    assert_eq!(finished.get(TimingPhase::Other), 10);
    assert_eq!(finished.total(), state.history[0].wall);
}

#[test]
fn a_shots_run_in_flight_ends_and_its_time_goes_by_the_phase() {
    let dir = tempfile::tempdir().unwrap();
    let state = saved_with(dir.path(), with_shots_in_flight)
        .load()
        .unwrap()
        .unwrap();
    let item = &state.work_items[0];
    assert_eq!(item.review_call, ReviewCallState::Idle);
    assert_eq!(item.timings.as_ref().unwrap().call, None);
}

#[test]
fn a_claude_round_in_flight_is_left_running() {
    let dir = tempfile::tempdir().unwrap();
    let state = saved_with(dir.path(), |value| {
        with_shots_in_flight(value);
        value["work_items"][0]["timings"]["call"] = json!("claude");
    })
    .load()
    .unwrap()
    .unwrap();
    let item = &state.work_items[0];
    assert_eq!(
        item.review_call,
        ReviewCallState::Running {
            since: Timestamp(11)
        }
    );
    assert_eq!(item.timings.as_ref().unwrap().call, Some(CallKind::Claude));
}

#[test]
fn a_merge_ruling_and_a_notice_lose_whether_the_shots_failed() {
    let dir = tempfile::tempdir().unwrap();
    let state = saved_with(dir.path(), with_shots_failed)
        .load()
        .unwrap()
        .unwrap();
    assert_eq!(
        state.rulings[0].kind,
        RulingKind::Merge {
            head: "c0ffee".into(),
            unreviewed: None,
        }
    );
    assert_eq!(
        state.notices,
        [Notice {
            issue: 9,
            pull_request: 91,
            head: "beef".into(),
        }]
    );
}

#[test]
fn a_file_with_all_of_them_loads_and_saves_without_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        with_shots_record(value);
        with_shots_time(value);
        with_shots_in_flight(value);
        with_shots_failed(value);
    });
    let state = store.load().unwrap().unwrap();
    store.save(&state).unwrap();
    let saved = fs::read_to_string(dir.path().join("state.json")).unwrap();
    assert!(
        !saved.contains("shots"),
        "the shots were saved again: {saved}"
    );
    assert_eq!(store.load().unwrap().unwrap(), state);
}
