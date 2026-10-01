use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::*;
use crate::test::a_work_item;
use crate::work_item::TimingPhase;

const WRITER_DIR: &str = "KELPIE_TEST_WRITER_DIR";

fn store_in(dir: &Path) -> StateStore {
    StateStore::new(dir.join("state.json"))
}

// Large, so a kill is likely to land inside a write, and uniform, so a
// mix of two saves is visible.
fn big_state(n: u64) -> ProjectState {
    let mut state = ProjectState::new(Timestamp(n));
    state.rulings = (0..20_000)
        .map(|id| Ruling {
            id,
            issue: Some(n),
            question: format!("question {n}"),
            pull_request: Some(n),
            kind: RulingKind::Closed,
            alerted: false,
            relayed: false,
            resend: false,
        })
        .collect();
    state
}

fn assert_whole(state: &ProjectState) {
    let n = state.since.0;
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
    let mut state = ProjectState::new(Timestamp(1_790_000_000));
    state.run = RunState::Running;
    state.work_items = vec![a_work_item()];
    state.rulings.push(Ruling {
        id: 1,
        issue: Some(7),
        question: "merge #43?".into(),
        pull_request: Some(43),
        kind: RulingKind::Merge {
            head: "c0ffee".into(),
            shots_failed: false,
        },
        alerted: true,
        relayed: true,
        resend: false,
    });
    state.last_ruling = 1;
    state.leases.push(LeaseHeld {
        resource: Resource::Gpu,
        issue: Some(7),
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
    fs::write(dir.path().join("state.json"), one.to_string()).unwrap();
    let state = store.load().unwrap().unwrap();
    assert_eq!(state.work_items, [a_work_item()]);
    assert_eq!(
        state.rulings[0].issue,
        Some(42),
        "its rulings are its item's"
    );

    store.save(&state).unwrap();
    let text = fs::read_to_string(dir.path().join("state.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (&saved["version"], saved.get("work_item")),
        (&serde_json::json!(2), None)
    );
    assert_eq!(store.load().unwrap(), Some(state));

    one["work_item"] = serde_json::Value::Null;
    fs::write(dir.path().join("state.json"), one.to_string()).unwrap();
    let state = store.load().unwrap().unwrap();
    assert_eq!((state.work_items, state.rulings[0].issue), (vec![], None));
}

#[test]
fn a_file_saved_before_pacing_loads_with_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let old =
        r#"{"version":1,"run":"running","since":7,"work_item":null,"rulings":[],"leases":[]}"#;
    fs::write(dir.path().join("state.json"), old).unwrap();
    let state = store.load().unwrap().unwrap();
    assert_eq!((state.run, state.pacing), (RunState::Running, None));
}

#[test]
fn the_file_format_is_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let mut state = ProjectState::new(Timestamp(7));
    state.leases.push(LeaseHeld {
        resource: Resource::Coderabbit,
        issue: Some(22),
        since: Timestamp(8),
    });
    let ruling = |id, kind| Ruling {
        id,
        issue: Some(22),
        question: "q".into(),
        pull_request: Some(30),
        kind,
        alerted: id.is_multiple_of(2),
        relayed: id.is_multiple_of(3),
        resend: false,
    };
    state.rulings = vec![
        ruling(
            1,
            RulingKind::Merge {
                head: "c0ffee".into(),
                shots_failed: true,
            },
        ),
        ruling(
            2,
            RulingKind::Rebase {
                reason: "conflicts".into(),
            },
        ),
        ruling(
            3,
            RulingKind::StillRed {
                head: "bad".into(),
                checks: vec!["lint".into()],
            },
        ),
        ruling(4, RulingKind::Closed),
        ruling(
            5,
            RulingKind::Question {
                asked: "Which name?".into(),
                resume: Resume::Nothing,
            },
        ),
        ruling(6, RulingKind::TurnTimeout { phase: None }),
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
            RulingKind::MergeRefused {
                head: "c0ffee".into(),
                reason: "a ruleset".into(),
            },
        ),
    ];
    state.last_ruling = 8;
    state.notices = vec![Notice {
        issue: 22,
        pull_request: 30,
        head: "c0ffee".into(),
        shots_failed: true,
    }];
    state.finished = vec![22, 30];
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
    let text = fs::read_to_string(dir.path().join("state.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let pinned = |id: u64, kind| {
        serde_json::json!({
            "id": id,
            "issue": 22,
            "question": "q",
            "pull_request": 30,
            "kind": kind,
            "alerted": id.is_multiple_of(2),
            "relayed": id.is_multiple_of(3),
        })
    };
    assert_eq!(
        value,
        serde_json::json!({
            "version": 2,
            "run": "paused",
            "since": 7,
            "work_items": [],
            "rulings": [
                pinned(1, serde_json::json!({ "kind": "merge", "head": "c0ffee", "shots_failed": true })),
                pinned(2, serde_json::json!({ "kind": "rebase", "reason": "conflicts" })),
                pinned(3, serde_json::json!({ "kind": "still-red", "head": "bad", "checks": ["lint"] })),
                pinned(4, serde_json::json!({ "kind": "closed" })),
                pinned(
                    5,
                    serde_json::json!({
                        "kind": "question",
                        "asked": "Which name?",
                        "resume": { "state": "nothing" },
                    }),
                ),
                pinned(6, serde_json::json!({ "kind": "turn-timeout" })),
                pinned(7, serde_json::json!({
                    "kind": "foreign-change",
                    "description": "the `bug` label was added",
                    "known": { "labels": ["bug"], "ready": false, "head": "c0ffee" },
                })),
                pinned(8, serde_json::json!({
                    "kind": "merge-refused",
                    "head": "c0ffee",
                    "reason": "a ruleset",
                })),
            ],
            "last_ruling": 8,
            "finished": [22, 30],
            "history": [],
            "reworked": ["PRR_1"],
            "adopted": [
                { "pull_request": 614, "by_label": false },
                { "pull_request": 638, "by_label": true },
            ],
            "leases": [{ "resource": "coderabbit", "issue": 22, "since": 8 }],
            "pacing": { "week_resets_at": 9, "day": 1, "week_used_pct": 10 },
            "notices": [{ "issue": 22, "pull_request": 30, "head": "c0ffee", "shots_failed": true }],
            "replies": { "last": { "id": "W3EqiUm5rsNq", "time": 5 } },
        })
    );
}

#[test]
fn the_history_keeps_the_last_hundred_finished_work_items() {
    let mut state = ProjectState::new(Timestamp(1));
    let finished = |issue: u64| Finished {
        issue,
        title: format!("Issue {issue}"),
        pull_request: None,
        merged: true,
        at: Timestamp(issue),
        wall: 0,
        seconds: Seconds::default(),
    };
    let last = HISTORY_CAP as u64 + 5;
    for issue in 1..=last {
        state.record_finished(finished(issue));
    }
    let issues: Vec<_> = state.history.iter().map(|f| f.issue).collect();
    assert_eq!(issues, (6..=last).collect::<Vec<_>>());
}

#[test]
fn a_finished_work_item_is_pinned() {
    let finished = Finished {
        issue: 7,
        title: "Seven".into(),
        pull_request: Some(71),
        merged: true,
        at: Timestamp(1_790_000_100),
        wall: 100,
        seconds: Seconds::of(&[(TimingPhase::Worker, 60), (TimingPhase::Ci, 40)]),
    };
    let saved = serde_json::to_value(&finished).unwrap();
    assert_eq!(serde_json::from_value::<Finished>(saved).unwrap(), finished);
    assert_eq!(
        serde_json::to_value(&finished).unwrap(),
        serde_json::json!({
            "issue": 7,
            "title": "Seven",
            "pull_request": 71,
            "merged": true,
            "at": 1_790_000_100,
            "wall": 100,
            "seconds": {
                "worker": 60,
                "gpu_wait": 0,
                "local_round": 0,
                "claude_round": 0,
                "judging": 0,
                "ci": 40,
                "coderabbit_window": 0,
                "coderabbit_review": 0,
                "ruling": 0,
                "merge": 0,
                "shots": 0,
                "paused": 0,
                "other": 0,
            },
        })
    );
}

#[test]
fn the_coderabbit_rulings_are_pinned() {
    let cap = RulingKind::CodeRabbitCap {
        rounds: 2,
        held: 1,
        prompt: "fix".into(),
        head: Some("c0ffee".into()),
    };
    let silent = RulingKind::CodeRabbitSilent {
        bot: crate::review_bot::Bot::Coderabbit,
        head: "c0ffee".into(),
    };
    let unpushed = RulingKind::FixNotPushed {
        fix: Fix::CodeRabbit {
            round: 3,
            head: "c0ffee".into(),
        },
        prompt: "again".into(),
    };
    for kind in [&cap, &silent, &unpushed] {
        let saved = serde_json::to_value(kind).unwrap();
        assert_eq!(&serde_json::from_value::<RulingKind>(saved).unwrap(), kind);
    }
    assert_eq!(
        serde_json::to_value([&cap, &silent, &unpushed]).unwrap(),
        serde_json::json!([
            {
                "kind": "coderabbit-cap",
                "rounds": 2,
                "held": 1,
                "prompt": "fix",
                "head": "c0ffee",
            },
            { "kind": "coderabbit-silent", "head": "c0ffee" },
            {
                "kind": "fix-not-pushed",
                "coderabbit": { "round": 3, "head": "c0ffee" },
                "prompt": "again",
            },
        ])
    );
    let saved_before_the_head: RulingKind = serde_json::from_value(serde_json::json!(
        { "kind": "coderabbit-cap", "rounds": 2, "held": 1, "prompt": "fix" }
    ))
    .unwrap();
    assert_eq!(
        saved_before_the_head,
        RulingKind::CodeRabbitCap {
            rounds: 2,
            held: 1,
            prompt: "fix".into(),
            head: None,
        }
    );
}

#[test]
fn a_timeout_keeps_the_phase_its_turn_ran_in() {
    let kind = RulingKind::TurnTimeout {
        phase: Some(Phase::Implement),
    };
    let saved = serde_json::to_value(&kind).unwrap();
    assert_eq!(
        saved,
        serde_json::json!({ "kind": "turn-timeout", "phase": { "state": "implement" } })
    );
    assert_eq!(serde_json::from_value::<RulingKind>(saved).unwrap(), kind);
}

#[test]
fn a_failed_turn_keeps_its_phase_and_the_turn_to_retry() {
    let kind = RulingKind::TurnFailed {
        reason: "no worktree".into(),
        phase: Phase::Implement,
        retry: Turn::Due,
    };
    let saved = serde_json::to_value(&kind).unwrap();
    assert_eq!(
        saved,
        serde_json::json!({
            "kind": "turn-failed",
            "reason": "no worktree",
            "phase": { "state": "implement" },
            "retry": { "state": "due" },
        })
    );
    assert_eq!(serde_json::from_value::<RulingKind>(saved).unwrap(), kind);
}

#[test]
fn a_change_to_claudes_files_keeps_its_head_files_and_phase() {
    let kind = RulingKind::ClaudeFiles {
        head: "c0ffee".into(),
        files: vec![".mcp.json".into()],
        phase: Phase::Implement,
    };
    let saved = serde_json::to_value(&kind).unwrap();
    assert_eq!(
        saved,
        serde_json::json!({
            "kind": "claude-files",
            "head": "c0ffee",
            "files": [".mcp.json"],
            "phase": { "state": "implement" },
        })
    );
    assert_eq!(serde_json::from_value::<RulingKind>(saved).unwrap(), kind);
}

#[test]
fn a_question_during_a_coderabbit_fix_is_pinned() {
    let resume = Resume::CodeRabbitFix {
        head: "c0ffee".into(),
    };
    let saved = serde_json::to_value(&resume).unwrap();
    assert_eq!(
        saved,
        serde_json::json!({ "state": "coderabbit-fix", "head": "c0ffee" })
    );
    assert_eq!(serde_json::from_value::<Resume>(saved).unwrap(), resume);
}

#[test]
fn a_qwen_fix_not_pushed_keeps_its_wire_shape() {
    let saved = serde_json::json!({
        "kind": "fix-not-pushed",
        "review": {
            "round": 1,
            "consecutive_clean": 0,
            "guard_cleared": false,
            "stage": { "stage": "fixing", "clean": false, "head": "c0ffee" },
        },
        "prompt": "again",
    });
    let kind: RulingKind = serde_json::from_value(saved.clone()).unwrap();
    let RulingKind::FixNotPushed {
        fix: Fix::Review(review),
        ..
    } = &kind
    else {
        panic!("read as {kind:?}");
    };
    assert_eq!(review.round, 1);
    assert_eq!(serde_json::to_value(&kind).unwrap(), saved);
    let stray = serde_json::json!({
        "kind": "fix-not-pushed",
        "coderabbit": { "round": 3, "head": "c0ffee" },
        "prompt": "again",
        "extra": true,
    });
    assert!(serde_json::from_value::<RulingKind>(stray).is_err());
    let misspelt = serde_json::json!({
        "kind": "fix-not-pushed",
        "coderabbit": { "round": 3, "head": "c0ffee", "heade": "c0ffee" },
        "prompt": "again",
    });
    assert!(serde_json::from_value::<RulingKind>(misspelt).is_err());
}

#[test]
fn a_state_saved_before_ruling_ids_were_counted_starts_counting_at_zero() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    fs::write(
        dir.path().join("state.json"),
        r#"{"version":1,"run":"running","since":3,"work_item":null,"rulings":[],"leases":[]}"#,
    )
    .unwrap();
    let state = store.load().unwrap().unwrap();
    assert_eq!((state.last_ruling, state.finished), (0, vec![]));
    assert!(state.reworked.is_empty());
}

