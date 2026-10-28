use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::*;
use crate::test::{a_work_item, a_work_item_from_before_comments, ticket};
use crate::work_item::TimingPhase;

const WRITER_DIR: &str = "KELPIE_TEST_WRITER_DIR";

fn store_in(dir: &Path) -> StateStore {
    StateStore::new(dir.join(STATE_FILE))
}

// The one place the number is written out in a test, so a bump is a
// deliberate edit here and shows in review. Every other test reads `VERSION`.
#[test]
fn the_state_file_version_is_pinned() {
    assert_eq!(VERSION, 89);
}

// Large, so a kill is likely to land inside a write, and uniform, so a
// mix of two saves is visible.
fn big_state(n: TicketId) -> ProjectState {
    let mut state = ProjectState::new();
    state.finished = vec![n];
    state.rulings = (1..20_000u64)
        .map(|id| Ruling {
            issue: Some(n),
            pull_request: n.github(),
            ..Ruling::new(
                ticket(id),
                format!("question {n}"),
                RulingKind::from(Stuck::Closed),
            )
        })
        .collect();
    state
}

fn assert_whole(state: &ProjectState) {
    let n = state.finished[0];
    assert_eq!(state, &big_state(n), "state {n} was saved in part");
}

#[test]
fn no_file_loads_as_no_state() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(store_in(dir.path()).load().unwrap(), None);
}

#[test]
fn a_saved_state_loads_back_whole() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items = vec![a_work_item()];
    state.rulings.push(Ruling {
        issue: Some(ticket(7)),
        pull_request: Some(43),
        alerted: true,
        ..Ruling::new(
            ticket(1),
            "merge #43?",
            RulingKind::Merge {
                head: "c0ffee".into(),
                unreviewed: None,
                open_threads: None,
                held_by: Vec::new(),
                unread_head: false,
                note_fix: false,
                late_fix: false,
                nits: None,
                disputes: None,
                rivals: Vec::new(),
            },
        )
    });
    state.leases.push(LeaseHeld {
        resource: Resource::Gpu,
        issue: Some(ticket(7)),
        since: Timestamp(1_790_000_100),
    });
    state.pacing = Some(DayStart {
        week_resets_at: Timestamp(1_790_500_000),
        day: 2,
        week_used_pct: 31,
    });
    state.codex_pacing = Some(DayStart {
        week_resets_at: Timestamp(1_790_600_000),
        day: 3,
        week_used_pct: 12,
    });
    store.save(&state).unwrap();
    assert_eq!(store.load().unwrap(), Some(state));
}

#[test]
fn a_file_with_one_work_item_loads_as_a_list_of_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut one = serde_json::json!({
        "version": 1,
        "run": "running",
        "since": 7,
        "work_item": serde_json::to_value(a_work_item()).unwrap(),
        "rulings": [{ "id": 3, "question": "q", "pull_request": 51, "kind": { "kind": "closed" } }],
        "last_ruling": 3,
        "leases": [],
    });
    fs::write(dir.path().join(STATE_FILE), one.to_string()).unwrap();
    let state = store.load().unwrap().unwrap();
    assert_eq!(state.work_items, [a_work_item()]);
    assert_eq!(
        state.rulings[0].issue,
        Some(ticket(42)),
        "its rulings are its item's"
    );

    store.save(&state).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (&saved["version"], saved.get("work_item")),
        (&serde_json::json!(VERSION), None)
    );
    assert_eq!(store.load().unwrap(), Some(state));

    one["work_item"] = serde_json::Value::Null;
    fs::write(dir.path().join(STATE_FILE), one.to_string()).unwrap();
    let state = store.load().unwrap().unwrap();
    assert_eq!((state.work_items, state.rulings[0].issue), (vec![], None));
}

#[test]
fn a_file_saved_before_pacing_loads_with_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let old =
        r#"{"version":1,"run":"running","since":7,"work_item":null,"rulings":[],"leases":[]}"#;
    fs::write(dir.path().join(STATE_FILE), old).unwrap();
    let state = store.load().unwrap().unwrap();
    assert_eq!(state.pacing, None);
}

