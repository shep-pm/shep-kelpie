//! A work item's time through the runner's loop, with the stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use super::Bucket;
use crate::ports::{Checks, PullRequestState};
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Rig, Scripted, ScriptedRound};

// A running project with a local round, an open pull request 71 for issue 7,
// and its worker's first turn about to run
fn ready(rig: &Rig) -> Mutex<Runner> {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    runner
}

fn split_of(status: &Value) -> Value {
    status["work_item"]["split"].clone()
}

#[test]
fn a_work_items_time_is_split_by_where_it_went_and_adds_up_to_its_wall_time() {
    let rig = Rig::new("shep");
    let runner = ready(&rig);
    rig.clock.advance(30); // waiting for its first turn
    rig.claude.script([
        Scripted::Slow(100, Box::new(Scripted::Push("work.txt", "work\n"))),
        Scripted::Slow(50, Box::new(Scripted::Text("CLEAN"))),
    ]);
    rig.reviewer.script([ScriptedRound::Slow {
        gpu_wait: 40,
        run: 25,
        findings: vec![],
    }]);
    step(&runner).unwrap(); // the worker's turn
    step(&runner).unwrap(); // round 1, the local one
    step(&runner).unwrap(); // round 2, Claude's, clean
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    rig.clock.advance(90);
    assert!(
        step(&runner).unwrap().is_none(),
        "the checks have to settle"
    );
    rig.clock.advance(CHECKS_SETTLE);
    let Some(StepReport::Ruling { id, .. }) = step(&runner).unwrap() else {
        panic!("CI passing did not raise the merge ruling");
    };
    rig.clock.advance(600); // the maintainer is away
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    rig.clock.advance(7);
    step(&runner).unwrap(); // marks the draft ready
    rig.forge.set_state(71, PullRequestState::Merged);
    let Some(StepReport::Finished { split, .. }) = step(&runner).unwrap() else {
        panic!("the merged work item did not finish");
    };

    let wall = 30 + 100 + 65 + 50 + CHECKS_SETTLE + 90 + 600 + 7;
    let timings = rig.ask(&runner, "timings", None);
    let row = &timings["rows"][0];
    assert_eq!(row["issue"], 7);
    assert_eq!(row["wall"], wall);
    assert_eq!(
        row["split"],
        json!({
            "worker": 100, "gpu_wait": 40, "local_round": 25, "claude_round": 50,
            "judging": 0, "ci": CHECKS_SETTLE + 90, "coderabbit_window": 0,
            "coderabbit_review": 0, "ruling": 600, "merge": 7, "idle": 30,
        })
    );
    assert_eq!(serde_json::to_value(split).unwrap(), row["split"]);
    let sum: u64 = row["split"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(sum, wall, "the phases add up to the work item's wall time");
}

#[test]
fn a_gpu_wait_is_counted_apart_from_the_round_that_waited() {
    let rig = Rig::new("shep");
    let runner = ready(&rig);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    rig.reviewer.script([ScriptedRound::Slow {
        gpu_wait: 300,
        run: 20,
        findings: vec![],
    }]);
    step(&runner).unwrap(); // the worker's turn
    step(&runner).unwrap(); // round 1, which queued for the GPU

    let split = split_of(&rig.ask(&runner, "status", None));
    assert_eq!(
        (&split["gpu_wait"], &split["local_round"]),
        (&json!(300), &json!(20))
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["qwen"]["seconds"],
        320,
        "the tally of qwen rounds still counts the whole round"
    );
}

#[test]
fn a_round_that_took_no_lock_waited_for_nothing() {
    let rig = Rig::new("shep");
    let runner = ready(&rig);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    rig.reviewer.script([ScriptedRound::Slow {
        gpu_wait: 0,
        run: 80,
        findings: vec![],
    }]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let split = split_of(&rig.ask(&runner, "status", None));
    assert_eq!(
        (&split["gpu_wait"], &split["local_round"]),
        (&json!(0), &json!(80))
    );
}

#[test]
fn status_shows_the_running_stretch_and_a_restart_keeps_the_split() {
    let rig = Rig::new("shep");
    let runner = ready(&rig);
    rig.clock.advance(45);
    assert_eq!(split_of(&rig.ask(&runner, "status", None))["idle"], 45);
    assert_eq!(rig.ask(&runner, "status", None)["work_item"]["wall"], 45);
    rig.claude.script([Scripted::Slow(
        10,
        Box::new(Scripted::Push("work.txt", "work\n")),
    )]);
    step(&runner).unwrap();
    drop(runner);

    rig.clock.advance(5); // down, in the bucket it was in
    let runner = rig.open().unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["wall"], 60);
    assert_eq!(split_of(&status)["worker"], 10);
    assert_eq!(split_of(&status)["idle"], 50);
}

#[test]
fn a_work_item_saved_before_timings_starts_its_clock_at_the_next_save() {
    let rig = Rig::new("shep");
    let runner = ready(&rig);
    drop(runner);
    let state = rig.paths().state;
    let mut text: Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    assert!(
        text["work_items"][0]
            .as_object_mut()
            .unwrap()
            .remove("timings")
            .is_some()
    );
    std::fs::write(&state, text.to_string()).unwrap();

    rig.clock.advance(500);
    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["split"],
        Value::Null
    );
    rig.ask(&runner, "pause", None); // any save
    rig.clock.advance(20);
    assert_eq!(rig.ask(&runner, "status", None)["work_item"]["wall"], 20);
}

