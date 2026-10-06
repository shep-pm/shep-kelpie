//! A state file saved while the deep round read, confirmed and re-checked:
//! its reviewer is `defect-hunter`, and its two reads are its two looks

use std::fs;

use serde_json::{Value, json};

use super::saved_with;
use crate::ports::Finding;
use crate::settings::AgentName;
use crate::state::{Fix, RulingKind, StateError, Stuck};
use crate::work_item::{Phase, Review, ReviewStage};

fn high() -> Value {
    json!({ "severity": "high", "file": "a.rs", "line": 3, "what": "racy", "why": "two writers" })
}

fn medium() -> Value {
    json!({ "severity": "medium", "file": "b.rs", "line": 1, "what": "unused", "why": "dead" })
}

// Both findings as the round held them: the HIGH backed by a pinned test,
// the MEDIUM found unfixed by a re-check.
fn held() -> Value {
    json!([
        {
            "finding": high(),
            "backing": {
                "backing": "test", "file": "tests/a.rs", "command": "cargo test a",
                "written": [{ "path": "tests/a.rs", "added": ["fn a() {}"] }],
            },
        },
        { "finding": medium(), "still": "it still panics" },
    ])
}

// The deep round at round 2, after qwen, at `step`.
fn deep_review(step: Value) -> Value {
    let mut stage = step;
    stage["stage"] = json!("deep");
    json!({ "round": 2, "stage": stage, "reviewer": "deep", "ran": ["qwen"] })
}

fn in_phase(review: Value) -> Value {
    let mut phase = review;
    phase["state"] = json!("review");
    phase
}

fn round_2(stage: ReviewStage) -> Review {
    let name = |n: &str| AgentName::try_from(n.to_owned()).unwrap();
    Review {
        round: 2,
        stage,
        reviewer: Some(name("defect-hunter")),
        ran: vec![name("qwen")],
        unread: false,
        ..Review::first()
    }
}

fn loaded_stage(step: Value) -> ReviewStage {
    let dir = tempfile::tempdir().unwrap();
    let phase = in_phase(deep_review(step));
    let store = saved_with(dir.path(), |value| value["work_items"][0]["phase"] = phase);
    let state = store.load().unwrap().unwrap();
    let Phase::Review(review) = state.work_items[0].phase.clone() else {
        panic!("{:?}", state.work_items[0].phase);
    };
    assert_eq!(review, round_2(review.stage.clone()));
    review.stage
}

fn findings(values: &[Value]) -> Vec<Finding> {
    values
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect()
}

fn not_sent() -> ReviewStage {
    ReviewStage::Found {
        findings: findings(&[high(), medium()]),
        threads: Vec::new(),
    }
}

fn done() -> ReviewStage {
    ReviewStage::Found {
        findings: Vec::new(),
        threads: Vec::new(),
    }
}

#[test]
fn the_two_reads_load_as_defect_hunters_two_looks() {
    assert_eq!(loaded_stage(json!({ "step": "read" })), ReviewStage::Round);
    assert_eq!(
        loaded_stage(json!({ "step": "missed", "first": [high()] })),
        ReviewStage::SecondLook {
            first: findings(&[high()])
        }
    );
}

#[test]
fn a_round_confirming_a_high_goes_to_the_fix_turn_with_every_finding() {
    let confirming = json!({
        "step": "confirming",
        "held": held(),
        "before": [{ "path": "tests/a.rs", "blob": "abc" }],
    });
    assert_eq!(loaded_stage(confirming), not_sent());
}

#[test]
fn findings_about_to_be_sent_the_first_time_go_to_the_fix_turn() {
    let sending = json!({ "step": "sending", "held": held(), "again": false });
    assert_eq!(loaded_stage(sending), not_sent());
}

#[test]
fn findings_about_to_be_sent_back_have_had_their_fix_and_the_pass_goes_on() {
    let sending = json!({ "step": "sending", "held": held(), "again": true });
    assert_eq!(loaded_stage(sending), done());
}

#[test]
fn a_fix_waiting_on_its_recheck_ends_the_round_and_the_pass_goes_on() {
    for again in [false, true] {
        let rechecking =
            json!({ "step": "rechecking", "held": held(), "head": "c0ffee", "again": again });
        assert_eq!(loaded_stage(rechecking), done(), "again: {again}");
    }
}

#[test]
fn a_fix_under_way_goes_on_and_must_still_move_the_head() {
    let fixing = json!({ "step": "fixing", "held": held(), "head": "c0ffee", "again": true });
    assert_eq!(
        loaded_stage(fixing),
        ReviewStage::Fixing {
            head: Some("c0ffee".into()),
            sent: Vec::new(),
            deferred_before: Vec::new(),
        }
    );
}

#[test]
fn a_fix_not_pushed_ruling_of_the_deep_round_loads_without_its_why() {
    let dir = tempfile::tempdir().unwrap();
    let fixing = json!({ "step": "fixing", "held": held(), "head": "c0ffee", "again": false });
    let store = saved_with(dir.path(), |value| {
        value["last_ruling"] = json!(3);
        value["rulings"] = json!([{
            "id": 3, "issue": 42, "question": "q", "pull_request": 51,
            "kind": {
                "kind": "fix-not-pushed",
                "review": deep_review(fixing),
                "prompt": "p",
                "why": "tests/a.rs is not in the pushed head",
            },
        }]);
    });
    let state = store.load().unwrap().unwrap();
    assert_eq!(
        state.rulings[0].kind,
        RulingKind::from(Stuck::FixNotPushed {
            fix: Fix::Review(round_2(ReviewStage::Fixing {
                head: Some("c0ffee".into()),
                sent: Vec::new(),
                deferred_before: Vec::new(),
            })),
            prompt: "p".into(),
        })
    );
    store.save(&state).unwrap();
    let saved = fs::read_to_string(dir.path().join("state.json")).unwrap();
    for gone in [
        "\"why\": \"tests",
        "held",
        "backing",
        "written",
        "still",
        "again",
    ] {
        assert!(!saved.contains(gone), "{gone} was saved again: {saved}");
    }
    assert_eq!(store.load().unwrap().unwrap(), state);
}

#[test]
fn a_round_saved_at_a_step_kept_by_a_resume_loads_the_same_way() {
    let dir = tempfile::tempdir().unwrap();
    let confirming = json!({ "step": "confirming", "held": held() });
    let store = saved_with(dir.path(), |value| {
        value["work_items"][0]["resume"] = in_phase(deep_review(confirming));
    });
    let state = store.load().unwrap().unwrap();
    assert_eq!(
        state.work_items[0].resume,
        Some(Phase::Review(round_2(not_sent())))
    );
}

#[test]
fn a_pending_deep_review_ruling_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let fixing = json!({ "step": "fixing", "held": held(), "head": "c0ffee", "again": true });
    let store = saved_with(dir.path(), |value| {
        value["last_ruling"] = json!(5);
        value["rulings"] = json!([{
            "id": 5, "issue": 42, "question": "q", "pull_request": 51,
            "kind": {
                "kind": "deep-review",
                "review": deep_review(fixing),
                "unfixed": ["a.rs:3 racy (still: it fails)"],
                "prompt": "p",
            },
        }]);
    });
    assert_eq!(
        store.load(),
        Err(StateError::RemovedRuling {
            path: dir.path().join("state.json"),
            id: 5,
            kind: "deep-review".into(),
        })
    );
}
