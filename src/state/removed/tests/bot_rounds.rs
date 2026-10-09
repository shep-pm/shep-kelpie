//! A state file saved while review bots ran a round of their own after CI

use serde_json::{Value, json};

use super::saved_with;
use crate::ports::{Finding, Timestamp};
use crate::review_bot::Bot;
use crate::settings::AgentName;
use crate::state::{Fix, ProjectState, Resume, RulingKind, Stuck};
use crate::work_item::{Phase, Review, ReviewStage};

fn racy() -> Value {
    json!({ "severity": "medium", "file": "a.rs", "line": 3, "what": "racy", "why": "two writers" })
}

fn finding(value: Value) -> Finding {
    serde_json::from_value(value).unwrap()
}

// A version 3 file, the last to keep bot rounds, changed by `old`.
fn loaded(old: impl FnOnce(&mut Value)) -> ProjectState {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["version"] = json!(3);
        old(value);
    });
    store.load().unwrap().unwrap()
}

fn loaded_phase(phase: Value) -> Phase {
    let state = loaded(|value| value["work_items"][0]["phase"] = phase);
    state.work_items[0].phase.clone()
}

// A pass of the bots alone at `stage`, held by `reviewer`'s round.
fn bots_pass(stage: ReviewStage, reviewer: Option<&str>) -> Phase {
    Phase::Review(Review {
        stage,
        reviewer: reviewer.map(|name| AgentName::try_from(name.to_owned()).unwrap()),
        bots_only: true,
        unread: false,
        ..Review::first()
    })
}

fn fixing(head: Option<&str>) -> ReviewStage {
    ReviewStage::Fixing {
        head: head.map(str::to_owned),
        sent: Vec::new(),
        deferred_before: Vec::new(),
    }
}

#[test]
fn a_round_waiting_for_the_lease_starts_a_pass_of_the_bots() {
    let phase = loaded_phase(json!({
        "state": "coderabbit", "stage": "lease", "head": "c0ffee", "readied": 5, "full": true,
    }));
    assert_eq!(phase, bots_pass(ReviewStage::Round, None));
}

#[test]
fn a_summon_waits_on_as_its_bots_round() {
    let phase = loaded_phase(json!({
        "state": "coderabbit", "stage": "summoned", "head": "c0ffee", "at": 12,
    }));
    let stage = ReviewStage::Summoned {
        bot: Bot::Coderabbit,
        started: Timestamp(12),
        head: "c0ffee".into(),
        at: Timestamp(12),
        full: false,
        resent: false,
    };
    assert_eq!(phase, bots_pass(stage, Some("coderabbit")));

    let phase = loaded_phase(json!({
        "state": "coderabbit", "stage": "summoned", "bot": "cubic", "head": "c0ffee",
        "at": 12, "full": true, "resent": true,
    }));
    let stage = ReviewStage::Summoned {
        bot: Bot::Cubic,
        started: Timestamp(12),
        head: "c0ffee".into(),
        at: Timestamp(12),
        full: true,
        resent: true,
    };
    assert_eq!(phase, bots_pass(stage, Some("cubic")));
}

#[test]
fn threads_found_are_the_rounds_findings_with_their_ids() {
    let phase = loaded_phase(json!({
        "state": "coderabbit", "stage": "found", "bot": "codex", "head": "c0ffee",
        "threads": [{ "id": "PRRT_1", "finding": racy() }],
    }));
    let stage = ReviewStage::Found {
        findings: vec![finding(racy())],
        threads: vec!["PRRT_1".into()],
    };
    assert_eq!(phase, bots_pass(stage, Some("codex")));
}

#[test]
fn a_fix_under_way_goes_on_and_resolves_the_threads_it_was_sent() {
    let state = loaded(|value| {
        let item = &mut value["work_items"][0];
        item["phase"] = json!({ "state": "coderabbit", "stage": "fixing", "head": "c0ffee" });
        item["threads_sent"] = json!(["PRRT_1"]);
    });
    let item = &state.work_items[0];
    assert_eq!(item.phase, bots_pass(fixing(Some("c0ffee")), None));
    assert_eq!(item.threads_sent, ["PRRT_1"]);
}