#[test]
fn a_file_saved_before_bumps_loads_with_none_and_a_bump_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let old =
        r#"{"version":55,"run":"running","since":7,"work_item":null,"rulings":[],"leases":[]}"#;
    fs::write(dir.path().join(STATE_FILE), old).unwrap();
    let mut state = store.load().unwrap().unwrap();
    assert_eq!(state.bumps, []);

    let mut bump = Bump::new(
        40,
        "dependabot".into(),
        "dependabot/cargo/serde-1.0.230".into(),
        "3b9e2f7".into(),
        Timestamp(9),
    );
    bump.red = Some(Red {
        head: "3b9e2f7".into(),
        why: vec!["check `test` failed".into()],
        step: RedStep::Close,
    });
    state.bumps.push(bump);
    store.save(&state).unwrap();

    assert_eq!(store.load().unwrap().unwrap().bumps, state.bumps);
}

#[test]
fn the_file_format_is_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.leases.push(LeaseHeld {
        resource: Resource::Coderabbit,
        issue: Some(ticket(22)),
        since: Timestamp(8),
    });
    let ruling = |id: u64, kind| Ruling {
        issue: Some(ticket(22)),
        pull_request: Some(30),
        alerted: id.is_multiple_of(2),
        ..Ruling::new(ticket(id), "q", kind)
    };
    state.rulings = vec![
        ruling(
            1,
            RulingKind::Merge {
                head: "c0ffee".into(),
                unreviewed: None,
                open_threads: Some("3 from cubic".into()),
                held_by: Vec::new(),
                unread_head: true,
                note_fix: false,
                late_fix: true,
                nits: Some("2 nits left open (cubic)".into()),
                disputes: None,
                rivals: Vec::new(),
            },
        ),
        ruling(
            2,
            RulingKind::from(Stuck::Rebase {
                why: "conflicts".into(),
                review: None,
            }),
        ),
        ruling(
            3,
            RulingKind::from(Stuck::StillRed {
                head: "bad".into(),
                checks: vec!["lint".into()],
                fix_turns: Some(2),
                review: Some(Review::first()),
                rerun: true,
            }),
        ),
        ruling(4, RulingKind::from(Stuck::Closed)),
        ruling(
            5,
            RulingKind::Question {
                asked: "Which name?".into(),
                resume: Resume::Nothing,
            },
        ),
        ruling(6, RulingKind::from(Stuck::TurnTimeout { phase: None })),
        ruling(
            7,
            RulingKind::ForeignChange {
                description: "the `bug` label was added".into(),
                known: Known {
                    labels: vec!["bug".into()],
                    ready: false,
                    head: Some("c0ffee".into()),
                },
            },
        ),
        ruling(
            8,
            RulingKind::from(Stuck::MergeRefused {
                head: "c0ffee".into(),
                why: "a ruleset".into(),
            }),
        ),
    ];
    state.rulings.push(ruling(
        9,
        RulingKind::from(Stuck::Unpushed {
            files: vec!["late.txt".into()],
            head: "a1".into(),
            pushed: "b2".into(),
            review: Review::first(),
        }),
    ));
    state.rulings.push(ruling(
        11,
        RulingKind::from(Stuck::CheckQueued {
            head: "c0ffee".into(),
            checks: vec!["windows-gnu".into()],
            run: 39_697_676_985,
            url: "https://github.com/shep-pm/shep/actions/runs/39697676985".into(),
            review: None,
        }),
    ));
    state.rulings.push(ruling(
        12,
        RulingKind::from(Stuck::NoChecks {
            head: "c0ffee".into(),
            review: None,
        }),
    ));
    state.notices = vec![Notice::Merged(MergeNotice {
        issue: ticket(22),
        pull_request: 30,
        head: "c0ffee".into(),
    })];
    state.finished = vec![ticket(22), ticket(30)];
    state.reworked = vec!["PRR_1".into()];
    state.adopted = vec![
        Waiting {
            pull_request: 614,
            by_label: false,
        },
        Waiting {
            pull_request: 638,
            by_label: true,
        },
    ];
    state.pacing = Some(DayStart {
        week_resets_at: Timestamp(9),
        day: 1,
        week_used_pct: 10,
    });
    state.replies = Replies {
        last: Some(LastRead {
            id: "W3EqiUm5rsNq".into(),
            time: Timestamp(5),
        }),
    };
    store.save(&state).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let pinned = |id: u64, kind| {
        serde_json::json!({
            "id": id.to_string(),
            "issue": "22",
            "question": "q",
            "pull_request": 30,
            "kind": kind,
            "alerted": id.is_multiple_of(2),
        })
    };
    assert_eq!(
        value,
        serde_json::json!({
            "version": VERSION,
            "work_items": [],
            "rulings": [
                pinned(1, serde_json::json!({
                    "kind": "merge",
                    "head": "c0ffee",
                    "open_threads": "3 from cubic",
                    "unread_head": true,
                    "late_fix": true,
                    "nits": "2 nits left open (cubic)",
                })),
                pinned(2, serde_json::json!({ "kind": "stuck", "reason": "rebase", "why": "conflicts" })),
                pinned(3, serde_json::json!({
                    "kind": "stuck",
                    "reason": "still-red",
                    "head": "bad",
                    "checks": ["lint"],
                    "fix_turns": 2,
                    "review": { "round": 1, "stage": { "stage": "round" }, "unread": true },
                    "rerun": true,
                })),
                pinned(4, serde_json::json!({ "kind": "stuck", "reason": "closed" })),
                pinned(
                    5,
                    serde_json::json!({
                        "kind": "question",
                        "asked": "Which name?",
                        "resume": { "state": "nothing" },
                    }),
                ),
                pinned(6, serde_json::json!({ "kind": "stuck", "reason": "turn-timeout" })),
                pinned(7, serde_json::json!({
                    "kind": "foreign-change",
                    "description": "the `bug` label was added",
                    "known": { "labels": ["bug"], "ready": false, "head": "c0ffee" },
                })),
                pinned(8, serde_json::json!({
                    "kind": "stuck",
                    "reason": "merge-refused",
                    "head": "c0ffee",
                    "why": "a ruleset",
                })),
                pinned(9, serde_json::json!({
                    "kind": "stuck",
                    "reason": "unpushed",
                    "files": ["late.txt"],
                    "head": "a1",
                    "pushed": "b2",
                    "review": { "round": 1, "stage": { "stage": "round" }, "unread": true },
                })),
                pinned(11, serde_json::json!({
                    "kind": "stuck",
                    "reason": "check-queued",
                    "head": "c0ffee",
                    "checks": ["windows-gnu"],
                    "run": 39_697_676_985_u64,
                    "url": "https://github.com/shep-pm/shep/actions/runs/39697676985",
                })),
                pinned(12, serde_json::json!({
                    "kind": "stuck",
                    "reason": "no-checks",
                    "head": "c0ffee",
                })),
            ],
            "finished": ["22", "30"],
            "history": [],
            "reworked": ["PRR_1"],
            "adopted": [
                { "pull_request": 614, "by_label": false },
                { "pull_request": 638, "by_label": true },
            ],
            "leases": [{ "resource": "coderabbit", "issue": "22", "since": 8 }],
            "pacing": { "week_resets_at": 9, "day": 1, "week_used_pct": 10 },
            "notices": [{ "issue": "22", "pull_request": 30, "head": "c0ffee" }],
            "replies": { "last": { "id": "W3EqiUm5rsNq", "time": 5 } },
        })
    );
}

