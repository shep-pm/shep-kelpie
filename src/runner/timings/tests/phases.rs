//! Which phase each call and each gate counts in, and a whole run's sum

use std::sync::Mutex;

use serde_json::json;

use super::{PATIENCE, assert_sums, held_while, read_while_held, secs, timings};
use crate::ports::Checks;
use crate::runner::{Runner, StepReport, step};
use crate::test::{Hold, Rig, Scripted, ScriptedRound};
use crate::work_item::TimingPhase;

// One work item's clock runs through a held worker turn and a held local
// round. The round queues, then runs. Then come CI, a ruling and the merge.
// Its seconds sum to its wall time at every stop and in the merge record.
// The CodeRabbit phases are covered one at a time below.
#[test]
fn a_merged_work_items_phases_sum_to_its_wall_time_at_every_stop() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    let mut stops = Vec::new();

    let turn = Hold::default();
    rig.claude.script([Scripted::Hold(turn.clone())]);
    let push = || {
        let worktree = rig.worktree_7();
        std::fs::write(worktree.join("work.txt"), "work\n").unwrap();
        crate::test::git(&worktree, &["add", "work.txt"]);
        crate::test::git(&worktree, &["commit", "--quiet", "-m", "work"]);
        crate::test::git(&worktree, &["push", "--quiet", "origin", "HEAD"]);
    };
    let t = held_while(&rig, &runner, &turn, 100, push);
    assert_eq!(
        (t["phase"].as_str(), secs(&t, "worker")),
        (Some("worker"), 100)
    );
    stops.push(t);

    let (queued, running) = (Hold::default(), Hold::default());
    rig.reviewer.script([ScriptedRound::Queued {
        queued: queued.clone(),
        running: running.clone(),
    }]);
    std::thread::scope(|scope| {
        let round = scope.spawn(|| step(&runner));
        assert!(queued.entered(PATIENCE), "the round never queued");
        rig.clock.advance(20);
        stops.push(timings(&rig, &runner));
        queued.release();
        assert!(running.entered(PATIENCE), "the round never ran");
        rig.clock.advance(30);
        stops.push(timings(&rig, &runner));
        running.release();
        round.join().unwrap().unwrap();
    });
    rig.claude.script([Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // round 2, Claude: clean

    rig.clock.advance(300);
    stops.push(timings(&rig, &runner));
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    rig.clock.advance(1_200);
    stops.push(timings(&rig, &runner));
    rig.ask(&runner, "rule", Some("1 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(crate::runner::CHECKS_SETTLE);
    stops.push(timings(&rig, &runner));
    let Some(StepReport::Finished {
        timings: split,
        merged: true,
        ..
    }) = step(&runner).unwrap()
    else {
        panic!("the yes did not merge it");
    };

    let phases: Vec<_> = stops.iter().map(|t| t["phase"].as_str().unwrap()).collect();
    assert_eq!(
        phases,
        ["worker", "gpu_wait", "local_round", "ci", "ruling", "merge"]
    );
    for stop in &stops {
        assert_sums(stop);
    }
    assert_eq!(split.wall, split.seconds.total());
    let wall = crate::ports::Clock::now(&rig.clock).0 - Rig::EPOCH;
    assert_eq!(split.wall, wall);
    let got = |phase| split.seconds.get(phase);
    assert_eq!(got(TimingPhase::Worker), 100);
    assert_eq!(got(TimingPhase::GpuWait), 20);
    assert_eq!(got(TimingPhase::LocalRound), 30);
    assert_eq!(got(TimingPhase::Ruling), 1_200);
    assert!(got(TimingPhase::Ci) >= 300);
    assert!(got(TimingPhase::Merge) >= crate::runner::CHECKS_SETTLE);
    let t = rig.ask(&runner, "timings", None);
    assert_eq!(
        (&t["items"], &t["issues"], &t["wall"]),
        (&json!(1), &json!([7]), &json!(split.wall))
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["history"][0]["wall"],
        json!(split.wall)
    );
}

#[test]
fn a_gpu_wait_is_counted_apart_from_the_round() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn opens the pull request
    let (queued, running) = (Hold::default(), Hold::default());
    rig.reviewer.script([ScriptedRound::Queued {
        queued: queued.clone(),
        running: running.clone(),
    }]);
    std::thread::scope(|scope| {
        let round = scope.spawn(|| step(&runner));
        assert!(queued.entered(PATIENCE), "the round never queued");
        rig.clock.advance(120);
        let t = timings(&rig, &runner);
        assert_eq!(t["phase"], "gpu_wait");
        assert_eq!((secs(&t, "gpu_wait"), secs(&t, "local_round")), (120, 0));
        assert_sums(&t);

        queued.release();
        assert!(running.entered(PATIENCE), "the round never ran");
        rig.clock.advance(45);
        let t = timings(&rig, &runner);
        assert_eq!(t["phase"], "local_round");
        assert_eq!((secs(&t, "gpu_wait"), secs(&t, "local_round")), (120, 45));
        assert_sums(&t);

        running.release();
        round.join().unwrap().unwrap();
    });
    let t = timings(&rig, &runner);
    assert_eq!((secs(&t, "gpu_wait"), secs(&t, "local_round")), (120, 45));
    assert_eq!(
        t["phase"], "other",
        "the round is over, and no call is running"
    );
    assert_sums(&t);
}

// A running project whose worker opened pull request 71. Its local round
// came back clean, so the next round is Claude's.
fn at_the_claude_round() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, local: clean by default
    (rig, runner)
}

#[test]
fn a_claude_round_in_flight_is_claude_round() {
    let (rig, runner) = at_the_claude_round();
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let t = read_while_held(&rig, &runner, &hold, 90);
    assert_eq!(t["phase"], "claude_round");
    assert_eq!(secs(&t, "claude_round"), 90);
    assert_sums(&t);
    let t = timings(&rig, &runner);
    assert_eq!(
        (t["phase"].as_str(), secs(&t, "claude_round")),
        (Some("other"), 90)
    );
    assert_sums(&t);
}

#[test]
fn a_second_look_in_flight_is_a_reviewers_session() {
    let rig = Rig::new("koji");
    rig.default_review();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, local: clean by default
    rig.claude.script([Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // round 2, defect-hunter's first look
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let t = read_while_held(&rig, &runner, &hold, 90);
    assert_eq!(t["phase"], "claude_round");
    assert_eq!(secs(&t, "claude_round"), 90);
    assert_sums(&t);
}

#[test]
fn coderabbit_is_the_window_until_the_summon_and_the_review_after() {
    let rig = Rig::new("koji");
    rig.coderabbit_on();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, local: clean by default
    step(&runner).unwrap(); // round 2, Claude: clean
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.leases.withhold(true);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    assert_eq!(step(&runner).unwrap(), None, "the lease is not granted");

    rig.clock.advance(300);
    let t = timings(&rig, &runner);
    assert_eq!(
        (t["phase"].as_str(), secs(&t, "coderabbit_window")),
        (Some("coderabbit_window"), 300)
    );
    assert_sums(&t);

    rig.leases.withhold(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Summoned { .. })
    ));
    rig.clock.advance(700);
    let t = timings(&rig, &runner);
    assert_eq!(t["phase"], "coderabbit_review");
    assert_eq!(
        (secs(&t, "coderabbit_window"), secs(&t, "coderabbit_review")),
        (300, 700)
    );
    assert_sums(&t);
}
