//! Draining through the runner's stand-ins: what starts, what goes on, and
//! what `status` shows

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::ports::{Cost, Role, Usage};
use crate::runner::gate::CHECKS_SETTLE;
use crate::runner::{Pass, Runner, advance, step};
use crate::test::{Hold, Rig, Scripted};

// Real threads on real time, so every wait has this ceiling.
const PATIENCE: Duration = Duration::from_secs(30);

// A running project with issue 7 open and its first turn due
fn seven_due(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    (rig, runner)
}

fn draining(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    rig.ask(runner, "status", None)["draining"].clone()
}

#[test]
fn a_drained_runner_starts_no_turn_until_it_is_undrained() {
    let (rig, runner) = seven_due("koji");
    assert_eq!(draining(&rig, &runner), json!(null), "not draining");
    assert_eq!(
        rig.ask(&runner, "drain", None)["draining"],
        json!({ "calls": [], "ceiling": 3600 })
    );
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.claude.calls(), [], "the due turn waits");
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["turn"]["state"],
        "due"
    );

    assert_eq!(rig.ask(&runner, "undrain", None)["draining"], json!(null));
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    assert_eq!(rig.claude.calls().len(), 1, "the turn ran once undrained");
}

#[test]
fn drain_answers_the_calls_still_running_until_they_end() {
    let (rig, runner) = seven_due("golbat");
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(PATIENCE), "the turn never began");
    assert_eq!(
        rig.ask(&runner, "drain", None)["draining"]["calls"],
        json!([{ "issue": 7, "role": "worker" }])
    );
    assert!(!hold.returned(), "draining ended the call");

    hold.release();
    let deadline = Instant::now() + PATIENCE;
    while draining(&rig, &runner)["calls"] != json!([]) {
        assert!(
            Instant::now() < deadline,
            "the turn's end was never recorded"
        );
        step(&runner).unwrap();
        std::thread::yield_now();
    }
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.claude.calls().len(), 1, "nothing started after it");
}

#[test]
fn a_restart_ends_draining() {
    let (rig, runner) = seven_due("rotom");
    rig.ask(&runner, "drain", None);
    drop(runner);
    let runner = rig.open().unwrap();
    assert_eq!(draining(&rig, &runner), json!(null));
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    assert_eq!(rig.claude.calls().len(), 1);
}

#[test]
fn a_drained_runner_starts_no_review_call() {
    let (rig, runner) = seven_due("shep");
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    rig.ask(&runner, "drain", None);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(rig.reviewer.seen().is_empty(), "the local round waits");

    rig.ask(&runner, "undrain", None);
    step(&runner).unwrap(); // round 1, qwen
    assert_eq!(rig.reviewer.seen().len(), 1);
    rig.ask(&runner, "drain", None);
    assert_eq!(step(&runner).unwrap(), None);
    let reviews = |rig: &Rig| {
        let calls = rig.claude.all_calls().into_iter();
        calls.filter(|c| c.role == Role::Reviewer).count()
    };
    assert_eq!(reviews(&rig), 0, "the reviewer's session waits");
    rig.ask(&runner, "undrain", None);
    step(&runner).unwrap(); // round 2, claude
    assert_eq!(reviews(&rig), 1);
}

#[test]
fn a_drained_runner_still_merges() {
    let (rig, runner, head) = Rig::parked("reactmap");
    rig.ask(&runner, "drain", None);
    rig.ask(&runner, "rule", Some("1 yes"));
    for _ in 0..3 {
        if !rig.forge.merges().is_empty() {
            break;
        }
        step(&runner).unwrap();
        rig.clock.advance(CHECKS_SETTLE);
    }
    assert_eq!(rig.forge.merges(), [(71, head)]);
}

#[test]
fn drain_and_undrain_take_no_params() {
    let rig = Rig::new("xilriws");
    let runner = rig.open().unwrap();
    for action in ["drain", "undrain"] {
        assert_eq!(
            rig.ask(&runner, action, Some("now")),
            json!({ "error": format!("`{action}` takes no params") })
        );
    }
    assert_eq!(draining(&rig, &runner), json!(null));
}

#[test]
fn a_drained_runner_does_not_wake_the_project_manager() {
    let rig = Rig::new("eevee");
    rig.edit_settings(|s| {
        let listed = "\nimplementers = [\"sonnet-high\"]\n";
        assert!(s.contains(listed), "the example's implementers moved");
        s.replace(listed, "\nimplementers = [\"sonnet-high\"]\npm = \"pm\"\n")
    });
    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "drain", None)["draining"]["ceiling"],
        3600,
        "the turn ceiling, longer than the project manager's"
    );
    rig.ask(&runner, "tell", Some("Hold #12"));
    let woken = |rig: &Rig| {
        let calls = rig.claude.all_calls().into_iter();
        calls.filter(|c| c.role == Role::Pm).count()
    };
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(woken(&rig), 0, "the wake waits");

    rig.ask(&runner, "undrain", None);
    rig.claude.script([Scripted::Say("{\"pick\": null}")]);
    step(&runner).unwrap();
    assert_eq!(woken(&rig), 1);
}
