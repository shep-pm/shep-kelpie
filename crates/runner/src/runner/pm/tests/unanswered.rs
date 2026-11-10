//! A stuck item's ruling the reply named nothing for, which is asked about
//! once more, and left alone on the reply after that

use std::sync::Mutex;

use serde_json::json;

use super::asked::{advanced_until, pending};
use super::{
    NOTHING, answered, asked_again, left_alone_again, name_pm, past_the_window, pm_on, pm_seen,
    step_until,
};
use crate::ports::{AgentError, Checks, Role};
use crate::runner::pm::Wake;
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted, ticket};

/// What a failed turn says on the ruling it parks the item on
const TURN_FAILED: &str = "its worker's turn parked: the call failed once, the model host \
    stayed unreachable for 30 minutes, or the turn stopped short twice";

/// The wake for `issue`, parked on ruling `id` after a failed turn
fn parked_wake(issue: u64, id: u64) -> String {
    format!("#{issue} is stuck: {TURN_FAILED}, and ruling {id} asks the maintainer")
}

/// The wake for #7, parked on ruling 71 with its CI red past the fix cap
fn red_wake() -> String {
    "#7 is stuck: CI failed and the worker's fix turns reached the cap `ci.fix_attempts` \
     sets, and ruling 71 asks the maintainer"
        .to_owned()
}

/// A turn whose call failed, as the model host crashing does
fn crashed(why: &str) -> AgentError {
    AgentError::Failed(Harness::ClaudeCode, why.to_owned())
}

/// A project with a project manager and #7 open, parked on ruling 7 for a turn
/// whose call failed, with its wake not yet gone out
fn parked(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = pm_on(Rig::new(project));
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Fail(crashed("it crashed"))]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Failed { .. })
    ));
    (rig, runner)
}

/// A project with a project manager, #7 parked on `StillRed` ruling 71, and #8
/// open on a branch that clashes with #7's in `work.txt`, so one wake carries
/// the stuck item and their conflict
fn parked_and_conflicting(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = Rig::with_pull_request_worked(
        project,
        |rig| {
            rig.fix_attempts(0);
            name_pm(rig);
            rig.edit_settings(|s| {
                assert!(s.contains("active_items = 1"), "the example's cap moved");
                s.replace("active_items = 1", "active_items = 3")
            });
        },
        Scripted::Push("work.txt", "seven\n"),
    );
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. }) if id == ticket(71)
    ));
    rig.forge.open_pull_request(81, "kelpie/8", &[8]);
    rig.ask(&runner, "add", Some("8"));
    rig.claude.script([Scripted::Push("work.txt", "eight\n")]);
    // Its turn pushes the same file #7's branch changed, and the clash is
    // queued beside the stuck reason, before either waits its window.
    advanced_until(&runner, || clash_queued(&runner));
    (rig, runner)
}

/// Whether a clash between two open branches is queued for the next wake
fn clash_queued(runner: &Mutex<Runner>) -> bool {
    let held = runner.lock().unwrap();
    (held.pm.due.iter()).any(|wake| matches!(wake, Wake::Conflict(..)))
}

