//! The work item as its own record, and the format its file pins

use serde_json::json;

use super::*;
use crate::review_bot::Bot;
use crate::test::{a_work_item, ticket};

#[test]
fn the_format_is_pinned() {
    let mut item = a_work_item();
    item.reviewed_heads = vec!["c0ffee".into()];
    item.sent_unread = vec!["f1x".into()];
    item.nit_fix_heads = vec!["f1x".into()];
    item.late_from = Some("c0ffee".into());
    assert_eq!(
        serde_json::to_value(item).unwrap(),
        json!({
            "issue": "42",
            "title": "Add a thing",
            "branch": "kelpie/42",
            "rework": false,
            "adopted": false,
            "worktree": "/k/wt/shep/42",
            "build": "/k/targets/shep/42",
            "agent": "opus-high",
            "pinned": true,
            "session": "5e55",
            "turn": { "state": "running", "since": 9 },
            "pull_request": 51,
            "phase": { "state": "ci", "head": "c0ffee", "since": 11 },
            "red_head": "bad",
            "conflict": { "head": "c0ffee", "main": "a11ce", "turns": 1 },
            "resume": null,
            "review_call": { "state": "idle" },
            "known": { "labels": ["review please"], "ready": false },
            "reviewed_heads": ["c0ffee"],
            "sent_unread": ["f1x"],
            "nit_fix_heads": ["f1x"],
            "late_from": "c0ffee",
            "qwen": { "rounds": 0, "seconds": 0 },
            "timings": {
                "created": 5,
                "since": 12,
                "seconds": {
                    "worker": 4, "review": 0, "ci": 3,
                    "ruling": 0, "merge": 0, "other": 0,
                },
            },
            "counts": { "review_rounds": 2, "fix_turns": 1, "rulings": 1, "ci_fix_turns": 1 },
            "calls": [{
                "role": "worker",
                "at": 10,
                "session": "5e55",
                "usage": { "input": 1, "cache_write": 2, "cache_read": 3, "output": 4 },
                "cost": 5,
                "session_cost": 6,
            }],
        })
    );
}

#[test]
fn every_turn_state_is_pinned() {
    let value = |t: Turn| serde_json::to_value(t).unwrap();
    assert_eq!(value(Turn::Due), json!({ "state": "due" }));
    assert_eq!(
        value(Turn::Ended { at: Timestamp(3) }),
        json!({ "state": "ended", "at": 3 })
    );
    assert_eq!(
        value(Turn::Failed {
            at: Timestamp(4),
            reason: "no worktree".into()
        }),
        json!({ "state": "failed", "at": 4, "reason": "no worktree" })
    );
}