// Runs issue `issue` from add to merge, with `ci` seconds in CI and `ruling` on the maintainer.
fn merge_one(rig: &Rig, runner: &Mutex<Runner>, issue: u64, pr: u64, ci: u64, ruling: u64) {
    rig.ask(runner, "add", Some(&issue.to_string()));
    let branch = format!("kelpie/{issue}");
    rig.forge.open_pull_request(pr, &branch, &[issue]);
    rig.claude.script([
        Scripted::Slow(10, Box::new(Scripted::Push("work.txt", "work\n"))),
        Scripted::Text("CLEAN"),
    ]);
    step(runner).unwrap(); // the worker's turn
    step(runner).unwrap(); // round 1
    step(runner).unwrap(); // round 2
    let head = rig.forge.head_of(&branch).unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    rig.clock.advance(ci);
    assert!(step(runner).unwrap().is_none(), "the checks have to settle");
    rig.clock.advance(CHECKS_SETTLE);
    let Some(StepReport::Ruling { id, .. }) = step(runner).unwrap() else {
        panic!("no merge ruling");
    };
    rig.clock.advance(ruling);
    rig.ask(runner, "rule", Some(&format!("{id} yes")));
    step(runner).unwrap(); // marks the draft ready
    rig.forge.set_state(pr, PullRequestState::Merged);
    assert!(matches!(
        step(runner).unwrap(),
        Some(StepReport::Finished { .. })
    ));
}

#[test]
fn the_totals_match_the_items() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    merge_one(&rig, &runner, 7, 71, 5, 100);
    merge_one(&rig, &runner, 8, 81, 15, 300);
    merge_one(&rig, &runner, 9, 91, 25, 0);

    let all = rig.ask(&runner, "timings", None);
    assert_eq!(all["items"], 3);
    let issues: Vec<u64> = all["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["issue"].as_u64().unwrap())
        .collect();
    assert_eq!(issues, [9, 8, 7], "newest first");
    for bucket in Bucket::ALL {
        let name = serde_json::to_value(bucket).unwrap();
        let name = name.as_str().unwrap();
        let items: u64 = all["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["split"][name].as_u64().unwrap())
            .sum();
        assert_eq!(all["total"]["split"][name], items, "{name}");
    }
    let walls: u64 = all["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["wall"].as_u64().unwrap())
        .sum();
    assert_eq!(all["total"]["wall"], walls);
    assert_eq!(all["total"]["split"]["ruling"], 400);
    assert_eq!(all["total"]["split"]["ci"], 3 * CHECKS_SETTLE + 45);

    let last_two = rig.ask(&runner, "timings", Some("2"));
    assert_eq!(last_two["items"], 2);
    assert_eq!(last_two["total"]["split"]["ruling"], 300);
    assert!(all["table"].as_str().unwrap().contains("#7"));
    assert!(!last_two["table"].as_str().unwrap().contains("#7"));
}

#[test]
fn the_history_survives_a_restart_and_a_dropped_item_is_in_it() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    merge_one(&rig, &runner, 7, 71, 5, 0);
    rig.ask(&runner, "add", Some("8"));
    rig.clock.advance(12);
    rig.ask(&runner, "drop", Some("8"));
    drop(runner);

    let runner = rig.open().unwrap();
    let timings = rig.ask(&runner, "timings", None);
    assert_eq!(timings["items"], 2);
    assert_eq!(timings["rows"][0]["issue"], 8);
    assert_eq!(timings["rows"][0]["merged"], false);
    assert_eq!(timings["rows"][0]["split"]["idle"], 12);
    assert_eq!(timings["rows"][1]["merged"], true);
}

#[test]
fn timings_with_nothing_finished_is_an_empty_table_and_bad_counts_are_refused() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let empty = rig.ask(&runner, "timings", None);
    assert_eq!(
        (&empty["items"], &empty["total"]["wall"]),
        (&json!(0), &json!(0))
    );
    for bad in ["0", "-1", "many", "1 2"] {
        let reply = rig.ask(&runner, "timings", Some(bad));
        assert!(
            reply["error"]
                .as_str()
                .unwrap()
                .starts_with("`timings` takes a count"),
            "{bad}: {reply}"
        );
    }
}
