use serde_json::json;

use super::*;
use crate::state::ProjectState;
use crate::test::a_work_item;
use crate::work_item::{CodeRabbitStage, Review};

fn at(seconds: u64) -> Timestamp {
    Timestamp(seconds)
}

#[test]
fn a_clock_closes_each_stretch_into_the_bucket_it_was_spent_in() {
    let mut clock = Timings::begin(at(100), Bucket::Idle);
    clock.tick(at(130), Bucket::Worker);
    clock.tick(at(190), Bucket::Ci);
    let split = clock.as_of(at(200));
    assert_eq!((split.idle, split.worker, split.ci), (30, 60, 10));
    assert_eq!(split.total(), 100, "the buckets add up to the wall time");
}

#[test]
fn a_clock_that_runs_backwards_loses_nothing_and_gains_nothing() {
    let mut clock = Timings::begin(at(100), Bucket::Worker);
    clock.tick(at(90), Bucket::Ci);
    assert_eq!(clock.as_of(at(90)).total(), 0);
    assert_eq!(clock.as_of(at(105)).ci, 5);
}

#[test]
fn the_gpu_wait_comes_out_of_the_local_round_and_never_more_than_it_ran() {
    let mut clock = Timings::begin(at(0), Bucket::LocalRound);
    clock.gpu_waited(at(60), 45);
    let split = clock.as_of(at(60));
    assert_eq!((split.gpu_wait, split.local_round), (45, 15));

    let mut clock = Timings::begin(at(0), Bucket::LocalRound);
    clock.gpu_waited(at(10), 45);
    let split = clock.as_of(at(10));
    assert_eq!((split.gpu_wait, split.local_round), (10, 0));
}

#[test]
fn a_split_adds_bucket_by_bucket() {
    let a = Split {
        worker: 1,
        ci: 2,
        ..Split::default()
    };
    let b = Split {
        worker: 10,
        idle: 5,
        ..Split::default()
    };
    let sum = a.plus(&b);
    assert_eq!((sum.worker, sum.ci, sum.idle), (11, 2, 5));
    assert_eq!(sum.total(), a.total() + b.total());
}

#[test]
fn the_clock_is_pinned() {
    let mut clock = Timings::begin(at(100), Bucket::Idle);
    clock.tick(at(107), Bucket::CodeRabbitWindow);
    assert_eq!(
        serde_json::to_value(&clock).unwrap(),
        json!({
            "started": 100,
            "since": 107,
            "bucket": "coderabbit_window",
            "split": {
                "worker": 0, "gpu_wait": 0, "local_round": 0, "claude_round": 0,
                "judging": 0, "ci": 0, "coderabbit_window": 0, "coderabbit_review": 0,
                "ruling": 0, "merge": 0, "idle": 7,
            },
        })
    );
}

// An item whose turn is over, in `phase`.
fn between_turns(phase: Phase) -> WorkItem {
    let mut item = a_work_item();
    item.turn = Turn::Ended { at: at(1) };
    item.phase = phase;
    item
}