#[test]
fn a_question_asked_during_a_bot_fix_resumes_in_the_pass_of_the_bots() {
    let state = loaded(|value| {
        value["last_ruling"] = json!(3);
        value["rulings"] = json!([{
            "id": 3, "issue": 42, "question": "q", "pull_request": 51,
            "kind": {
                "kind": "question", "asked": "Which flag?",
                "resume": { "state": "coderabbit-fix", "head": "c0ffee" },
            },
        }]);
    });
    let RulingKind::Question { resume, .. } = &state.rulings[0].kind else {
        panic!("{:?}", state.rulings[0].kind);
    };
    let Phase::Review(review) = bots_pass(fixing(Some("c0ffee")), None) else {
        unreachable!()
    };
    assert_eq!(resume, &Resume::Review(review));
}

#[test]
fn a_bot_fix_that_pushed_nothing_is_a_fix_in_the_pass_of_the_bots() {
    let state = loaded(|value| {
        value["last_ruling"] = json!(3);
        value["rulings"] = json!([{
            "id": 3, "issue": 42, "question": "q", "pull_request": 51,
            "kind": {
                "kind": "fix-not-pushed",
                "coderabbit": { "round": 2, "head": "c0ffee" },
                "prompt": "again",
            },
        }]);
    });
    let Phase::Review(review) = bots_pass(fixing(Some("c0ffee")), None) else {
        unreachable!()
    };
    assert_eq!(
        state.rulings[0].kind,
        RulingKind::from(Stuck::FixNotPushed {
            fix: Fix::Review(review),
            prompt: "again".into(),
        })
    );
}

// The cap is gone, so its question says what a yes now does: send the
// threads it held, which the fix resolves.
#[test]
fn a_round_cap_ruling_asks_again_to_send_the_threads_it_held() {
    let state = loaded(|value| {
        value["last_ruling"] = json!(4);
        let item = &mut value["work_items"][0];
        item["phase"] = json!({ "state": "ruling", "id": 3 });
        item["held"] = json!([racy()]);
        item["threads_sent"] = json!(["PRRT_1"]);
        value["rulings"] = json!([
            {
                "id": 3, "issue": 42, "question": "q", "pull_request": 51,
                "kind": {
                    "kind": "coderabbit-cap", "rounds": 2, "held": 1,
                    "prompt": "fix these", "head": "c0ffee",
                },
            },
            {
                "id": 4, "issue": 9, "question": "q", "pull_request": 91,
                "kind": { "kind": "coderabbit-cap", "rounds": 2, "held": 1, "prompt": "older" },
            },
        ]);
    });
    let review = |head: Option<&str>, sent: Vec<Finding>| match bots_pass(
        ReviewStage::Fixing {
            head: head.map(str::to_owned),
            sent,
            deferred_before: Vec::new(),
        },
        None,
    ) {
        Phase::Review(review) => review,
        _ => unreachable!(),
    };
    assert_eq!(
        state.rulings[0].kind,
        RulingKind::from(Stuck::FixNotPushed {
            fix: Fix::Review(review(Some("c0ffee"), vec![finding(racy())])),
            prompt: "fix these".into(),
        })
    );
    assert_eq!(
        state.rulings[0].question,
        "CodeRabbit's rounds on pull request #51 reached their cap with 1 of its threads \
         open. `shep kelpie rule 3 yes` sends the worker those threads, which kelpie \
         resolves once its fix moves the head, and `shep kelpie rule 3 no <note>` sends \
         the worker your note."
    );
    assert_eq!(state.work_items[0].threads_sent, ["PRRT_1"]);
    assert_eq!(
        state.rulings[1].kind,
        RulingKind::from(Stuck::FixNotPushed {
            fix: Fix::Review(review(None, Vec::new())),
            prompt: "older".into(),
        }),
        "a ruling that parks no item sends what it held when answered"
    );
}

