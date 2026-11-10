//! A stuck item the project manager already answered `none` for, which is
//! woken for again only when its state changes or its back-off ends

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::json;

use super::{answered, asked_again, past_the_window, pm_on, pm_seen, step_until};
use crate::ports::{AgentError, CallActivity, Clock};
use crate::runner::flight::advance;
use crate::runner::pm::{DOWN_FOR, HOLD_FOR};
use crate::runner::{Pass, Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted, ticket};

/// The project manager leaving #7 to resolve itself
const NONE_7: &str = "{\"pick\": null, \"unstick\": {\"item\": 7, \"action\": \"none\", \
    \"why\": \"its worker is thinking\"}}";

/// The project manager ending #7's idle call and resuming its turn
const RETRY_7: &str = "{\"pick\": null, \"unstick\": {\"item\": 7, \"action\": \"retry\", \
    \"why\": \"its worker is hung\"}}";

/// The project manager starting nothing and unsticking nothing
const NOTHING: &str = "{\"pick\": null}";

/// What a failed turn says on the ruling it parks the item on
const TURN_FAILED: &str = "its worker's turn parked: the call failed once, the model host \
    stayed unreachable for 30 minutes, or the turn stopped short twice";

/// The wake for `issue`, parked on ruling `id` after a failed turn
fn parked_wake(issue: u64, id: u64) -> String {
    format!("#{issue} is stuck: {TURN_FAILED}, and ruling {id} asks the maintainer")
}

/// A turn whose call failed, as the model host crashing does
fn crashed(why: &str) -> AgentError {
    AgentError::Failed(Harness::ClaudeCode, why.to_owned())
}

/// A project with a project manager and #7 open on a worker's turn held in
/// flight, which has shown nothing since its start
///
/// Its turn ceiling is long enough for the call to outlast the back-off.
fn idle_turn(project: &str) -> (Rig, Mutex<Runner>, Hold) {
    let rig = Rig::new(project);
    rig.edit_settings(|s| {
        assert!(
            s.contains("turn_timeout = 60"),
            "the example's turn ceiling moved"
        );
        s.replace("turn_timeout = 60", "turn_timeout = 240")
    });
    let (rig, runner) = pm_on(rig);
    rig.ask(&runner, "add", Some("7"));
    let work = Hold::default();
    rig.claude.script([Scripted::Hold(work.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(work.entered(CALL_PATIENCE), "the turn never began");
    rig.claude.set_activity(CallActivity::Nothing);
    (rig, runner, work)
}

/// A project with a project manager and #7 open, parked on ruling 7 for a turn
/// whose call failed
fn turn_failed(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = pm_project(project);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Fail(crashed("it crashed"))]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Failed { .. })
    ));
    (rig, runner)
}

/// A project that names a project manager, on a turn ceiling long enough to
/// outlast the back-off
fn pm_project(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.edit_settings(|s| {
        assert!(
            s.contains("turn_timeout = 60"),
            "the example's turn ceiling moved"
        );
        s.replace("turn_timeout = 60", "turn_timeout = 240")
    });
    pm_on(rig)
}

/// The worker's call showing output now, which the board reads
fn shows_output(rig: &Rig, runner: &Mutex<Runner>) {
    rig.claude.set_activity(CallActivity::At(rig.clock.now()));
    assert_eq!(step(runner).unwrap(), None, "output is no wake");
}

/// Steps until `work`'s call is in flight, which no step may wait on
fn until_in_flight(runner: &Mutex<Runner>, work: &Hold) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while Instant::now() < deadline {
        if work.entered(Duration::from_secs(1)) {
            return;
        }
        advance(runner).unwrap();
    }
    panic!("no pass started the call the test holds");
}

