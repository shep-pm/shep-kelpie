//! A state file saved while rulings had 13 kinds

use serde_json::{Value, json};

use super::saved_with;
use crate::state::{Fix, ProjectState, Ruling, RulingKind, Stuck};
use crate::work_item::{Phase, Review, ReviewStage, Turn};

// A version 6 file whose one pending ruling is of the old `kind`, parking
// the work item, as a version 6 kelpie saved it.
fn loaded_with(kind: Value) -> ProjectState {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["version"] = json!(6);
        value["last_ruling"] = json!(3);
        value["work_items"][0]["phase"] = json!({ "state": "ruling", "id": 3 });
        value["rulings"] = json!([{
            "id": 3,
            "issue": 7,
            "question": "The question as it was asked",
            "pull_request": 51,
            "kind": kind,
            "alerted": true,
        }]);
    });
    store.load().unwrap().unwrap()
}

fn ruling(state: &ProjectState) -> &Ruling {
    let [ruling] = state.rulings.as_slice() else {
        panic!("{:?}", state.rulings);
    };
    assert_eq!(ruling.question, "The question as it was asked");
    assert_eq!((ruling.id, ruling.pull_request), (3, Some(51)));
    assert_eq!(state.work_items[0].phase, Phase::Ruling { id: 3 });
    ruling
}

fn loads_as(old: Value, reason: Stuck) {
    let name = old["kind"].clone();
    let state = loaded_with(old);
    assert_eq!(ruling(&state).kind, RulingKind::from(reason), "{name}");
}

fn round_2() -> Review {
    Review {
        round: 2,
        stage: ReviewStage::Fixing {
            head: Some("c0ffee".into()),
            sent: Vec::new(),
            deferred_before: Vec::new(),
        },
        ..Review::first()
    }
}

#[test]
fn a_rebase_ruling_is_stuck_on_a_rebase() {
    loads_as(
        json!({ "kind": "rebase", "reason": "conflict in a.txt" }),
        Stuck::Rebase {
            why: "conflict in a.txt".into(),
        },
    );
}

#[test]
fn a_still_red_ruling_is_stuck_on_a_red_run() {
    loads_as(
        json!({ "kind": "still-red", "head": "bad", "checks": ["lint"] }),
        Stuck::StillRed {
            head: "bad".into(),
            checks: vec!["lint".into()],
        },
    );
}

#[test]
fn a_merge_refused_ruling_is_stuck_on_the_refusal() {
    loads_as(
        json!({ "kind": "merge-refused", "head": "c0ffee", "reason": "a ruleset" }),
        Stuck::MergeRefused {
            head: "c0ffee".into(),
            why: "a ruleset".into(),
        },
    );
}

#[test]
fn a_closed_ruling_is_stuck_on_the_close() {
    loads_as(json!({ "kind": "closed" }), Stuck::Closed);
}

#[test]
fn a_local_model_spilled_ruling_is_stuck_on_the_spill() {
    let review = serde_json::to_value(round_2()).unwrap();
    loads_as(
        json!({ "kind": "local-model-spilled", "review": review, "reason": "qwen at 60%" }),
        Stuck::LocalModelSpilled {
            review: round_2(),
            why: "qwen at 60%".into(),
        },
    );
}

#[test]
fn a_fix_not_pushed_ruling_is_stuck_on_the_fix() {
    let review = serde_json::to_value(round_2()).unwrap();
    loads_as(
        json!({ "kind": "fix-not-pushed", "review": review, "prompt": "again" }),
        Stuck::FixNotPushed {
            fix: Fix::Review(round_2()),
            prompt: "again".into(),
        },
    );
}

#[test]
fn a_turn_timeout_ruling_is_stuck_on_the_timeout() {
    loads_as(
        json!({ "kind": "turn-timeout", "phase": { "state": "implement" } }),
        Stuck::TurnTimeout {
            phase: Some(Phase::Implement),
        },
    );
    loads_as(
        json!({ "kind": "turn-timeout" }),
        Stuck::TurnTimeout { phase: None },
    );
}

#[test]
fn a_turn_failed_ruling_is_stuck_on_the_failure() {
    loads_as(
        json!({
            "kind": "turn-failed",
            "reason": "no worktree",
            "phase": { "state": "implement" },
            "retry": { "state": "due" },
        }),
        Stuck::TurnFailed {
            why: "no worktree".into(),
            phase: Phase::Implement,
            retry: Turn::Due,
        },
    );
}

#[test]
fn a_claude_files_ruling_is_an_agent_files_one() {
    let state = loaded_with(json!({
        "kind": "claude-files",
        "head": "c0ffee",
        "files": [".mcp.json"],
        "phase": { "state": "implement" },
    }));
    let kind = RulingKind::AgentFiles {
        head: "c0ffee".into(),
        files: vec![".mcp.json".into()],
        phase: Phase::Implement,
    };
    assert_eq!(ruling(&state).kind, kind);
}

#[test]
fn a_follow_up_ruling_loads_unchanged() {
    let old = json!({
        "kind": "follow-up",
        "findings": [{
            "file": "src/a.rs",
            "line": 3,
            "severity": "high",
            "what": "racy",
            "why": "two writers",
        }],
    });
    let state = loaded_with(old.clone());
    let kind = &ruling(&state).kind;
    assert!(matches!(kind, RulingKind::FollowUp { .. }), "{kind:?}");
    assert_eq!(serde_json::to_value(kind).unwrap(), old);
}

// A `stuck` ruling this kelpie saved is left as it is.
#[test]
fn a_ruling_saved_by_this_kelpie_loads_as_it_was_saved() {
    let saved = json!({ "kind": "stuck", "reason": "rebase", "why": "conflict" });
    let state = loaded_with(saved);
    let reason = Stuck::Rebase {
        why: "conflict".into(),
    };
    assert_eq!(ruling(&state).kind, RulingKind::from(reason));
}
