//! Draining through the runner's stand-ins: what starts, what goes on, and
//! what `status` shows

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};

use crate::guard::Pause;
use crate::ports::{CLEAN, Cost, Role, Usage};
use crate::runner::drain::PAUSE_HOLD;
use crate::runner::gate::CHECKS_SETTLE;
use crate::runner::turn::paused::PAUSED;
use crate::runner::turn::tests::step_paused_in_a_tool;
use crate::runner::{CallRole, Pass, Runner, advance, answer, step};
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};
use crate::work_item::Turn;

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
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");
    assert_eq!(
        rig.ask(&runner, "drain", None)["draining"]["calls"],
        json!([{ "issue": "7", "role": "worker" }])
    );
    assert!(!hold.returned(), "draining ended the call");

    hold.release();
    let deadline = Instant::now() + CALL_PATIENCE;
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
fn a_pause_sets_no_flag_for_a_work_item_with_no_turn_in_flight() {
    let (rig, runner) = seven_due("shep");
    // Issue 7's first turn is due, not running: nothing for a tool result to end.
    std::fs::create_dir_all(rig.build_7()).unwrap();
    let drained = rig.ask(&runner, "drain", Some("pause"));
    assert_eq!(drained["draining"]["at_tool"], json!(null), "{drained}");
    assert!(
        !Pause::in_build(&rig.build_7()).is_requested(),
        "a flag with no turn to read it would end the next one"
    );
}

fn pause_notes(runner: &Mutex<Runner>) -> Vec<String> {
    let mut notes = runner.lock().unwrap().take_notes();
    notes.retain(|note| note.starts_with("drain pause:"));
    notes
}

#[test]
fn ten_identical_pauses_log_one_line() {
    let (rig, runner) = seven_due("shep");
    pause_notes(&runner);
    for _ in 0..10 {
        rig.ask(&runner, "drain", Some("pause"));
    }
    assert_eq!(
        pause_notes(&runner),
        ["drain pause: no worker turn in flight can end at a tool call"]
    );
}

#[test]
fn a_pause_that_names_other_turns_logs_a_new_line() {
    let (rig, runner) = seven_due("shep");
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    std::fs::create_dir_all(rig.build_7()).unwrap();
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");
    pause_notes(&runner);
    rig.ask(&runner, "drain", Some("pause"));
    rig.ask(&runner, "drain", Some("pause"));
    assert_eq!(
        pause_notes(&runner),
        ["drain pause: turns asked to end at their next tool call: #7"]
    );

    hold.release();
    let deadline = Instant::now() + CALL_PATIENCE;
    while draining(&rig, &runner)["calls"] != json!([]) {
        assert!(
            Instant::now() < deadline,
            "the turn's end was never recorded"
        );
        step(&runner).unwrap();
        std::thread::yield_now();
    }
    pause_notes(&runner);
    rig.ask(&runner, "drain", Some("pause"));
    assert_eq!(
        pause_notes(&runner),
        ["drain pause: no worker turn in flight can end at a tool call"]
    );
}

#[test]
fn a_pause_after_an_undrain_logs_again() {
    let (rig, runner) = seven_due("shep");
    rig.ask(&runner, "drain", Some("pause"));
    rig.ask(&runner, "undrain", None);
    pause_notes(&runner);
    rig.ask(&runner, "drain", Some("pause"));
    assert_eq!(
        pause_notes(&runner),
        ["drain pause: no worker turn in flight can end at a tool call"]
    );
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
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
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
    rig.ask(&runner, "rule", Some("71 yes"));
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
fn drain_takes_no_params() {
    let rig = Rig::new("xilriws");
    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "drain", Some("now")),
        json!({ "error": "`drain` takes no params" })
    );
    assert_eq!(draining(&rig, &runner), json!(null));
}