#[test]
fn an_idle_call_answered_none_does_not_wake_it_while_it_goes_quiet_again() {
    let (rig, runner, work) = idle_turn("munchla");
    rig.claude.script([
        Scripted::Say(NONE_7),
        Scripted::Say(NONE_7),
        Scripted::Say(NONE_7),
    ]);

    // 11m from its start: idle, and the project manager leaves it alone.
    rig.clock.advance(11 * 60);
    // The pass that notices it queues it, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let (woke_for, acted, _) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 11m"]
    );
    assert_eq!(acted, ["left #7 to resolve itself"]);
    assert_eq!(pm_seen(&rig).len(), 1);

    // Its worker shows output, and goes quiet 10 minutes later: the same idle
    // call is no second wake.
    shows_output(&rig, &runner);
    rig.clock.advance(10 * 60);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the same idle call woke it again"
    );
    assert_eq!(pm_seen(&rig).len(), 1);
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        (notes.iter())
            .any(|n| n.starts_with("#7 is idle on the turn the project manager left alone")),
        "{notes:?}"
    );

    // Output, quiet again, and past 30 minutes from the none it wakes once more.
    shows_output(&rig, &runner);
    rig.clock.advance(21 * 60);
    // The pass that notices it queues it, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let (woke_for, acted, _) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 21m"]
    );
    assert_eq!(acted, ["left #7 to resolve itself"]);
    assert_eq!(pm_seen(&rig).len(), 2);

    // A second none holds it for 60 minutes more: quiet at 101m is no wake,
    // and the next one is no earlier than 102m.
    shows_output(&rig, &runner);
    rig.clock.advance(59 * 60);
    assert_eq!(step(&runner).unwrap(), None, "a second none woke it early");
    assert_eq!(pm_seen(&rig).len(), 2);
    shows_output(&rig, &runner);
    rig.clock.advance(11 * 60);
    // The pass that notices it queues it, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let (woke_for, ..) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 11m"]
    );
    assert_eq!(pm_seen(&rig).len(), 3);

    work.release();
    assert!(work.answered(CALL_PATIENCE), "the turn never answered");
}

#[test]
fn an_idle_call_held_out_wakes_it_when_the_back_off_ends() {
    let (rig, runner, work) = idle_turn("nibblical");
    rig.claude.script([
        Scripted::Say(NONE_7),
        Scripted::Say(NONE_7),
        Scripted::Say(NONE_7),
    ]);

    // 11m from its start: idle, and the project manager leaves it alone.
    rig.clock.advance(11 * 60);
    // The pass that notices it queues it, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let (woke_for, acted, _) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 11m"]
    );
    assert_eq!(acted, ["left #7 to resolve itself"]);
    assert_eq!(pm_seen(&rig).len(), 1);

    // Its worker shows output once, goes quiet, and is hung from there. The
    // flip at 21m is dropped inside the back-off, and no output comes to flip
    // it again.
    shows_output(&rig, &runner);
    rig.clock.advance(10 * 60);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "the same idle call woke it again"
    );
    rig.clock.advance(19 * 60);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "it woke before its back-off ended"
    );
    assert_eq!(pm_seen(&rig).len(), 1);

    // 31m after the none, past the 30 minutes it was held for, the hung worker
    // is put to it once even though the board showed nothing more.
    rig.clock.advance(2 * 60);
    // The pass that notices it queues it, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let (woke_for, acted, _) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 10 minutes or more"]
    );
    assert_eq!(acted, ["left #7 to resolve itself"]);
    assert_eq!(pm_seen(&rig).len(), 2);

    // The second none holds it for 60 minutes more, and it is not woken for on
    // every pass while it is held.
    rig.clock.advance(59 * 60);
    assert_eq!(step(&runner).unwrap(), None, "a second none woke it early");
    assert_eq!(pm_seen(&rig).len(), 2);
    rig.clock.advance(2 * 60);
    // The pass that notices it queues it, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let (woke_for, ..) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 10 minutes or more"]
    );
    assert_eq!(pm_seen(&rig).len(), 3);

    work.release();
    assert!(work.answered(CALL_PATIENCE), "the turn never answered");
}