#[test]
fn a_ruling_saved_before_webhooks_is_posted_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    let old = r#"{"version":1,"run":"running","since":7,"work_item":null,
        "rulings":[{"id":1,"question":"q","pull_request":3,"kind":{"kind":"closed"}}],
        "leases":[]}"#;
    fs::write(dir.path().join("state.json"), old).unwrap();
    assert!(!store.load().unwrap().unwrap().rulings[0].alerted);
}

#[test]
fn a_torn_temporary_file_leaves_the_saved_state_readable() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    store.save(&big_state(1)).unwrap();
    let whole = serde_json::to_vec(&big_state(2)).unwrap();
    fs::write(temporary_path(&store.path), &whole[..whole.len() / 2]).unwrap();

    assert_eq!(store.load().unwrap(), Some(big_state(1)));
    store.save(&big_state(3)).unwrap();
    assert_eq!(store.load().unwrap(), Some(big_state(3)));
}

#[test]
fn a_malformed_file_names_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    fs::write(dir.path().join("state.json"), "{\"version\": 1, \"run\": ").unwrap();
    let err = store.load().unwrap_err();
    assert!(matches!(&err, StateError::Malformed { path, .. } if path == &store.path));
    assert!(err.to_string().contains("state.json is malformed"));
}

#[test]
fn a_newer_format_is_reported_as_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(dir.path());
    fs::write(
        dir.path().join("state.json"),
        r#"{"version": 3, "shape": "new"}"#,
    )
    .unwrap();
    assert_eq!(
        store.load().unwrap_err(),
        StateError::Version {
            path: store.path.clone(),
            found: 3
        }
    );
}

#[test]
fn a_project_with_no_folder_yet_gets_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::new(dir.path().join("projects/koji/state.json"));
    let state = ProjectState::new(Timestamp(1));
    store.save(&state).unwrap();
    assert_eq!(store.load().unwrap(), Some(state));
}

#[test]
fn a_write_that_cannot_happen_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("projects"), "a file, not a folder").unwrap();
    let store = StateStore::new(dir.path().join("projects/koji/state.json"));
    let err = store.save(&ProjectState::new(Timestamp(1))).unwrap_err();
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
    for n in 0..=u64::MAX {
        store.save(&big_state(n)).unwrap();
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