#[test]
fn a_drained_runner_does_not_wake_the_project_manager() {
    let rig = Rig::new("eevee");
    rig.edit_settings(|s| {
        let listed = "[app.dogs.kelpie.agents]\n";
        assert!(s.contains(listed), "the example's agents table moved");
        s.replace(listed, "[app.dogs.kelpie.agents]\npm = \"pm\"\n")
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

// A `drain pause` answered while a pass holds the runner's lock sets its flag
// at once, and leaves the pause for the pass to set at its next lock.
#[test]
fn a_drain_answered_while_a_pass_holds_the_lock_leaves_its_ask_for_the_pass() {
    let (rig, runner) = seven_due("koji");
    std::fs::create_dir_all(rig.build_7()).unwrap();
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");

    let runner = Arc::new(runner);
    let postings = runner.lock().unwrap().postings();
    let mut pass = runner.lock().unwrap();
    let asking = {
        let (runner, postings) = (Arc::clone(&runner), Arc::clone(&postings));
        std::thread::spawn(move || answer(&runner, &postings, "drain", Some("pause")))
    };
    let reply: Value = serde_json::from_str(&asking.join().unwrap()).expect("a JSON reply");
    assert_eq!(
        reply["draining"]["calls"],
        json!([{ "issue": "7", "role": "worker" }]),
        "{reply}"
    );
    assert!(
        !Pause::in_build(&rig.build_7()).is_requested(),
        "the pause was set while the pass still held the lock"
    );

    pass.take_posted();
    assert!(
        Pause::in_build(&rig.build_7()).is_requested(),
        "the drain pause never reached the turn in flight"
    );
    hold.release();
}

// An `undrain` answered with the lock free acts at once, so it takes up a
// `drain` that landed while a pass held the lock. Left for the next pass, that
// drain pauses the turns this one let go of, and the drain wins last.
#[test]
fn an_undrain_takes_up_a_drain_that_landed_while_a_pass_held_the_lock() {
    let (rig, runner) = seven_due("koji");
    std::fs::create_dir_all(rig.build_7()).unwrap();
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");

    let runner = Arc::new(runner);
    let postings = runner.lock().unwrap().postings();
    let held = runner.lock().unwrap();
    let asking = {
        let (runner, postings) = (Arc::clone(&runner), Arc::clone(&postings));
        std::thread::spawn(move || answer(&runner, &postings, "drain", Some("pause")))
    };
    asking.join().unwrap();
    drop(held);
    assert!(
        !Pause::in_build(&rig.build_7()).is_requested(),
        "the posted drain set the pause while a pass held the lock"
    );

    // The lock is free, so this `undrain` is answered by the runner.
    rig.ask(&runner, "undrain", None);
    assert!(!postings.draining(), "the undrain left the runner draining");

    let mut pass = runner.lock().unwrap();
    pass.take_posted();
    assert!(
        !Pause::in_build(&rig.build_7()).is_requested(),
        "the next pass paused the turn the undrain had let go of"
    );
    assert!(!postings.draining(), "the next pass drained again");
    drop(pass);
    hold.release();
}

// A `drain` answered with the lock free acts at once, so it takes up an
// `undrain` that landed while a pass held the lock. Left for the next pass,
// that undrain withdraws the pause this one set, and the undrain wins last.
#[test]
fn a_drain_takes_up_an_undrain_that_landed_while_a_pass_held_the_lock() {
    let (rig, runner) = seven_due("koji");
    std::fs::create_dir_all(rig.build_7()).unwrap();
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");

    rig.ask(&runner, "drain", Some("pause"));
    assert!(
        Pause::in_build(&rig.build_7()).is_requested(),
        "the pause never reached the turn in flight"
    );

    let runner = Arc::new(runner);
    let postings = runner.lock().unwrap().postings();
    let held = runner.lock().unwrap();
    let asking = {
        let (runner, postings) = (Arc::clone(&runner), Arc::clone(&postings));
        std::thread::spawn(move || answer(&runner, &postings, "undrain", None))
    };
    asking.join().unwrap();
    drop(held);
    assert!(
        !postings.draining(),
        "the posted undrain did not let go at once"
    );
    assert!(
        Pause::in_build(&rig.build_7()).is_requested(),
        "the posted undrain withdrew the pause itself"
    );

    // The lock is free, so this `drain` is answered by the runner.
    rig.ask(&runner, "drain", None);
    assert!(postings.draining(), "the drain did not hold new calls back");

    let mut pass = runner.lock().unwrap();
    pass.take_posted();
    assert!(
        postings.draining(),
        "the next pass undrained what this drained"
    );
    assert!(
        Pause::in_build(&rig.build_7()).is_requested(),
        "the next pass withdrew the pause the drain set"
    );
    drop(pass);
    hold.release();
}

// Issue 7's turn ended at a tool call for a `drain pause`, and issue 8 is due
fn seven_paused_eight_due(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    step_paused_in_a_tool(&rig, &runner, Scripted::Say("done"));
    rig.ask(&runner, "add", Some("8"));
    (rig, runner)
}

// The prompts of the calls the worker made, oldest first
fn prompts(rig: &Rig) -> Vec<String> {
    (rig.claude.calls().into_iter())
        .map(|call| call.prompt)
        .collect()
}

#[test]
fn undrain_keep_holds_a_paused_turn_for_the_hold_and_starts_the_others() {
    let (rig, runner) = seven_paused_eight_due("shep");
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);

    rig.ask(&runner, "undrain", Some("keep"));
    step(&runner).unwrap();
    assert_eq!(rig.claude.calls().len(), 2, "issue 8 started at once");
    step(&runner).unwrap();
    rig.clock.advance(PAUSE_HOLD - 1);
    step(&runner).unwrap();
    assert!(
        !prompts(&rig).contains(&PAUSED.to_owned()),
        "held too short"
    );

    rig.clock.advance(1);
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    assert!(prompts(&rig).contains(&PAUSED.to_owned()), "never resumed");
}

#[test]
fn a_bare_undrain_starts_the_paused_turn_at_once() {
    let (rig, runner) = seven_paused_eight_due("shep");
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    rig.ask(&runner, "undrain", None);
    for _ in 0..2 {
        step(&runner).unwrap();
    }
    assert!(
        prompts(&rig).contains(&PAUSED.to_owned()),
        "{:?}",
        prompts(&rig)
    );
}

#[test]
fn a_second_pause_inside_the_hold_finds_the_turns_stopped() {
    let (rig, runner) = seven_paused_eight_due("shep");
    rig.ask(&runner, "undrain", Some("keep"));
    rig.clock.advance(PAUSE_HOLD / 2);
    // Only issue 8 could be running, and its turn has not begun.
    let drained = rig.ask(&runner, "drain", Some("pause"));
    assert_eq!(drained["draining"]["calls"], json!([]), "{drained}");
    assert_eq!(rig.claude.calls().len(), 1, "nothing started");
}

// An `undrain keep` answered while a pass holds the lock sets the hold when
// the pass takes it up.
#[test]
fn an_undrain_keep_that_landed_while_a_pass_held_the_lock_still_holds() {
    let (rig, runner) = seven_paused_eight_due("shep");
    let runner = Arc::new(runner);
    let postings = runner.lock().unwrap().postings();
    let mut pass = runner.lock().unwrap();
    {
        let (runner, postings) = (Arc::clone(&runner), Arc::clone(&postings));
        let asking =
            std::thread::spawn(move || answer(&runner, &postings, "undrain", Some("keep")));
        asking.join().unwrap();
    }
    pass.take_posted();
    drop(pass);
    // Issue 8 is due behind issue 7 in the rotation, so step until it ran.
    let runner = Arc::into_inner(runner).expect("the only holder");
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    for _ in 0..3 {
        step(&runner).unwrap();
    }
    assert!(
        !prompts(&rig).contains(&PAUSED.to_owned()),
        "{:?}",
        prompts(&rig)
    );
    assert!(rig.claude.calls().len() > 1, "issue 8 never started");
}

// The pass that holds the lock while `undrain keep` lands reaches the turn
// before it takes the ask up.
#[test]
fn an_undrain_keep_not_yet_taken_up_already_holds_the_paused_turn() {
    let (rig, runner) = seven_paused_eight_due("shep");
    let runner = Arc::new(runner);
    let postings = runner.lock().unwrap().postings();
    let pass = runner.lock().unwrap();
    let asking = {
        let (runner, postings) = (Arc::clone(&runner), Arc::clone(&postings));
        std::thread::spawn(move || answer(&runner, &postings, "undrain", Some("keep")))
    };
    asking.join().unwrap();
    let (seven, eight) = (&pass.state.work_items[0], &pass.state.work_items[1]);
    assert!(pass.pause_held(seven), "the pass would start issue 7");
    assert!(!pass.pause_held(eight), "issue 8's turn was not paused");
    drop((pass, rig));
}

#[test]
fn undrain_keep_holds_only_the_turns_that_were_paused_when_it_came() {
    let (rig, runner) = seven_paused_eight_due("shep");
    rig.ask(&runner, "undrain", Some("keep"));
    let mut pass = runner.lock().unwrap();
    // A turn that ends for a pause later on, say the `[builds]` limit's, is no
    // turn a pause gave up on.
    pass.state.work_items[1].turn = Turn::Next {
        prompt: PAUSED.to_owned(),
    };
    assert!(pass.pause_held(&pass.state.work_items[0]));
    assert!(!pass.pause_held(&pass.state.work_items[1]));
}

#[test]
fn undrain_takes_only_the_keep_param() {
    let rig = Rig::new("xilriws");
    let runner = rig.open().unwrap();
    assert_eq!(
        rig.ask(&runner, "undrain", Some("now")),
        json!({ "error": "`undrain` takes `keep` or nothing, not \"now\"" })
    );
    assert_eq!(draining(&rig, &runner), json!(null));
}

#[test]
fn every_harness_whose_attached_session_holds_a_handover_has_a_role() {
    let held: Vec<_> = crate::settings::Harness::ALL
        .into_iter()
        .filter(|h| h.facts().holds_handover_at_attach.is_some())
        .collect();
    let roles: Vec<_> = crate::settings::Harness::ALL
        .into_iter()
        .filter(|h| h.facts().attached_role.is_some())
        .collect();

    assert!(held.iter().all(|h| roles.contains(h)), "{held:?} {roles:?}");
    assert_eq!(roles.len(), held.len());
}

#[test]
fn status_names_each_role_as_it_always_has() {
    let roles = [
        (CallRole::Worker, "worker"),
        (CallRole::Reviewer, "reviewer"),
        (CallRole::Pm, "pm"),
        (CallRole::Security, "security"),
        (CallRole::Triager, "triager"),
        (CallRole::Retro, "retro"),
        (CallRole::E2e, "e2e"),
        (CallRole::Attached("pi_session"), "pi_session"),
    ];
    for (role, name) in roles {
        assert_eq!(serde_json::to_value(role).unwrap(), name);
    }
}

#[test]
fn attached_sessions_are_listed_pi_then_goose_then_codex() {
    let roles: Vec<_> = crate::settings::Harness::ALL
        .into_iter()
        .filter_map(|h| h.facts().attached_role)
        .collect();

    assert_eq!(roles, ["pi_session", "goose_session", "codex_session"]);
}
