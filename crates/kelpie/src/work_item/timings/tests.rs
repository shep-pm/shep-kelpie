use serde_json::json;

use super::*;
use crate::test::a_work_item;
use crate::work_item::Review;

fn split(pairs: &[(TimingPhase, u64)]) -> Timings {
    let mut timings = Timings::starting(Timestamp(100));
    for &(phase, seconds) in pairs {
        timings.seconds.add(phase, seconds);
    }
    timings
}

#[test]
fn all_matches_the_serialized_key_order_and_as_str_the_keys() {
    let text = serde_json::to_string(&PhaseSeconds::default()).unwrap();
    let keys: Vec<&str> = text
        .trim_matches(['{', '}'])
        .split(',')
        .map(|pair| pair.split(':').next().unwrap().trim_matches('"'))
        .collect();
    let named: Vec<&str> = TimingPhase::ALL.iter().map(|p| p.as_str()).collect();
    assert_eq!(keys, named);
    for phase in TimingPhase::ALL {
        assert_eq!(serde_json::to_value(phase).unwrap(), json!(phase.as_str()));
    }
}

#[test]
fn every_phase_is_serialized_and_an_unknown_one_is_refused() {
    let zeros: serde_json::Value = serde_json::to_value(PhaseSeconds::default()).unwrap();
    assert_eq!(zeros.as_object().unwrap().len(), TimingPhase::ALL.len());
    assert!(zeros.as_object().unwrap().values().all(|v| v == 0));
    assert!(serde_json::from_value::<PhaseSeconds>(json!({ "napping": 1 })).is_err());
    let sparse: PhaseSeconds = serde_json::from_value(json!({ "ci": 4 })).unwrap();
    assert_eq!((sparse.ci, sparse.total()), (4, 4));
}

#[test]
fn timings_serialize_their_marks_only_once_set() {
    assert_eq!(
        serde_json::to_value(Timings::default()).unwrap(),
        json!({ "seconds": PhaseSeconds::default() })
    );
    let started = serde_json::to_value(Timings::starting(Timestamp(5))).unwrap();
    assert_eq!(
        (&started["started"], &started["charged"]),
        (&json!(5), &json!(5))
    );
}

#[test]
fn a_charge_with_no_mark_adds_nothing_and_sets_the_mark() {
    let mut timings = Timings::default();
    timings.charge(TimingPhase::Ci, Timestamp(50));
    assert_eq!(timings.started, Some(Timestamp(50)));
    assert_eq!(timings.charged, Some(Timestamp(50)));
    assert_eq!(timings.seconds.total(), 0);
}

#[test]
fn a_clock_that_goes_back_adds_nothing_and_never_moves_the_mark_back() {
    let mut timings = Timings::starting(Timestamp(100));
    timings.charge(TimingPhase::Worker, Timestamp(160));
    timings.charge(TimingPhase::Worker, Timestamp(120));
    assert_eq!(timings.charged, Some(Timestamp(160)));
    assert_eq!(timings.seconds.worker, 60);
    timings.charge(TimingPhase::Ci, Timestamp(170));
    assert_eq!((timings.seconds.worker, timings.seconds.ci), (60, 10));
}

#[test]
fn at_charges_a_copy_and_leaves_the_original() {
    let timings = Timings::starting(Timestamp(100));
    let later = timings.at(TimingPhase::Merge, Timestamp(130));
    assert_eq!(timings, Timings::starting(Timestamp(100)));
    assert_eq!(later.seconds.merge, 30);
    assert_eq!(later.charged, Some(Timestamp(130)));
}

#[test]
fn seconds_always_total_the_time_since_the_start() {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..200 {
        let start = next() % 1_000_000;
        let mut timings = Timings::starting(Timestamp(start));
        let mut now = start;
        for _ in 0..50 {
            now += next() % 500;
            let phase = TimingPhase::ALL[usize::try_from(next() % 12).unwrap()];
            let before = timings;
            let view = timings.at(phase, Timestamp(now));
            assert_eq!(timings, before);
            timings.charge(phase, Timestamp(now));
            assert_eq!(timings, view);
            assert_eq!(timings.seconds.total(), now - start);
        }
    }
}

#[test]
fn reassign_moves_at_most_what_the_source_holds_and_keeps_the_total() {
    let mut timings = split(&[(TimingPhase::LocalRound, 40), (TimingPhase::Ci, 5)]);
    timings.reassign(TimingPhase::LocalRound, TimingPhase::GpuWait, 15);
    assert_eq!(
        (timings.seconds.local_round, timings.seconds.gpu_wait),
        (25, 15)
    );
    timings.reassign(TimingPhase::LocalRound, TimingPhase::GpuWait, 1_000);
    assert_eq!(
        (timings.seconds.local_round, timings.seconds.gpu_wait),
        (0, 40)
    );
    assert_eq!(timings.seconds.total(), 45);
}

#[test]
fn phase_seconds_add_field_by_field() {
    let mut a = PhaseSeconds::default();
    a.add(TimingPhase::Worker, 3);
    let mut b = PhaseSeconds::default();
    b.add(TimingPhase::Worker, 4);
    b.add(TimingPhase::Other, 1);
    a += b;
    assert_eq!(
        (a.get(TimingPhase::Worker), a.get(TimingPhase::Other)),
        (7, 1)
    );
}