#[test]
fn a_work_items_state_decides_its_bucket() {
    let review = |round, stage| {
        Phase::Review(Review {
            round,
            stage,
            ..Review::first()
        })
    };
    let running = ReviewCallState::Running { since: at(1) };
    let head = || "c0ffee".to_owned();
    let cases = [
        (a_work_item(), Bucket::Worker),
        (between_turns(Phase::Implement), Bucket::Idle),
        (
            between_turns(Phase::Ci {
                head: None,
                since: at(1),
            }),
            Bucket::Ci,
        ),
        (between_turns(Phase::Ruling { id: 3 }), Bucket::Ruling),
        (between_turns(Phase::Done { merged: true }), Bucket::Merge),
        (
            between_turns(Phase::Merge {
                head: head(),
                readied: None,
                auto: false,
            }),
            Bucket::Merge,
        ),
        (
            between_turns(Phase::CodeRabbit(CodeRabbitStage::Lease {
                head: head(),
                readied: None,
                full: false,
            })),
            Bucket::CodeRabbitWindow,
        ),
        (
            between_turns(Phase::CodeRabbit(CodeRabbitStage::Summoned {
                head: head(),
                at: at(1),
                full: false,
            })),
            Bucket::CodeRabbitReview,
        ),
        (
            between_turns(Phase::CodeRabbit(CodeRabbitStage::Fixing { head: head() })),
            Bucket::Idle,
        ),
        (
            between_turns(review(
                2,
                ReviewStage::Judging {
                    findings: vec![],
                    verdicts: vec![],
                },
            )),
            Bucket::Judging,
        ),
    ];
    for (item, bucket) in cases {
        assert_eq!(
            item.bucket(true),
            bucket,
            "{:?} {:?}",
            item.turn,
            item.phase
        );
    }

    let mut item = between_turns(review(1, ReviewStage::Round));
    assert_eq!(item.bucket(true), Bucket::Idle, "before its call is marked");
    item.review_call = running;
    assert_eq!(
        item.bucket(true),
        Bucket::LocalRound,
        "round 1 is the local one"
    );
    assert_eq!(
        item.bucket(false),
        Bucket::ClaudeRound,
        "without one, Claude's"
    );
    item.phase = review(2, ReviewStage::Round);
    assert_eq!(item.bucket(true), Bucket::ClaudeRound);

    let mut failed = between_turns(Phase::Implement);
    failed.turn = Turn::Failed {
        at: at(1),
        reason: "down".into(),
    };
    assert_eq!(
        failed.bucket(true),
        Bucket::Ruling,
        "a failed turn waits on the maintainer"
    );
}

fn a_finished(issue: u64, split: Split) -> Finished {
    Finished {
        issue,
        pull_request: Some(issue + 100),
        merged: true,
        started: at(0),
        ended: at(split.total()),
        split,
    }
}

#[test]
fn the_history_keeps_the_last_two_hundred() {
    let mut state = ProjectState::new(at(0));
    for issue in 1..=HISTORY_KEPT as u64 + 3 {
        state.record_finished(a_finished(issue, Split::default()));
    }
    let issues: Vec<u64> = state.history.iter().map(|f| f.issue).collect();
    assert_eq!(issues.len(), HISTORY_KEPT);
    assert_eq!(
        (issues[0], issues[HISTORY_KEPT - 1]),
        (4, HISTORY_KEPT as u64 + 3)
    );
}

#[test]
fn a_report_totals_the_last_items_newest_first() {
    let with = |ci, ruling| Split {
        ci,
        ruling,
        ..Split::default()
    };
    let history = [
        a_finished(1, with(1000, 1)),
        a_finished(2, with(20, 30)),
        a_finished(3, with(5, 7)),
    ];
    let report = Report::of(&history, 2);
    assert_eq!(report.items, 2);
    assert_eq!(
        report.rows.iter().map(|r| r.issue).collect::<Vec<_>>(),
        [3, 2]
    );
    assert_eq!((report.total.split.ci, report.total.split.ruling), (25, 37));
    assert_eq!(report.total.wall, 12 + 50);
    assert_eq!(
        Report::of(&history, 50).items,
        3,
        "asking for more covers what there is"
    );
    assert_eq!(Report::of(&[], 10).items, 0);
}

#[test]
fn the_table_names_each_item_a_total_and_shares() {
    let split = Split {
        ci: 3600 + 120,
        ruling: 60,
        ..Split::default()
    };
    let mut unmerged = a_finished(8, split);
    unmerged.merged = false;
    let report = Report::of(
        &[
            unmerged,
            a_finished(
                9,
                Split {
                    worker: 90,
                    ..Split::default()
                },
            ),
        ],
        10,
    );
    let lines: Vec<&str> = report.table.lines().collect();
    assert_eq!(lines.len(), 5, "{}", report.table);
    assert!(
        lines[0].starts_with("item") && lines[0].contains("cr window"),
        "{}",
        report.table
    );
    assert!(
        lines[1].starts_with("#9") && lines[1].contains("1m30s"),
        "{}",
        report.table
    );
    assert!(
        lines[2].starts_with("#8 (not merged)") && lines[2].contains("1h02m"),
        "{}",
        report.table
    );
    assert!(lines[3].starts_with("total"), "{}", report.table);
    assert!(
        lines[4].starts_with("share") && lines[4].contains("100%"),
        "{}",
        report.table
    );
}
