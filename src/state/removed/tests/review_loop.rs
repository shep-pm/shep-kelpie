//! A state file saved while the review loop and its judge existed

use std::fs;

use serde_json::{Value, json};

use super::saved_with;
use crate::ports::Finding;
use crate::settings::AgentName;
use crate::state::{Fix, RulingKind, StateError};
use crate::test::a_work_item;
use crate::work_item::{
    CallKind, CodeRabbitStage, OpenThread, Phase, Review, ReviewStage, TimingPhase,
};

fn named(name: &str) -> Option<AgentName> {
    Some(AgentName::try_from(name.to_owned()).unwrap())
}

fn racy() -> Value {
    json!({ "severity": "medium", "file": "a.rs", "line": 3, "what": "racy", "why": "two writers" })
}

fn nit() -> Value {
    json!({ "severity": "low", "file": "b.rs", "line": 1, "what": "unused", "why": "dead code" })
}

fn finding(value: Value) -> Finding {
    serde_json::from_value(value).unwrap()
}

// A review at round 3, between qwen's round and claude's, with the loop's
// streak, guard and lone-reviewer mark.
fn loop_review(stage: Value) -> Value {
    json!({
        "round": 3,
        "consecutive_clean": 1,
        "guard_cleared": true,
        "stage": stage,
        "last": "qwen",
        "alone": true,
    })
}

fn in_phase(review: Value) -> Value {
    let mut phase = review;
    phase["state"] = json!("review");
    phase
}

fn loaded_phase(phase: Value) -> Phase {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| value["work_items"][0]["phase"] = phase);
    let state = store.load().unwrap().unwrap();
    state.work_items[0].phase.clone()
}

#[test]
fn a_review_saved_between_rounds_goes_on_after_the_reviewer_it_recorded() {
    let phase = loaded_phase(in_phase(loop_review(json!({ "stage": "round" }))));
    assert_eq!(
        phase,
        Phase::Review(Review {
            round: 3,
            stage: ReviewStage::Round,
            reviewer: None,
            last: named("qwen"),
            unread: false,
            ..Review::first()
        })
    );
}

#[test]
fn a_round_waiting_on_the_judge_keeps_its_findings_without_verdicts() {
    let judging = json!({
        "stage": "judging",
        "findings": [racy(), nit()],
        "verdicts": [{ "holds": false, "severity": "low", "reason": "not so" }],
    });
    let phase = loaded_phase(in_phase(loop_review(judging)));
    let Phase::Review(review) = phase else {
        panic!("{phase:?}");
    };
    assert_eq!(
        review.stage,
        ReviewStage::Found {
            findings: vec![finding(racy()), finding(nit())],
        },
        "every finding the reviewer made, at its own severity"
    );
}

#[test]
fn a_fix_under_way_loses_its_streak_mark() {
    let fixing = json!({ "stage": "fixing", "clean": true, "head": "c0ffee" });
    let phase = loaded_phase(in_phase(loop_review(fixing)));
    let Phase::Review(review) = phase else {
        panic!("{phase:?}");
    };
    assert_eq!(
        review.stage,
        ReviewStage::Fixing {
            head: Some("c0ffee".into()),
            sent: Vec::new(),
            deferred_before: Vec::new(),
        }
    );
}

#[test]
fn a_bot_review_waiting_on_the_judge_keeps_its_threads_without_verdicts() {
    let phase = loaded_phase(json!({
        "state": "coderabbit",
        "stage": "judging",
        "bot": "cubic",
        "head": "c0ffee",
        "threads": [{ "id": "PRRT_1", "finding": racy() }],
        "verdicts": [{ "holds": true, "severity": "high", "reason": "real" }],
    }));
    assert_eq!(
        phase,
        Phase::CodeRabbit(CodeRabbitStage::Found {
            bot: crate::review_bot::Bot::Cubic,
            head: "c0ffee".into(),
            threads: vec![OpenThread {
                id: "PRRT_1".into(),
                finding: finding(racy()),
            }],
        })
    );
}

#[test]
fn a_review_kept_by_a_ruling_or_a_resume_loses_the_loops_fields_too() {
    let dir = tempfile::tempdir().unwrap();
    let fixing = json!({ "stage": "fixing", "clean": false, "head": "c0ffee" });
    let store = saved_with(dir.path(), |value| {
        value["work_items"][0]["resume"] = in_phase(loop_review(json!({ "stage": "round" })));
        value["last_ruling"] = json!(2);
        value["rulings"] = json!([
            {
                "id": 1, "issue": 42, "question": "q", "pull_request": 51,
                "kind": { "kind": "fix-not-pushed", "review": loop_review(fixing), "prompt": "p" },
            },
            {
                "id": 2, "issue": 42, "question": "q", "pull_request": 51,
                "kind": { "kind": "local-model-spilled", "review": loop_review(json!({ "stage": "round" })), "reason": "r" },
            },
        ]);
    });
    let state = store.load().unwrap().unwrap();
    let round_3 = |stage| Review {
        round: 3,
        stage,
        reviewer: None,
        last: named("qwen"),
        unread: false,
        ..Review::first()
    };
    assert_eq!(
        state.work_items[0].resume,
        Some(Phase::Review(round_3(ReviewStage::Round)))
    );
    assert_eq!(
        state.rulings[0].kind,
        RulingKind::FixNotPushed {
            fix: Fix::Review(round_3(ReviewStage::Fixing {
                head: Some("c0ffee".into()),
                sent: Vec::new(),
                deferred_before: Vec::new(),
            })),
            prompt: "p".into(),
        }
    );
    assert_eq!(
        state.rulings[1].kind,
        RulingKind::LocalModelSpilled {
            review: round_3(ReviewStage::Round),
            reason: "r".into(),
        }
    );
}