#[test]
fn a_ruling_that_keeps_a_bot_rounds_phase_keeps_it_as_the_pass_of_the_bots() {
    let state = loaded(|value| {
        value["last_ruling"] = json!(3);
        value["rulings"] = json!([{
            "id": 3, "issue": 42, "question": "q", "pull_request": 51,
            "kind": {
                "kind": "turn-timeout",
                "phase": { "state": "coderabbit", "stage": "fixing", "head": "c0ffee" },
            },
        }]);
    });
    assert_eq!(
        state.rulings[0].kind,
        RulingKind::from(Stuck::TurnTimeout {
            phase: Some(bots_pass(fixing(Some("c0ffee")), None)),
        })
    );
}

#[test]
fn the_rounds_counted_are_coderabbits_reads() {
    let state = loaded(|value| {
        value["work_items"][0]["coderabbit"] =
            json!({ "rounds": 2, "cap_cleared": true, "satisfied": false });
    });
    let item = &state.work_items[0];
    assert_eq!(item.bot_reads.get(&Bot::Coderabbit), Some(&2));
    assert_eq!(item.bot_reads.len(), 1);

    let state = loaded(|value| {
        value["work_items"][0]["coderabbit"] =
            json!({ "rounds": 0, "cap_cleared": false, "satisfied": true });
    });
    assert!(state.work_items[0].bot_reads.is_empty());
}

// Its yes promised a summon after CI: the pass of the bots gives it now.
#[test]
fn a_pending_silent_bot_ruling_is_a_pass_of_the_bots_instead() {
    let state = loaded(|value| {
        value["last_ruling"] = json!(3);
        value["work_items"][0]["phase"] = json!({ "state": "ruling", "id": 3 });
        value["rulings"] = json!([{
            "id": 3, "issue": 42, "question": "q", "pull_request": 51,
            "kind": { "kind": "coderabbit-silent", "bot": "cubic", "head": "c0ffee" },
        }]);
    });
    assert_eq!(state.rulings, []);
    assert_eq!(
        state.work_items[0].phase,
        bots_pass(ReviewStage::Round, None)
    );
    assert!(!state.work_items[0].bots_after_ci, "its pass is the bots'");
}

#[test]
fn a_pending_silent_bot_ruling_that_parks_no_item_is_dropped() {
    let state = loaded(|value| {
        value["last_ruling"] = json!(3);
        value["rulings"] = json!([{
            "id": 3, "issue": 9, "question": "q", "pull_request": 91,
            "kind": { "kind": "coderabbit-silent", "head": "c0ffee" },
        }]);
    });
    assert_eq!(state.rulings, []);
    assert_eq!(state.last_ruling, 3, "its id is never given again");
}

#[test]
fn an_item_past_its_review_that_no_bot_read_owes_the_bots_a_pass_after_ci() {
    let owed = |tally: Value| {
        let state = loaded(|value| value["work_items"][0]["coderabbit"] = tally);
        state.work_items[0].bots_after_ci
    };
    assert!(owed(
        json!({ "rounds": 0, "cap_cleared": false, "satisfied": false })
    ));
    assert!(!owed(
        json!({ "rounds": 1, "cap_cleared": false, "satisfied": true })
    ));
}

#[test]
fn an_owed_summon_loads_for_the_runner_to_give_each_listed_bot() {
    let state = loaded(|value| value["work_items"][0]["summon_owed"] = json!(true));
    assert!(state.work_items[0].summon_owed);
    assert!(state.work_items[0].summons_owed.is_empty());
}

#[test]
fn a_file_with_a_bot_round_saves_as_the_current_version_with_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = saved_with(dir.path(), |value| {
        value["version"] = json!(3);
        let item = &mut value["work_items"][0];
        item["phase"] = json!({ "state": "coderabbit", "stage": "lease", "head": "c0ffee" });
        item["coderabbit"] = json!({ "rounds": 1, "cap_cleared": false, "satisfied": false });
    });
    let state = store.load().unwrap().unwrap();
    store.save(&state).unwrap();
    let saved = std::fs::read_to_string(dir.path().join("state.json")).unwrap();
    let saved: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved["version"], 16);
    assert_eq!(saved["work_items"][0]["phase"]["state"], "review");
    assert_eq!(saved["work_items"][0].get("coderabbit"), None);
    assert_eq!(store.load().unwrap().unwrap(), state);
}
