//! The state file's shape, pinned as the JSON it saves

use super::store_in;
use super::*;

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
