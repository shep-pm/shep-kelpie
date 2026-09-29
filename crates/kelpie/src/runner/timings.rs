//! Where a work item's time goes, through the runner's stand-ins

use std::sync::Mutex;
use std::time::Duration;

use crate::ports::Checks;
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::state::{FinishedItem, StateStore};
use crate::test::{Hold, Rig, Scripted, git};
use crate::work_item::Timings;

fn saved(rig: &Rig) -> crate::state::ProjectState {
    StateStore::new(rig.paths().state)
        .load()
        .unwrap()
        .expect("a state file")
}

// A running project whose worker's first turn is held, while the worker
// pushes `work.txt` to `kelpie/7` and the clock moves 30 seconds
fn through_the_first_turn(rig: &Rig, runner: &Mutex<Runner>) {
    rig.ask(runner, "start", None);
    rig.ask(runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    let hold = Hold::default();
    rig.claude
        .script([Scripted::Hold(hold.clone()), Scripted::Text("CLEAN")]);
    std::thread::scope(|s| {
        let turn = s.spawn(|| step(runner).unwrap());
        assert!(
            hold.entered(Duration::from_secs(10)),
            "the turn never began"
        );
        rig.clock.advance(30);
        let tree = rig.worktree_7();
        std::fs::write(tree.join("work.txt"), "work\n").unwrap();
        git(&tree, &["add", "work.txt"]);
        git(&tree, &["commit", "--quiet", "-m", "work"]);
        git(&tree, &["push", "--quiet", "origin", "HEAD"]);
        hold.release();
        turn.join().unwrap();
    });
}

fn run_to_a_merge(rig: &Rig, runner: &Mutex<Runner>) -> StepReport {
    through_the_first_turn(rig, runner);
    rig.clock.advance(4);
    step(runner).unwrap(); // review round 1, qwen: clean by default
    rig.clock.advance(5);
    step(runner).unwrap(); // review round 2, claude
    let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
    rig.forge.set_checks(&head, Checks::Passed);
    rig.clock.advance(6);
    assert!(matches!(
        rig.verdict(runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    rig.clock.advance(70);
    rig.ask(runner, "rule", Some("1 yes"));
    step(runner).unwrap(); // marks the draft ready
    rig.clock.advance(CHECKS_SETTLE);
    step(runner).unwrap().expect("the merge")
}

#[test]
fn a_merged_work_item_records_a_split_that_sums_to_its_wall_time() {
    let rig = Rig::new("timings-merge");
    let runner = rig.open().unwrap();
    let report = run_to_a_merge(&rig, &runner);
    let StepReport::Finished {
        merged: true,
        timings,
        ..
    } = report
    else {
        panic!("not a finished merge: {report:?}");
    };

    let history = saved(&rig).history;
    let [entry] = history.as_slice() else {
        panic!("one finished item, not {}", history.len());
    };
    assert_eq!(entry.timings, timings);
    assert!(entry.merged && entry.pull_request == Some(71));
    let started = timings.started.expect("counted from the start");
    assert_eq!(timings.seconds.total(), entry.at.0 - started.0);
    let s = timings.seconds;
    assert!(s.worker >= 30 && s.ci > 0 && s.ruling >= 70 && s.merge >= CHECKS_SETTLE);
}

#[test]
fn a_drop_records_an_unmerged_entry() {
    let rig = Rig::new("timings-drop");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "pause", None);
    rig.clock.advance(90);
    rig.ask(&runner, "drop", None);
    let history = saved(&rig).history;
    let [
        FinishedItem {
            issue: 7,
            merged: false,
            pull_request: None,
            timings,
            ..
        },
    ] = history.as_slice()
    else {
        panic!("the drop is not recorded");
    };
    assert_eq!(timings.seconds.total(), 90);
    assert_eq!(timings.seconds.other, 90);
}

#[test]
fn timings_survive_a_reopen_and_the_time_across_it_lands_where_the_item_was_saved() {
    let (rig, runner, _) = Rig::parked("timings-restart");
    let ruling = |t: &Timings| t.seconds.ruling;
    let before = saved(&rig).work_items[0].timings;
    assert!(before.started.is_some());
    drop(runner);

    rig.clock.advance(500);
    let reopened = rig.open().unwrap();
    rig.ask(&reopened, "pause", None);
    let after = saved(&rig).work_items[0].timings;
    assert_eq!(after.started, before.started);
    assert_eq!(ruling(&after), ruling(&before) + 500);
    assert_eq!(
        after.seconds.total() - before.seconds.total(),
        500,
        "every second across the restart went to the ruling"
    );
    assert_eq!(after.seconds.worker, before.seconds.worker);
}