fn round_of(stage: ReviewStage) -> Phase {
    Phase::Review(Review {
        round: 1,
        consecutive_clean: 0,
        guard_cleared: false,
        stage,
    })
}

fn judging() -> ReviewStage {
    ReviewStage::Judging {
        findings: vec![],
        verdicts: vec![],
    }
}

fn in_ci() -> Phase {
    Phase::Ci {
        head: None,
        since: Timestamp(1),
    }
}

fn coderabbit(stage: CodeRabbitStage) -> Phase {
    Phase::CodeRabbit(stage)
}

fn head() -> String {
    "c0ffee".into()
}

#[test]
fn each_rule_of_the_classifier_holds() {
    use ReviewCallKind::{Claude, Judge, Local, Shots};
    use TimingPhase as T;
    let call = |kind| ReviewCallState::Running {
        since: Timestamp(1),
        kind,
    };
    let idle = || ReviewCallState::Idle;
    let running = || Turn::Running {
        since: Timestamp(1),
    };
    let cases: Vec<(&str, Turn, ReviewCallState, Phase, TimingPhase)> = vec![
        (
            "a running turn",
            running(),
            idle(),
            Phase::Implement,
            T::Worker,
        ),
        (
            "a turn beats a call",
            running(),
            call(Some(Judge)),
            in_ci(),
            T::Worker,
        ),
        (
            "a local round",
            Turn::Due,
            call(Some(Local)),
            round_of(ReviewStage::Round),
            T::LocalRound,
        ),
        (
            "a claude round",
            Turn::Due,
            call(Some(Claude)),
            round_of(ReviewStage::Round),
            T::ClaudeRound,
        ),
        (
            "a judge call",
            Turn::Due,
            call(Some(Judge)),
            round_of(judging()),
            T::Judging,
        ),
        (
            "a shots run",
            Turn::Due,
            call(Some(Shots)),
            round_of(ReviewStage::Round),
            T::Shots,
        ),
        (
            "a call with no kind",
            Turn::Due,
            call(None),
            in_ci(),
            T::Other,
        ),
        ("CI", Turn::Due, idle(), in_ci(), T::Ci),
        (
            "the CodeRabbit lease",
            Turn::Due,
            idle(),
            coderabbit(CodeRabbitStage::Lease {
                head: head(),
                readied: None,
                full: false,
            }),
            T::CodeRabbitWindow,
        ),
        (
            "a summoned review",
            Turn::Due,
            idle(),
            coderabbit(CodeRabbitStage::Summoned {
                head: head(),
                at: Timestamp(1),
                full: false,
            }),
            T::CodeRabbitReview,
        ),
        (
            "CodeRabbit's threads awaiting the judge",
            Turn::Due,
            idle(),
            coderabbit(CodeRabbitStage::Judging {
                head: head(),
                threads: vec![],
                verdicts: vec![],
            }),
            T::Judging,
        ),
        (
            "findings awaiting the judge",
            Turn::Due,
            idle(),
            round_of(judging()),
            T::Judging,
        ),
        (
            "a ruling",
            Turn::Due,
            idle(),
            Phase::Ruling { id: 1 },
            T::Ruling,
        ),
        (
            "a merge",
            Turn::Due,
            idle(),
            Phase::Merge {
                head: head(),
                readied: None,
                auto: false,
            },
            T::Merge,
        ),
        (
            "the cleanup after a merge",
            Turn::Due,
            idle(),
            Phase::Done { merged: true },
            T::Merge,
        ),
        (
            "a turn waiting to start",
            Turn::Due,
            idle(),
            Phase::Implement,
            T::Other,
        ),
        (
            "a round waiting to start",
            Turn::Due,
            idle(),
            round_of(ReviewStage::Round),
            T::Other,
        ),
        (
            "a review fix waiting for its turn",
            Turn::Due,
            idle(),
            round_of(ReviewStage::Fixing {
                clean: true,
                head: None,
            }),
            T::Other,
        ),
        (
            "a CodeRabbit fix waiting for its turn",
            Turn::Due,
            idle(),
            coderabbit(CodeRabbitStage::Fixing { head: head() }),
            T::Other,
        ),
    ];
    for (name, turn, review_call, phase, expected) in cases {
        let mut item = a_work_item();
        (item.turn, item.review_call, item.phase) = (turn, review_call, phase);
        assert_eq!(item.timing_phase(), expected, "{name}");
    }
}

#[test]
fn a_work_items_timings_at_and_charge_time_go_by_its_phase() {
    let mut item = a_work_item();
    item.turn = Turn::Due;
    item.phase = Phase::Ruling { id: 1 };
    item.timings = Timings::starting(Timestamp(10));
    assert_eq!(item.timings_at(Timestamp(25)).seconds.ruling, 15);
    assert_eq!(item.timings.seconds.total(), 0);
    item.charge_time(Timestamp(25));
    assert_eq!(item.timings.seconds.ruling, 15);
}