#[test]
fn the_history_keeps_the_last_hundred_finished_work_items() {
    let mut state = ProjectState::new();
    let finished = |issue: u64| Finished {
        issue: ticket(issue),
        title: format!("Issue {issue}"),
        pull_request: None,
        merged: true,
        closed: false,
        at: Timestamp(issue),
        wall: 0,
        seconds: Seconds::default(),
        spend: None,
    };
    let last = HISTORY_CAP as u64 + 5;
    for issue in 1..=last {
        state.record_finished(finished(issue));
    }
    let issues: Vec<_> = state.history.iter().map(|f| f.issue).collect();
    assert_eq!(issues, (6..=last).map(ticket).collect::<Vec<_>>());
}

#[test]
fn a_finished_work_item_is_pinned() {
    let finished = Finished {
        issue: ticket(7),
        title: "Seven".into(),
        pull_request: Some(71),
        merged: true,
        closed: false,
        at: Timestamp(1_790_000_100),
        wall: 100,
        seconds: Seconds::of(&[(TimingPhase::Worker, 60), (TimingPhase::Ci, 40)]),
        spend: Some(a_work_item().tally()),
    };
    let saved = serde_json::to_value(&finished).unwrap();
    assert_eq!(serde_json::from_value::<Finished>(saved).unwrap(), finished);
    assert_eq!(
        serde_json::to_value(&finished).unwrap(),
        serde_json::json!({
            "issue": "7",
            "title": "Seven",
            "pull_request": 71,
            "merged": true,
            "at": 1_790_000_100,
            "wall": 100,
            "seconds": {
                "worker": 60,
                "review": 0,
                "ci": 40,
                "ruling": 0,
                "merge": 0,
                "other": 0,
            },
            "spend": {
                "worker": {
                    "calls": 1,
                    "tokens": { "input": 1, "cache_write": 2, "cache_read": 3, "output": 4 },
                    "units": 25,
                    "cost": 5,
                },
                "reviewer": {
                    "calls": 0,
                    "tokens": { "input": 0, "cache_write": 0, "cache_read": 0, "output": 0 },
                    "units": 0,
                    "cost": 0,
                },
                "qwen": { "rounds": 0, "seconds": 0 },
                "counts": { "review_rounds": 2, "fix_turns": 1, "rulings": 1, "ci_fix_turns": 1 },
            },
        })
    );
}