#[test]
fn the_judges_calls_count_as_a_reviewers_and_the_spend_holds() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["work_items"][0]["calls"][0]["role"] = json!("judge");
    });
    let state = store.load().unwrap().unwrap();
    let spend = state.work_items[0].spend();
    assert_eq!(spend.reviewer, a_work_item().spend().worker);
    assert_eq!(spend.worker.calls, 0);
}

#[test]
fn the_judges_time_counts_as_a_claude_rounds_and_the_totals_hold() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        let seconds = &mut value["work_items"][0]["timings"]["seconds"];
        seconds["ci"] = json!(1);
        seconds["judging"] = json!(2);
        let seconds = &mut value["history"][0]["seconds"];
        seconds["ci"] = json!(4);
        seconds["claude_round"] = json!(1);
        seconds["judging"] = json!(5);
    });
    let state = store.load().unwrap().unwrap();
    let open = &state.work_items[0].timings.as_ref().unwrap().seconds;
    assert_eq!(open.get(TimingPhase::ClaudeRound), 2);
    assert_eq!(
        open.total(),
        7,
        "the work item's seconds since it was created"
    );
    let finished = &state.history[0].seconds;
    assert_eq!(finished.get(TimingPhase::ClaudeRound), 6);
    assert_eq!(finished.total(), state.history[0].wall);
}

#[test]
fn a_judge_call_in_flight_counts_as_a_claude_round() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        let item = &mut value["work_items"][0];
        item["review_call"] = json!({ "state": "running", "since": 11 });
        item["timings"]["call"] = json!("judge");
    });
    let state = store.load().unwrap().unwrap();
    let timings = state.work_items[0].timings.as_ref().unwrap();
    assert_eq!(timings.call, Some(CallKind::Claude));
}

#[test]
fn a_work_items_count_of_local_rounds_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["work_items"][0]["local_rounds"] = json!(2);
    });
    assert_eq!(store.load().unwrap().unwrap().work_items, [a_work_item()]);
}

#[test]
fn a_file_with_all_of_the_loop_loads_and_saves_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        let judging = json!({ "stage": "judging", "findings": [racy()], "verdicts": [] });
        let item = &mut value["work_items"][0];
        item["phase"] = in_phase(loop_review(judging));
        item["local_rounds"] = json!(1);
        item["calls"][0]["role"] = json!("judge");
        item["timings"]["seconds"]["judging"] = json!(0);
        value["history"][0]["seconds"]["judging"] = json!(0);
    });
    let state = store.load().unwrap().unwrap();
    store.save(&state).unwrap();
    let saved = fs::read_to_string(dir.path().join("state.json")).unwrap();
    for gone in [
        "judg",
        "verdicts",
        "consecutive_clean",
        "guard_cleared",
        "alone",
        "local_rounds",
    ] {
        assert!(!saved.contains(gone), "{gone} was saved again: {saved}");
    }
    assert_eq!(store.load().unwrap().unwrap(), state);
}

#[test]
fn a_pending_round_guard_ruling_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["last_ruling"] = json!(4);
        value["rulings"] = json!([{
            "id": 4, "issue": 42, "question": "q", "pull_request": 51,
            "kind": { "kind": "review-guard", "review": loop_review(json!({ "stage": "round" })) },
        }]);
    });
    assert_eq!(
        store.load(),
        Err(StateError::RemovedRuling {
            path: dir.path().join("state.json"),
            id: 4,
            kind: "review-guard".into(),
        })
    );
}

// Nothing a bot fix saved names its threads: the findings sent carry none,
// so the fix resolves none and the next review sends them again.
#[test]
fn a_bot_fix_saved_with_the_loop_loads_with_no_threads_to_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        let item = &mut value["work_items"][0];
        item["phase"] = json!({ "state": "coderabbit", "stage": "fixing", "head": "c0ffee" });
        item["held"] = json!([racy()]);
    });
    let item = store.load().unwrap().unwrap().work_items[0].clone();
    assert_eq!(
        item.phase,
        Phase::CodeRabbit(CodeRabbitStage::Fixing {
            head: "c0ffee".into()
        })
    );
    assert_eq!(item.held, [finding(racy())]);
    assert!(item.threads_sent.is_empty());
}