/// Moves the clock past the wake's window, steps until it ends, and returns
/// what the wake was for, what kelpie did, and what it dropped
fn woke(rig: &Rig, runner: &Mutex<Runner>) -> (Vec<String>, Vec<String>, Vec<String>) {
    past_the_window(rig);
    let report = step_until(runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    answered(Some(report))
}

/// `pm-notes.md`, which kelpie writes for each wake it answered
fn notes(rig: &Rig) -> String {
    std::fs::read_to_string(rig.paths().pm.join("pm-notes.md")).unwrap_or_default()
}

/// Steps until a project manager's call is in flight
fn wake_in_flight(rig: &Rig, runner: &Mutex<Runner>) {
    advanced_until(runner, || !pm_seen(rig).is_empty());
}

#[test]
fn a_reply_with_no_unstick_for_a_carried_stuck_ruling_asks_again_naming_the_ruling() {
    let (rig, runner) = parked_and_conflicting("unstick-asked");
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    let (woke_for, acted, dropped) = woke(&rig, &runner);

    assert!(woke_for.contains(&red_wake()), "{woke_for:?}");
    assert!(
        woke_for
            .iter()
            .any(|w| w.starts_with("the branches of #7 and #8 conflict in")),
        "{woke_for:?}"
    );
    assert_eq!(acted, [asked_again(7, 71)]);
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(pm_seen(&rig).len(), 1);
}

#[test]
fn the_second_reply_with_no_unstick_leaves_the_item_alone_and_asks_no_more() {
    let (rig, runner) = parked("unstick-twice");
    rig.claude.script_for(
        Role::Pm,
        [
            Scripted::Say(NOTHING),
            Scripted::Say(NOTHING),
            Scripted::Say(NOTHING),
        ],
    );

    let (woke_for, acted, _) = woke(&rig, &runner);
    assert_eq!(acted, [asked_again(7, 7)], "{woke_for:?}");

    let (woke_for, acted, _) = woke(&rig, &runner);
    assert_eq!(acted, [left_alone_again(7, 7)], "{woke_for:?}");

    // Left alone on the ruling it is parked on, as a `none` leaves it, so the
    // same parked item wakes nothing more.
    for _ in 0..4 {
        past_the_window(&rig);
        assert_eq!(step(&runner).unwrap(), None, "the wake ran a third time");
    }
    assert_eq!(pm_seen(&rig).len(), 2);
    assert!(pending(&rig), "the ruling it is parked on went");
    assert!(
        notes(&rig).contains(&left_alone_again(7, 7)),
        "{}",
        notes(&rig)
    );
}

#[test]
fn a_reply_that_names_the_item_in_unstick_fix_or_answer_is_not_asked_again() {
    let cases = [
        (
            "unstick-named-unstick",
            "{\"pick\": null, \"unstick\": {\"item\": 7, \"action\": \"retry\", \"why\": \
             \"a turn can run again\"}}",
            "retried #7",
        ),
        (
            "unstick-named-fix",
            "{\"pick\": null, \"fix\": {\"item\": 7, \"on\": 9, \"why\": \"the machine is at \
             fault\"}}",
            "parked #7 on #9",
        ),
        (
            "unstick-named-answer",
            "{\"pick\": null, \"answer\": {\"item\": 7, \"text\": \"run it again\", \"why\": \
             \"the outage passed\"}}",
            "answer #7: not a work item parked on a question or follow-up ruling",
        ),
    ];
    for (project, answer, did) in cases {
        let (rig, runner) = parked(project);
        rig.forge
            .open_issue(9, "A fix for the machine", "It cannot start a turn.");
        rig.claude.script([Scripted::Say("done")]);
        rig.claude.script_for(Role::Pm, [Scripted::Say(answer)]);

        let (_, acted, dropped) = woke(&rig, &runner);

        let said = |lines: &[String]| (lines.iter()).any(|l| l.contains(did));
        assert!(
            said(&acted) || said(&dropped),
            "{project}: acted {acted:?} dropped {dropped:?}"
        );
        assert!(
            !acted.iter().any(|l| l.contains("asked again")),
            "{project}: {acted:?}"
        );
        assert_eq!(pm_seen(&rig).len(), 1, "{project}");
    }
}

#[test]
fn an_unstick_of_none_settles_the_ruling_and_asks_nothing_more() {
    let (rig, runner) = parked("unstick-none");
    let answer = "{\"pick\": null, \"unstick\": {\"item\": 7, \"action\": \"none\", \
                  \"why\": \"its worker is thinking\"}}";
    rig.claude.script_for(Role::Pm, [Scripted::Say(answer)]);

    let (_, acted, dropped) = woke(&rig, &runner);

    assert_eq!(acted, ["left #7 to resolve itself".to_owned()]);
    assert_eq!(dropped, Vec::<String>::new());
    for _ in 0..4 {
        past_the_window(&rig);
        assert_eq!(step(&runner).unwrap(), None, "a none woke it again");
    }
    assert_eq!(pm_seen(&rig).len(), 1);
}

#[test]
fn a_ruling_answered_meanwhile_is_not_asked_again() {
    let (rig, runner) = parked("unstick-answered");
    let hold = Hold::default();
    rig.claude.script([Scripted::Say("done")]);
    rig.claude
        .script_for(Role::Pm, [Scripted::HoldThenSay(hold.clone(), NOTHING)]);
    past_the_window(&rig);
    wake_in_flight(&rig, &runner);
    assert!(hold.entered(CALL_PATIENCE), "the wake never began");

    // The maintainer answers ruling 7 while its wake is out.
    rig.ask(&runner, "rule", Some("7 yes"));
    hold.release();

    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));

    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    assert_eq!(pm_seen(&rig).len(), 1);
}

#[test]
fn the_notes_line_of_a_wake_that_asked_again_names_the_ruling_and_not_nothing() {
    let (rig, runner) = parked("unstick-notes");
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    woke(&rig, &runner);

    let line = notes(&rig);
    assert!(line.contains(&asked_again(7, 7)), "{line}");
    assert!(line.contains("woke for:"), "{line}");
    assert!(!line.contains("kelpie: nothing"), "{line}");
}

#[test]
fn a_reply_missing_the_ruling_still_applies_its_other_actions() {
    let (rig, runner) = parked("unstick-other-actions");
    rig.claude.script_for(
        Role::Pm,
        [Scripted::Say(
            "{\"pick\": null, \"standing\": \"Never start #12 while #7 is parked.\", \"reply\": \
         \"#12 waits for #7.\"}",
        )],
    );

    let (_, acted, dropped) = woke(&rig, &runner);

    assert!(acted.contains(&asked_again(7, 7)), "{acted:?}");
    assert!(
        (acted.iter()).any(|l| l.starts_with("set the standing notes")),
        "{acted:?}"
    );
    assert_eq!(dropped, Vec::<String>::new());
    let standing = std::fs::read_to_string(rig.paths().pm.join("pm-standing.md")).unwrap();
    assert!(
        standing.contains("Never start #12 while #7 is parked."),
        "{standing}"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["pm"]["reply"],
        "#12 waits for #7."
    );
}

#[test]
fn the_ask_names_the_ruling_in_the_next_wake_and_says_none_leaves_it_alone() {
    let (rig, runner) = parked("unstick-prompt");
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(NOTHING), Scripted::Say(NOTHING)]);

    woke(&rig, &runner);
    past_the_window(&rig);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    answered(Some(report));

    let [first, second] = pm_seen(&rig).try_into().unwrap();
    let (first, second) = (&first.call.prompt, &second.call.prompt);
    assert!(first.contains(&parked_wake(7, 7)), "{first}");
    assert!(second.contains(&parked_wake(7, 7)), "{second}");
    assert!(
        second.contains("#7 is parked on ruling 7, and your reply held no `unstick` for it."),
        "{second}"
    );
    assert!(second.contains("Answer `none` in `unstick`"), "{second}");
    assert!(second.starts_with("Kelpie woke you again"), "{second}");
}