// A version 17 file has no `closed`: every finished item and `done` phase
// reads as not closed, and saves with none.
#[test]
fn a_version_17_file_loads_with_nothing_closed_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    let mut item = a_work_item();
    item.phase = Phase::Done {
        merged: false,
        closed: false,
    };
    state.work_items.push(item);
    let mut old = serde_json::to_value(state).unwrap();
    old["version"] = 17.into();
    old["history"] = serde_json::json!([{
        "issue": 3, "title": "Three", "pull_request": null, "merged": false,
        "at": 9, "wall": 0, "seconds": serde_json::to_value(Seconds::default()).unwrap(),
    }]);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert!(!loaded.history[0].closed);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    let mut older = old["history"].clone();
    older[0]["issue"] = serde_json::json!("3");
    assert_eq!(
        saved["history"], older,
        "the number saves as the ticket id of it"
    );
    assert_eq!(
        saved["work_items"][0]["phase"],
        old["work_items"][0]["phase"]
    );
}

// A version 18 file has no `finishing`, so it loads with the board picking.
#[test]
fn a_version_18_file_loads_not_finishing_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 18.into();
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let mut loaded = store.load().unwrap().expect("a saved state");
    assert!(!loaded.finishing);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (saved["version"].clone(), saved.get("finishing")),
        (VERSION.into(), None)
    );

    loaded.finishing = true;
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["finishing"], true);
    assert_eq!(store.load().unwrap(), Some(loaded));
}

#[test]
fn a_finished_work_item_closed_with_no_change_is_pinned() {
    let finished = Finished {
        issue: ticket(7),
        title: "Seven".into(),
        pull_request: None,
        merged: false,
        closed: true,
        at: Timestamp(1_790_000_100),
        wall: 0,
        seconds: Seconds::default(),
        spend: None,
    };
    let saved = serde_json::to_value(&finished).unwrap();
    assert_eq!(saved["closed"], true);
    assert_eq!(serde_json::from_value::<Finished>(saved).unwrap(), finished);
}

#[test]
fn a_version_9_file_loads_with_nothing_counted_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut old = serde_json::to_value(ProjectState::new()).unwrap();
    old["version"] = 9.into();
    old["run"] = "running".into();
    old["since"] = 7.into();
    let mut item = serde_json::to_value(a_work_item()).unwrap();
    item.as_object_mut().unwrap().remove("counts");
    old["work_items"] = serde_json::json!([item]);
    old["history"] = serde_json::json!([{
        "issue": 3, "title": "Three", "pull_request": 30, "merged": true, "at": 9,
        "wall": 0, "seconds": {
            "worker": 0, "review": 0, "ci": 0, "ruling": 0, "merge": 0, "other": 0,
        },
    }]);
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert!(loaded.work_items[0].counts.is_zero());
    assert_eq!(loaded.history[0].spend, None);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
}

