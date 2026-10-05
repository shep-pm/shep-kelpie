//! When the runner charges time, and to what, as the project pauses, stops and restarts

use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::{PATIENCE, assert_sums, held_while, read_while_held, secs, timings};
use crate::ports::{AgentError, Checks, SessionId};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::{Hold, Rig, Scripted};

#[test]
fn a_work_item_in_a_paused_project_spends_its_time_paused() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.clock.advance(30);
    let t = timings(&rig, &runner);
    assert_eq!((&t["phase"], &t["wall"]), (&json!("paused"), &json!(30)));
    assert_eq!(secs(&t, "paused"), 30);
    assert_sums(&t);
}

#[test]
fn a_running_project_between_steps_is_other() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.clock.advance(20);
    let t = timings(&rig, &runner);
    assert_eq!(t["phase"], "other");
    assert_eq!((secs(&t, "other"), secs(&t, "paused")), (20, 0));
    assert_sums(&t);
}

#[test]
fn every_phase_is_listed_with_zeros() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let t = timings(&rig, &runner);
    let names: Vec<_> = t["seconds"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(names.len(), 12, "{names:?}");
}

#[test]
fn a_turn_in_flight_is_the_workers_time_even_before_it_ends() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| step(&runner));
        assert!(hold.entered(PATIENCE), "the turn never began");
        rig.clock.advance(100);
        let t = timings(&rig, &runner);
        assert_eq!(
            (t["phase"].as_str(), secs(&t, "worker")),
            (Some("worker"), 100)
        );
        assert_sums(&t);
        hold.release();
        turn.join().unwrap().unwrap();
    });
    let t = timings(&rig, &runner);
    assert_eq!(
        (secs(&t, "worker"), t["phase"].as_str()),
        (100, Some("other"))
    );
    assert_sums(&t);
}

// The call dies unborn while the maintainer pauses the project, so no turn
// starts over, and the turn marked running has nothing running it.
#[test]
fn a_session_that_dies_unborn_in_a_paused_project_is_paused_not_the_workers() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    let unborn = AgentError::NoSession(Harness::ClaudeCode, SessionId("0e2c".into()));
    rig.claude
        .script([Scripted::HoldThenFail(hold.clone(), unborn)]);
    let t = held_while(&rig, &runner, &hold, 40, || {
        rig.ask(&runner, "pause", None);
    });
    assert_eq!(
        secs(&t, "worker"),
        40,
        "the call is the worker's while held"
    );
    rig.clock.advance(100);
    let t = timings(&rig, &runner);
    assert_eq!(t["phase"], "paused");
    assert_eq!((secs(&t, "worker"), secs(&t, "paused")), (40, 100));
    assert_sums(&t);
}

// A turn the stopping runner cut short stays marked running in the state
// file. The maintainer paused the project 50 seconds after it began. Nothing
// resumes the turn until `start`.
fn cut_short_while_paused() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude
        .script([Scripted::HoldThenFail(hold.clone(), AgentError::Stopped)]);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| step(&runner));
        assert!(hold.entered(PATIENCE), "the turn never began");
        rig.clock.advance(50);
        rig.ask(&runner, "pause", None);
        hold.release();
        assert_eq!(turn.join().unwrap().unwrap(), None);
    });
    drop(runner);
    rig.clock.advance(600);
    let runner = rig.open().unwrap();
    let state = std::fs::read_to_string(rig.paths().state).unwrap();
    assert!(state.contains(r#""state": "running""#), "{state}");
    (rig, runner)
}

#[test]
fn a_turn_cut_short_in_a_paused_project_is_paused_not_the_workers() {
    let (rig, runner) = cut_short_while_paused();
    let t = timings(&rig, &runner);
    assert_eq!((secs(&t, "worker"), secs(&t, "other")), (50, 600));
    rig.clock.advance(100);
    let t = timings(&rig, &runner);
    assert_eq!(t["phase"], "paused");
    assert_eq!((secs(&t, "worker"), secs(&t, "paused")), (50, 100));
    assert_sums(&t);
    crate::runner::settle(&runner).unwrap();
    rig.clock.advance(30);
    let t = timings(&rig, &runner);
    assert_eq!((secs(&t, "worker"), secs(&t, "paused")), (50, 130));
    assert_sums(&t);
}

#[test]
fn a_cut_short_turn_that_starts_again_is_the_workers_again() {
    let (rig, runner) = cut_short_while_paused();
    rig.clock.advance(100);
    rig.ask(&runner, "start", None);
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    let t = read_while_held(&rig, &runner, &hold, 40);
    assert_eq!(t["phase"], "worker");
    assert_eq!((secs(&t, "worker"), secs(&t, "paused")), (90, 100));
    assert_sums(&t);
}

#[test]
fn ci_and_a_ruling_each_keep_their_own_seconds() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.clock.advance(600);
    let t = timings(&rig, &runner);
    assert_eq!((t["phase"].as_str(), secs(&t, "ci")), (Some("ci"), 600));

    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    let waiting_on_ci = secs(&timings(&rig, &runner), "ci");
    assert!(waiting_on_ci >= 600);

    rig.clock.advance(900);
    let t = timings(&rig, &runner);
    assert_eq!(
        (t["phase"].as_str(), secs(&t, "ruling")),
        (Some("ruling"), 900)
    );
    assert_eq!(
        secs(&t, "ci"),
        waiting_on_ci,
        "the ruling's time is not CI's"
    );
    assert_sums(&t);
}