#[test]
fn a_new_turn_after_a_none_is_woken_for_at_once() {
    let (rig, runner) = turn_failed("oddish");
    let work = Hold::default();
    rig.claude.script([
        Scripted::Say(NONE_7),
        Scripted::Hold(work.clone()),
        Scripted::Say(NOTHING),
    ]);

    // Left alone on the ruling its failed turn parked it on, past the
    // 3-minute window that wake waits (#987).
    rig.clock.advance(HOLD_FOR);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (woke_for, acted, _) = answered(Some(report));
    assert_eq!(woke_for, [parked_wake(7, 7)]);
    assert_eq!(acted, ["left #7 to resolve itself"]);

    // The maintainer answers the ruling, its turn runs again, and goes quiet.
    rig.ask(&runner, "rule", Some("7 yes"));
    until_in_flight(&runner, &work);
    rig.claude.set_activity(CallActivity::Nothing);
    rig.clock.advance(11 * 60);
    // The pass that notices it queues it, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let (woke_for, ..) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 11m"]
    );
    assert_eq!(pm_seen(&rig).len(), 2);

    work.release();
    assert!(work.answered(CALL_PATIENCE), "the new turn never answered");
}

#[test]
fn a_retry_clears_the_item_so_its_next_idle_wakes_at_once() {
    let (rig, runner, _) = idle_turn("masquerain");
    let again = Hold::default();
    rig.claude.script([
        Scripted::Say(NONE_7),
        Scripted::Say(RETRY_7),
        Scripted::Hold(again.clone()),
        Scripted::Say(NOTHING),
    ]);

    // 11m from its start: idle, and left alone.
    rig.clock.advance(11 * 60);
    // The pass that notices it queues it, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let (_, acted, _) = answered(step(&runner).unwrap());

    assert_eq!(acted, ["left #7 to resolve itself"]);

    // A later wake ends its call instead, which is an action on the item.
    rig.ask(&runner, "tell", Some("how is #7?"));
    let (woke_for, acted, _) = answered(step(&runner).unwrap());
    assert_eq!(woke_for, ["the maintainer told you something"]);
    assert_eq!(acted, ["ended #7's idle call, to retry"]);

    // Its ended call parks it on the ceiling's ruling, which kelpie settles
    // with the retry, and its turn runs again.
    step_until(&runner, |r| matches!(r, StepReport::TimedOut { .. }));
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    until_in_flight(&runner, &again);
    rig.claude.set_activity(CallActivity::Nothing);
    rig.clock.advance(11 * 60);
    // The pass that notices it queues it, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let (woke_for, ..) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 11m"]
    );
    assert_eq!(pm_seen(&rig).len(), 3);

    again.release();
    assert!(again.answered(CALL_PATIENCE), "the new turn never answered");
}

#[test]
fn an_item_parked_on_a_stuck_ruling_and_left_alone_is_not_sent_again_when_it_is_back() {
    let (rig, runner) = turn_failed("quillava");
    rig.claude.script([Scripted::Say(NONE_7)]);
    // Its wake waits the 3-minute window (#987).
    rig.clock.advance(HOLD_FOR);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, _) = answered(Some(report));
    assert_eq!(acted, ["left #7 to resolve itself"]);

    // #9 parks on a ruling of its own, the wake for it fails, and what the
    // project manager missed while it was down is given up.
    rig.ask(&runner, "add", Some("9"));
    rig.claude.script([
        Scripted::Fail(crashed("it crashed")),
        Scripted::Fail(crashed("overloaded")),
    ]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Failed { .. })
    ));
    // Its wake waits the 3-minute window (#987).
    rig.clock.advance(HOLD_FOR);
    let failed = step_until(&runner, |r| matches!(r, StepReport::PmFailed { .. }));
    assert_eq!(
        failed,
        StepReport::PmFailed {
            reason: "claude failed: overloaded".into()
        }
    );

    // Once it is back it is asked again about what it missed: #9, and not #7.
    rig.clock.advance(DOWN_FOR + 60);
    // What it missed is queued on this pass, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    rig.claude.script([Scripted::Say(NOTHING)]);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (woke_for, _, _) = answered(Some(report));
    assert_eq!(woke_for, [parked_wake(9, 9)]);
    assert_eq!(pm_seen(&rig).len(), 3);
    let notes = runner.lock().unwrap().take_notes();
    let note = "#7 is parked on ruling 7, which the project manager left alone, \
               so it is not woken for it again";
    assert!(notes.contains(&note.to_owned()), "{notes:?}");
}