#[test]
fn a_version_10_file_with_the_ledger_and_a_run_state_loads_and_saves_at_the_current_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item_from_before_comments());
    state.work_items[0].counts.review_rounds = 2;
    state.record_finished(Finished {
        issue: ticket(3),
        title: "Three".into(),
        pull_request: Some(30),
        merged: true,
        closed: false,
        at: Timestamp(9),
        wall: 100,
        seconds: Seconds::of(&[(TimingPhase::Worker, 100)]),
        spend: Some(a_work_item().tally()),
    });
    let mut old = serde_json::to_value(&state).unwrap();
    old["version"] = 10.into();
    old["run"] = "paused".into();
    old["since"] = 7.into();
    assert!(old["work_items"][0].get("counts").is_some(), "{old}");
    assert!(old["history"][0].get("spend").is_some(), "{old}");
    fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(loaded, state);
    store.save(&loaded).unwrap();
    let text = fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["version"], VERSION);
    assert_eq!((saved.get("run"), saved.get("since")), (None, None));
    assert_eq!(
        saved["work_items"][0]["counts"],
        old["work_items"][0]["counts"]
    );
    assert_eq!(saved["history"][0]["spend"], old["history"][0]["spend"]);
}

#[test]
fn a_timeout_keeps_the_phase_its_turn_ran_in() {
    let kind = RulingKind::from(Stuck::TurnTimeout {
        phase: Some(Phase::Implement),
    });
    let saved = serde_json::to_value(&kind).unwrap();
    assert_eq!(
        saved,
        serde_json::json!({
            "kind": "stuck",
            "reason": "turn-timeout",
            "phase": { "state": "implement" },
        })
    );
    assert_eq!(serde_json::from_value::<RulingKind>(saved).unwrap(), kind);
}

#[test]
fn a_running_turn_saved_before_it_kept_a_prompt_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new();
    state.work_items.push(a_work_item());
    let mut old = serde_json::to_value(&state).unwrap();
    assert_eq!(
        old["work_items"][0]["turn"],
        serde_json::json!({ "state": "running", "since": 9 }),
        "a turn with no prompt saves as it always did"
    );
    old["work_items"][0]["turn"] = serde_json::json!({ "state": "running", "since": 9 });
    std::fs::write(dir.path().join(STATE_FILE), old.to_string()).unwrap();

    let loaded = store.load().unwrap().expect("a saved state");
    assert_eq!(
        loaded.work_items[0].turn,
        Turn::Running {
            since: Timestamp(9),
            prompt: None,
        }
    );
}

#[test]
fn a_running_turn_keeps_the_prompt_it_was_sent() {
    let turn = Turn::Running {
        since: Timestamp(9),
        prompt: Some("fix the five".into()),
    };
    let saved = serde_json::to_value(&turn).unwrap();
    assert_eq!(
        saved,
        serde_json::json!({ "state": "running", "since": 9, "prompt": "fix the five" })
    );
    assert_eq!(serde_json::from_value::<Turn>(saved).unwrap(), turn);
}

#[test]
fn a_failed_turn_keeps_its_phase_and_the_turn_to_retry() {
    let kind = RulingKind::from(Stuck::TurnFailed {
        why: "no worktree".into(),
        phase: Phase::Implement,
        retry: Turn::Due,
        restore: Vec::new(),
        setup: false,
    });
    let saved = serde_json::to_value(&kind).unwrap();
    assert_eq!(
        saved,
        serde_json::json!({
            "kind": "stuck",
            "reason": "turn-failed",
            "why": "no worktree",
            "phase": { "state": "implement" },
            "retry": { "state": "due" },
        })
    );
    assert_eq!(serde_json::from_value::<RulingKind>(saved).unwrap(), kind);
}