#[test]
fn every_phase_is_pinned() {
    let value = |p: Phase| serde_json::to_value(p).unwrap();
    assert_eq!(value(Phase::Implement), json!({ "state": "implement" }));
    assert_eq!(
        value(Phase::Review(Review {
            round: 2,
            ..Review::first()
        })),
        json!({
            "state": "review",
            "round": 2,
            "stage": { "stage": "round" },
            "unread": true,
        })
    );
    let name = |n: &str| Some(AgentName::try_from(n.to_owned()).unwrap());
    assert_eq!(
        value(Phase::Review(Review {
            round: 3,
            reviewer: name("opus"),
            ran: name("qwen").into_iter().collect(),
            failures: 1,
            ..Review::first()
        })),
        json!({
            "state": "review",
            "round": 3,
            "stage": { "stage": "round" },
            "reviewer": "opus",
            "ran": ["qwen"],
            "failures": 1,
            "unread": true,
        })
    );
    let older = json!({
        "state": "review",
        "round": 3,
        "stage": { "stage": "round" },
        "last": "qwen",
    });
    let read: Phase = serde_json::from_value(older.clone()).unwrap();
    assert_eq!(value(read), older, "kept until the next round reads it");
    let bot = |stage: ReviewStage| {
        value(Phase::Review(Review {
            stage,
            reviewer: name("cubic"),
            bots_only: true,
            unread: false,
            ..Review::first()
        }))
    };
    assert_eq!(
        bot(ReviewStage::Summon {
            bot: Bot::Cubic,
            started: Timestamp(10),
            head: "c0ffee".into(),
            readied: None,
            full: true,
        }),
        json!({
            "state": "review",
            "round": 1,
            "stage": { "stage": "summon", "bot": "cubic", "started": 10, "head": "c0ffee", "full": true },
            "reviewer": "cubic",
            "bots_only": true,
        })
    );
    assert_eq!(
        bot(ReviewStage::Summoned {
            bot: Bot::Coderabbit,
            started: Timestamp(10),
            head: "c0ffee".into(),
            at: Timestamp(12),
            full: false,
            resent: true,
            failed: false,
        })["stage"],
        json!({ "stage": "summoned", "bot": "coderabbit", "started": 10, "head": "c0ffee", "at": 12, "resent": true })
    );
    assert_eq!(
        bot(ReviewStage::Summoned {
            bot: Bot::Codex,
            started: Timestamp(10),
            head: "c0ffee".into(),
            at: Timestamp(12),
            full: true,
            resent: false,
            failed: true,
        })["stage"],
        json!({ "stage": "summoned", "bot": "codex", "started": 10, "head": "c0ffee", "at": 12, "full": true, "failed": true })
    );
    let found = ReviewStage::Found {
        findings: Vec::new(),
        threads: vec!["PRRT_1".into()],
    };
    assert_eq!(
        bot(found)["stage"],
        json!({ "stage": "found", "findings": [], "threads": ["PRRT_1"] })
    );
    assert_eq!(
        bot(ReviewStage::Settling {
            bot: Bot::Cubic,
            since: Timestamp(20),
            read: Timestamp(40),
            open: vec!["PRRT_2".into()],
        })["stage"],
        json!({ "stage": "settling", "bot": "cubic", "since": 20, "read": 40, "open": ["PRRT_2"] })
    );
    assert_eq!(
        bot(ReviewStage::Pushing)["stage"],
        json!({ "stage": "pushing" })
    );
    assert_eq!(
        value(Phase::Ruling { id: ticket(3) }),
        json!({ "state": "ruling", "id": "3" })
    );
    assert_eq!(
        value(Phase::Merge {
            head: "c0ffee".into(),
            readied: Some(Timestamp(12)),
            auto: false,
        }),
        json!({ "state": "merge", "head": "c0ffee", "readied": 12 })
    );
    assert_eq!(
        value(Phase::Merge {
            head: "c0ffee".into(),
            readied: None,
            auto: true,
        }),
        json!({ "state": "merge", "head": "c0ffee", "readied": null, "auto": true })
    );
    assert_eq!(
        value(Phase::Done {
            merged: true,
            closed: false
        }),
        json!({ "state": "done", "merged": true })
    );
    assert_eq!(
        value(Phase::Done {
            merged: false,
            closed: true
        }),
        json!({ "state": "done", "merged": false, "closed": true })
    );
    assert_eq!(
        serde_json::to_value(Turn::Next {
            prompt: "fix it".into()
        })
        .unwrap(),
        json!({ "state": "next", "prompt": "fix it" })
    );
}

#[test]
fn review_call_state_is_pinned() {
    assert_eq!(
        serde_json::to_value(ReviewCallState::Idle).unwrap(),
        json!({ "state": "idle" })
    );
    assert_eq!(
        serde_json::to_value(ReviewCallState::Running {
            since: Timestamp(9)
        })
        .unwrap(),
        json!({ "state": "running", "since": 9 })
    );
}

#[test]
fn a_work_item_saved_before_phases_reads_as_implementing() {
    let mut value = serde_json::to_value(a_work_item()).unwrap();
    let fields = value.as_object_mut().unwrap();
    fields.remove("phase");
    fields.remove("red_head");
    let item: WorkItem = serde_json::from_value(value).unwrap();
    assert_eq!((item.phase, item.red_head), (Phase::Implement, None));
}