#[test]
fn a_new_stuck_ruling_on_the_same_item_wakes_it() {
    let (rig, runner) = turn_failed("poochy");
    rig.claude.script([Scripted::Say(NONE_7)]);
    // Its wake waits the 3-minute window (#987).
    rig.clock.advance(HOLD_FOR);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, _) = answered(Some(report));
    assert_eq!(acted, ["left #7 to resolve itself"]);

    // The maintainer answers the ruling, the turn runs again and fails again.
    // The ruling it raises now is another state of the same item, under the
    // same number.
    rig.claude.script([
        Scripted::Fail(crashed("it crashed again")),
        Scripted::Say(NOTHING),
    ]);
    rig.clock.advance(60);
    rig.ask(&runner, "rule", Some("7 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Failed { .. })
    ));
    // Its wake waits the 3-minute window (#987).
    rig.clock.advance(HOLD_FOR);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (woke_for, _, _) = answered(Some(report));
    assert_eq!(woke_for, [parked_wake(7, 7)]);
    assert_eq!(pm_seen(&rig).len(), 2);
}

#[test]
fn a_none_for_one_item_leaves_another_items_wake_alone() {
    let (rig, runner) = turn_failed("delcatty");
    rig.claude.script([Scripted::Say(NONE_7)]);
    // Its wake waits the 3-minute window (#987).
    rig.clock.advance(HOLD_FOR);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, _) = answered(Some(report));
    assert_eq!(acted, ["left #7 to resolve itself"]);

    // #9 fails its turn while #7 is left alone, and its wake goes out.
    rig.ask(&runner, "add", Some("9"));
    rig.claude.script([
        Scripted::Fail(crashed("it crashed")),
        Scripted::Say(NOTHING),
    ]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Failed { .. })
    ));
    // Its wake waits the 3-minute window (#987).
    rig.clock.advance(HOLD_FOR);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (woke_for, acted, _) = answered(Some(report));
    assert_eq!(woke_for, [parked_wake(9, 9)]);
    // #7 stays out of the wake, left alone. #9's ruling went unanswered, so it
    // is asked about once more.
    assert_eq!(acted, [asked_again(9, 9)]);
    assert_eq!(pm_seen(&rig).len(), 2);
}

#[test]
fn a_wake_that_failed_leaves_its_item_to_be_woken_for_again() {
    let (rig, runner, work) = idle_turn("wugtrala");
    rig.claude
        .script([Scripted::Fail(crashed("overloaded")), Scripted::Say(NONE_7)]);

    // 11m from its start: idle, and the wake for it fails, so it records nothing.
    rig.clock.advance(11 * 60);
    // The pass that notices it queues it, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let failed = step_until(&runner, |r| matches!(r, StepReport::PmFailed { .. }));
    assert_eq!(
        failed,
        StepReport::PmFailed {
            reason: "claude failed: overloaded".into()
        }
    );

    // Once it is back, the same idle call wakes it again.
    rig.clock.advance(DOWN_FOR + 60);
    // The same idle call is queued again on this pass, and its wake waits the
    // window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (woke_for, acted, _) = answered(Some(report));
    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 10 minutes or more"]
    );
    assert_eq!(acted, ["left #7 to resolve itself"]);
    assert_eq!(pm_seen(&rig).len(), 2);

    work.release();
    assert!(work.answered(CALL_PATIENCE), "the turn never answered");
}

#[test]
fn a_read_wait_left_behind_does_not_hide_an_idle_call() {
    let (rig, runner, work) = idle_turn("unstick-read");
    // The gate once held #7 for a reviewer's read, then sent it back through a
    // pass. Its wait record stays to keep the wake count, but the item is not
    // waiting at the gate now.
    runner.lock().unwrap().pm_read_wait(ticket(7), 0);
    rig.claude.script([Scripted::Say(RETRY_7)]);

    // 11m from its start: idle, and the wake names the idle call, not the read.
    rig.clock.advance(11 * 60);
    // The pass that notices it queues it, and its wake waits the window.
    assert_eq!(step(&runner).unwrap(), None, "the wake waits its window");
    past_the_window(&rig);
    let (woke_for, acted, _) = answered(step(&runner).unwrap());

    assert_eq!(
        woke_for,
        ["#7 is stuck: its worker has shown no tool call or output for 11m"]
    );
    assert_eq!(acted, ["ended #7's idle call, to retry"]);
    assert!(work.answered(CALL_PATIENCE), "the held call never ended");
}