#[test]
fn a_failed_turn_keeps_the_agent_files_it_offers_to_restore() {
    let kind = RulingKind::from(Stuck::TurnFailed {
        why: "agents' own files differ".into(),
        phase: Phase::Implement,
        retry: Turn::Due,
        restore: vec![".claude/settings.json".into()],
        setup: false,
    });
    let saved = serde_json::to_value(&kind).unwrap();
    assert_eq!(
        saved["restore"],
        serde_json::json!([".claude/settings.json"])
    );
    assert_eq!(serde_json::from_value::<RulingKind>(saved).unwrap(), kind);
}

#[test]
fn a_change_to_agent_files_keeps_its_head_files_and_phase() {
    let kind = RulingKind::AgentFiles {
        head: "c0ffee".into(),
        files: vec![".mcp.json".into()],
        phase: Phase::Implement,
    };
    let saved = serde_json::to_value(&kind).unwrap();
    assert_eq!(
        saved,
        serde_json::json!({
            "kind": "agent-files",
            "head": "c0ffee",
            "files": [".mcp.json"],
            "phase": { "state": "implement" },
        })
    );
    assert_eq!(serde_json::from_value::<RulingKind>(saved).unwrap(), kind);
}

#[test]
fn a_test_still_fails_ruling_round_trips() {
    let saved = serde_json::json!({
        "kind": "stuck",
        "reason": "test-still-fails",
        "review": {
            "round": 2,
            "stage": { "stage": "fixing", "head": "c0ffee" },
        },
        "head": "c0ffee",
        "tests": [{ "test": "tests/flag.rs", "output": "assertion failed: flag" }],
        "prompt": "again",
    });
    let kind: RulingKind = serde_json::from_value(saved.clone()).unwrap();
    let RulingKind::Stuck(Stuck::TestStillFails {
        fix: Fix::Review(review),
        head,
        tests,
        prompt,
    }) = &kind
    else {
        panic!("read as {kind:?}");
    };
    assert_eq!(
        (review.round, head.as_str(), prompt.as_str()),
        (2, "c0ffee", "again")
    );
    assert_eq!(tests[0].test, "tests/flag.rs");
    assert_eq!(serde_json::to_value(&kind).unwrap(), saved);
}

#[test]
fn a_qwen_fix_not_pushed_keeps_its_wire_shape() {
    let saved = serde_json::json!({
        "kind": "stuck",
        "reason": "fix-not-pushed",
        "review": {
            "round": 1,
            "stage": { "stage": "fixing", "head": "c0ffee" },
        },
        "prompt": "again",
    });
    let kind: RulingKind = serde_json::from_value(saved.clone()).unwrap();
    let RulingKind::Stuck(Stuck::FixNotPushed {
        fix: Fix::Review(review),
        ..
    }) = &kind
    else {
        panic!("read as {kind:?}");
    };
    assert_eq!(review.round, 1);
    assert_eq!(serde_json::to_value(&kind).unwrap(), saved);
    let stray = serde_json::json!({
        "kind": "stuck",
        "reason": "fix-not-pushed",
        "coderabbit": { "round": 3, "head": "c0ffee" },
        "prompt": "again",
        "extra": true,
    });
    assert!(serde_json::from_value::<RulingKind>(stray).is_err());
    let misspelt = serde_json::json!({
        "kind": "stuck",
        "reason": "fix-not-pushed",

        "coderabbit": { "round": 3, "head": "c0ffee", "heade": "c0ffee" },
        "prompt": "again",
    });
    assert!(serde_json::from_value::<RulingKind>(misspelt).is_err());
}

#[test]
fn a_state_saved_before_ruling_ids_were_counted_loads_with_none_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    fs::write(
        dir.path().join(STATE_FILE),
        r#"{"version":1,"run":"running","since":3,"work_item":null,"rulings":[],"leases":[]}"#,
    )
    .unwrap();
    let state = store.load().unwrap().unwrap();
    assert_eq!((state.rulings, state.finished), (vec![], vec![]));
    assert!(state.reworked.is_empty());
}