#[test]
fn the_review_diffs_from_main_unless_a_pull_request_arrived_at_a_head() {
    let mut item = a_work_item();
    assert_eq!(item.review_base("main"), "origin/main");
    item.arrived = Some("c0ffee".into());
    assert_eq!(item.review_base("main"), "c0ffee");
}

#[test]
fn a_work_item_saved_before_reworks_and_adoptions_is_neither() {
    let mut value = serde_json::to_value(a_work_item()).unwrap();
    let fields = value.as_object_mut().unwrap();
    fields.remove("rework");
    fields.remove("adopted");
    let item: WorkItem = serde_json::from_value(value).unwrap();
    assert!(!item.rework && !item.adopted);
}

#[test]
fn a_refused_merge_is_saved_only_while_it_stands() {
    let mut item = a_work_item();
    let value = serde_json::to_value(&item).unwrap();
    assert!(value.get("merge_refused").is_none(), "{value}");
    assert!(
        !serde_json::from_value::<WorkItem>(value)
            .unwrap()
            .merge_refused
    );
    item.merge_refused = true;
    let value = serde_json::to_value(&item).unwrap();
    assert_eq!(value["merge_refused"], true);
    assert!(
        serde_json::from_value::<WorkItem>(value)
            .unwrap()
            .merge_refused
    );
}

#[test]
fn session_ids_are_distinct_version_4_uuids() {
    let a = new_session_id().unwrap().0;
    let b = new_session_id().unwrap().0;
    assert_ne!(a, b);
    let groups: Vec<_> = a.split('-').map(str::len).collect();
    assert_eq!(groups, [8, 4, 4, 4, 12]);
    assert_eq!(&a[14..15], "4");
    assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"), "{a}");
}

#[test]
fn a_calls_cost_is_measured_from_its_own_sessions_last_call() {
    let mut item = a_work_item();
    let other = SessionId("0th3r".into());
    let mut call = item.calls[0].clone();
    call.session = other.clone();
    call.session_cost = Cost(40);
    call.cost = Cost(40);
    item.calls.push(call);
    assert_eq!(item.session_cost(&item.session), Cost(6));
    assert_eq!(item.session_cost(&other), Cost(40));
    assert_eq!(item.session_cost(&SessionId("new".into())), Cost(0));
    assert_eq!(item.cost(), Cost(45));
}

#[test]
fn a_call_the_item_does_not_count_moves_its_sessions_cost_without_adding_to_its_own() {
    let mut item = a_work_item();
    let session = item.session.clone();
    let before = item.cost();

    item.advance_session_cost(&session, Cost(30));
    item.advance_session_cost(&session, Cost(10));
    item.advance_session_cost(&SessionId("new".into()), Cost(99));

    assert_eq!(item.session_cost(&session), Cost(30));
    assert_eq!(item.cost(), before);
    let next = item.record_call(
        Role::Worker,
        Timestamp(9),
        session,
        Usage::default(),
        Some(Cost(35)),
    );
    assert_eq!(next, Some(Cost(5)));
}

#[test]
fn a_call_priced_by_its_energy_counts_toward_the_items_cost_and_is_not_unpriced() {
    let mut item = a_work_item();
    let session = SessionId("local".into());
    let (calls, cost, unpriced) = (item.spend().worker.calls, item.cost(), item.spend().worker);
    let call = |item: &mut WorkItem, session_cost, energy| {
        let session = session.clone();
        item.record_metered_call(
            Role::Worker,
            Timestamp(9),
            session,
            Usage::default(),
            session_cost,
            energy,
        )
    };

    let by_energy = call(&mut item, None, Some(Cost(4)));
    let by_harness = call(&mut item, Some(Cost(10)), Some(Cost(4)));
    let neither = call(&mut item, None, None);

    assert_eq!(by_energy, Some(Cost(4)));
    assert_eq!(by_harness, Some(Cost(6)), "the harness's own dollars win");
    assert_eq!(neither, None);
    assert_eq!(item.cost(), Cost(cost.0 + 10));
    let worker = item.spend().worker;
    assert_eq!(worker.calls, calls + 3);
    assert_eq!(worker.unpriced_calls, unpriced.unpriced_calls + 1);
}