#[test]
fn a_ruling_parked_while_the_project_is_paused_is_paused_time() {
    let (rig, runner, head) = Rig::with_pull_request("koji");
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    rig.ask(&runner, "pause", None);
    let ruling = secs(&timings(&rig, &runner), "ruling");

    rig.clock.advance(900);
    let t = timings(&rig, &runner);
    assert_eq!(t["phase"], "paused");
    assert_eq!((secs(&t, "paused"), secs(&t, "ruling")), (900, ruling));
    assert_sums(&t);

    rig.ask(&runner, "start", None);
    rig.clock.advance(60);
    let t = timings(&rig, &runner);
    assert_eq!(t["phase"], "ruling");
    assert_eq!((secs(&t, "paused"), secs(&t, "ruling")), (900, ruling + 60));
    assert_sums(&t);
}

#[test]
fn the_time_kelpie_is_down_is_other() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    drop(runner);
    rig.clock.advance(600);
    let runner = rig.open().unwrap();
    let t = timings(&rig, &runner);
    assert_eq!(
        (secs(&t, "other"), secs(&t, "paused"), &t["wall"]),
        (600, 0, &json!(600))
    );
    assert_sums(&t);
}

#[test]
fn a_work_item_saved_without_timings_counts_from_the_load() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    drop(runner);
    let file = rig.paths().state;
    let mut state: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    state["work_items"][0]
        .as_object_mut()
        .unwrap()
        .remove("timings");
    std::fs::write(&file, serde_json::to_string(&state).unwrap()).unwrap();
    rig.clock.advance(5_000);

    let runner = rig.open().unwrap();
    let t = timings(&rig, &runner);
    assert_eq!(t["wall"], 0, "the work item counts from this first load");
    rig.clock.advance(10);
    let t = timings(&rig, &runner);
    assert_eq!((&t["wall"], secs(&t, "paused")), (&json!(10), 10));
}

#[test]
fn a_status_read_writes_nothing() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let before = std::fs::read(rig.paths().state).unwrap();
    rig.clock.advance(400);
    let t = timings(&rig, &runner);
    assert_eq!(t["wall"], 400);
    assert_eq!(std::fs::read(rig.paths().state).unwrap(), before);
}

fn saved_since(rig: &Rig) -> u64 {
    let text = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: Value = serde_json::from_str(&text).unwrap();
    state["work_items"][0]["timings"]["since"].as_u64().unwrap()
}

// A paused project's work item has no turn to run. A step finds nothing to
// do, so its only save is the loop's beat.
#[test]
fn an_idle_step_saves_the_time_only_every_half_minute() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let begun = saved_since(&rig);
    rig.clock.advance(10);
    crate::runner::settle(&runner).unwrap();
    assert_eq!(saved_since(&rig), begun + 10, "settle always saves");
    rig.clock.advance(10);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        saved_since(&rig),
        begun + 10,
        "under half a minute is left alone"
    );
    rig.clock.advance(25);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(saved_since(&rig), begun + 45, "the step's beat saved it");
}

#[test]
fn a_beat_that_cannot_save_lets_the_step_go_on_and_loses_no_time() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let folder = rig.paths().state.parent().unwrap().to_path_buf();
    let mode = |mode| std::fs::set_permissions(&folder, PermissionsExt::from_mode(mode)).unwrap();
    rig.clock.advance(60);
    mode(0o555);
    let stepped = step(&runner);
    mode(0o755);
    assert_eq!(stepped.unwrap(), None);
    rig.clock.advance(5);
    let t = timings(&rig, &runner);
    assert_eq!((&t["wall"], secs(&t, "paused")), (&json!(65), 65));
    assert_sums(&t);
}