#[test]
fn a_ruling_saved_before_webhooks_is_posted_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let old = r#"{"version":1,"run":"running","since":7,"work_item":null,
        "rulings":[{"id":1,"question":"q","pull_request":3,"kind":{"kind":"closed"}}],
        "leases":[]}"#;
    fs::write(dir.path().join(STATE_FILE), old).unwrap();
    assert!(!store.load().unwrap().unwrap().rulings[0].alerted);
}

#[test]
fn a_torn_temporary_file_leaves_the_saved_state_readable() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    store.save(&big_state(ticket(1))).unwrap();
    let whole = serde_json::to_vec(&big_state(ticket(2))).unwrap();
    fs::write(temporary_path(&store.path), &whole[..whole.len() / 2]).unwrap();

    assert_eq!(store.load().unwrap(), Some(big_state(ticket(1))));
    store.save(&big_state(ticket(3))).unwrap();
    assert_eq!(store.load().unwrap(), Some(big_state(ticket(3))));
}

#[test]
fn a_malformed_file_names_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    fs::write(dir.path().join(STATE_FILE), "{\"version\": 1, \"run\": ").unwrap();
    let err = store.load().unwrap_err();
    assert!(matches!(&err, StateError::Malformed { path, .. } if path == &store.path));
    assert!(err.to_string().contains("state.json is malformed"));
}

#[test]
fn a_newer_format_is_reported_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let newer = VERSION + 1;
    fs::write(
        dir.path().join(STATE_FILE),
        format!(r#"{{"version": {newer}, "shape": "new"}}"#),
    )
    .unwrap();
    assert_eq!(
        store.load().unwrap_err(),
        StateError::Version {
            path: store.path.clone(),
            found: newer
        }
    );
}

#[test]
fn a_project_with_no_folder_yet_gets_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::new(dir.path().join("projects/koji/state.json"));
    let state = ProjectState::new();
    store.save(&state).unwrap();
    assert_eq!(store.load().unwrap(), Some(state));
}

#[test]
fn a_write_that_cannot_happen_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("projects"), "a file, not a folder").unwrap();
    let store = StateStore::new(dir.path().join("projects/koji/state.json"));
    let err = store.save(&ProjectState::new()).unwrap_err();
    assert!(matches!(err, StateError::Write { .. }), "{err:?}");
}

#[test]
#[ignore = "a child process of a_runner_killed_mid_write_leaves_the_previous_state_readable"]
fn writer_child() {
    let Ok(dir) = std::env::var(WRITER_DIR) else {
        return;
    };
    let store = store_in(Path::new(&dir));
    let mut out = io::stdout().lock();
    for n in 1..=u64::MAX {
        store.save(&big_state(ticket(n))).unwrap();
        writeln!(out, "saved {n}").unwrap();
        out.flush().unwrap();
    }
}

#[test]
fn a_runner_killed_mid_write_leaves_the_previous_state_readable() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let temporary = temporary_path(&store.path);
    let mut torn = 0;
    for _ in 0..8 {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::tests::writer_child",
                "--ignored",
                "--nocapture",
            ])
            .env(WRITER_DIR, dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // The reader stays open until the kill, or the child would die on
        // a closed pipe between two saves.
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let line = lines.find(|line| line.as_ref().is_ok_and(|l| l.starts_with("saved ")));
        line.expect("the writer stopped before its first save")
            .unwrap();

        // Kill once the next save has bytes in its temporary file. A save
        // that writes the state file in place never gets there.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fs::metadata(&temporary).is_ok_and(|m| m.len() > 0) {
            assert!(Instant::now() < deadline, "no save wrote a temporary file");
            std::hint::spin_loop();
        }
        child.kill().unwrap();
        child.wait().unwrap();
        drop(lines);
        torn += usize::from(temporary.exists());

        assert_whole(&store.load().unwrap().expect("a saved state"));
    }
    assert!(
        torn > 0,
        "no kill landed inside a write, so nothing was tested"
    );
}

mod answer;
mod base;
mod budget;
mod ceiling;
mod checks;
mod clock;
mod heard;
mod merge;
mod numbered;
mod pass;
mod release;
mod remote;
mod triage;
mod versions;
