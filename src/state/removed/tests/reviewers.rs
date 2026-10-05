//! A state file saved before reviewers were agent files: the deep round's
//! and the Claude round's passes, time and calls, under the new names

use serde_json::json;

use super::saved_with;
use crate::settings::AgentName;
use crate::state::{Fix, RulingKind};
use crate::test::a_work_item;
use crate::work_item::{CallKind, Phase, Review, ReviewStage, TimingPhase};

fn name(n: &str) -> AgentName {
    AgentName::try_from(n.to_owned()).unwrap()
}

#[test]
fn a_pass_that_ran_the_deep_and_claude_rounds_names_defect_hunter_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        let item = &mut value["work_items"][0];
        item["phase"] = json!({
            "state": "review", "round": 4, "stage": { "stage": "round" },
            "reviewer": "claude", "ran": ["qwen", "deep", "claude"],
        });
        item["reviewers_skipped"] = json!(["claude", "deep", "gpu-box"]);
    });
    let state = store.load().unwrap().unwrap();
    let item = &state.work_items[0];
    assert_eq!(
        item.phase,
        Phase::Review(Review {
            round: 4,
            reviewer: None,
            ran: vec![name("qwen"), name("defect-hunter")],
            unread: false,
            ..Review::first()
        })
    );
    assert_eq!(
        item.reviewers_skipped,
        [name("defect-hunter"), name("gpu-box")],
        "a local reviewer keeps its name, as its agent file is named"
    );
}

#[test]
fn a_claude_round_s_last_reviewer_and_its_ruling_name_defect_hunter() {
    let dir = tempfile::tempdir().unwrap();
    let review = json!({
        "round": 2, "stage": { "stage": "fixing", "head": "c0ffee" },
        "reviewer": "claude", "last": "deep",
    });
    let store = saved_with(dir.path(), |value| {
        value["last_ruling"] = json!(3);
        value["rulings"] = json!([{
            "id": 3, "issue": 42, "question": "q", "pull_request": 51,
            "kind": { "kind": "fix-not-pushed", "review": review, "prompt": "p" },
        }]);
    });
    let state = store.load().unwrap().unwrap();
    assert_eq!(
        state.rulings[0].kind,
        RulingKind::FixNotPushed {
            fix: Fix::Review(Review {
                round: 2,
                stage: ReviewStage::Fixing {
                    head: Some("c0ffee".into()),
                    sent: Vec::new(),
                    deferred_before: Vec::new(),
                },
                reviewer: Some(name("defect-hunter")),
                last: Some(name("defect-hunter")),
                unread: false,
                ..Review::first()
            }),
            prompt: "p".into(),
        }
    );
}

#[test]
fn the_deep_rounds_time_counts_as_a_reviewers_session_and_the_totals_hold() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        let seconds = &mut value["work_items"][0]["timings"]["seconds"];
        seconds["ci"] = json!(1);
        seconds["deep_round"] = json!(2);
        let seconds = &mut value["history"][0]["seconds"];
        seconds["ci"] = json!(4);
        seconds["claude_round"] = json!(1);
        seconds["deep_round"] = json!(5);
    });
    let state = store.load().unwrap().unwrap();
    let open = &state.work_items[0].timings.as_ref().unwrap().seconds;
    assert_eq!(open.get(TimingPhase::ClaudeRound), 2);
    assert_eq!(open.total(), 7);
    let finished = &state.history[0].seconds;
    assert_eq!(finished.get(TimingPhase::ClaudeRound), 6);
    assert_eq!(finished.total(), state.history[0].wall);
}

#[test]
fn a_deep_read_in_flight_is_a_reviewers_session_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        let item = &mut value["work_items"][0];
        item["review_call"] = json!({ "state": "running", "since": 11 });
        item["timings"]["call"] = json!("deep");
    });
    let state = store.load().unwrap().unwrap();
    let timings = state.work_items[0].timings.as_ref().unwrap();
    assert_eq!(timings.call, Some(CallKind::Claude));
}

#[test]
fn a_deep_reviewers_calls_count_as_a_reviewers_and_the_spend_holds() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["work_items"][0]["calls"][0]["role"] = json!("deep_reviewer");
    });
    let state = store.load().unwrap().unwrap();
    let spend = state.work_items[0].spend();
    assert_eq!(spend.reviewer, a_work_item().spend().worker);
    assert_eq!(spend.worker.calls, 0);
}

#[test]
fn a_file_saved_since_keeps_an_agent_named_claude() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["version"] = json!(3);
        value["work_items"][0]["phase"] = json!({
            "state": "review", "round": 2, "stage": { "stage": "round" },
            "reviewer": "claude", "ran": ["deep"],
        });
    });
    let state = store.load().unwrap().unwrap();
    assert_eq!(
        state.work_items[0].phase,
        Phase::Review(Review {
            round: 2,
            reviewer: Some(name("claude")),
            ran: vec![name("deep")],
            unread: false,
            ..Review::first()
        })
    );
}

#[test]
fn a_pass_cut_short_in_claude_after_deep_does_not_run_defect_hunter_twice() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["work_items"][0]["phase"] = json!({
            "state": "review", "round": 3, "stage": { "stage": "round" },
            "reviewer": "claude", "ran": ["qwen", "deep"],
        });
    });
    let state = store.load().unwrap().unwrap();
    assert_eq!(
        state.work_items[0].phase,
        Phase::Review(Review {
            round: 3,
            reviewer: None,
            ran: vec![name("qwen"), name("defect-hunter")],
            unread: false,
            ..Review::first()
        }),
        "the next round goes to the first listed reviewer the pass has not run"
    );
}
