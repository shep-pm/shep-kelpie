//! A green run's log: the case lists the run and leaves the log, and the project
//! manager asks for it with `run_log`, which the next wake carries

use std::sync::Mutex;

use super::super::rulings::{answered, asking, left_to_pm, settled};
use super::super::{answered as reported, pm_seen, question, step_until};
use super::rig::{BODY, case, head_run, red, wake};
use crate::ports::{LogPart, Role};
use crate::runner::flight::advance;
use crate::runner::{Pass, Runner, StepReport};
use crate::test::{Rig, Scripted, Seen};

/// Run 102 is green on the head, with a whole log of `GREEN-LOG-LINE`
fn with_green_run(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = red(project, "A title", BODY);
    rig.forge.set_head_runs(
        &head,
        vec![
            head_run("lint", 103, Some("failure")),
            head_run("test", 102, Some("success")),
        ],
    );
    rig.forge
        .set_run_log(103, LogPart::Failed, "LINT-FAILED-LINE");
    rig.forge.set_run_log(102, LogPart::Whole, "GREEN-LOG-LINE");
    (rig, runner)
}

/// Tells the project manager something and waits for the wake it answers with `reply`
fn wake_saying(rig: &Rig, runner: &Mutex<Runner>, reply: &'static str) -> Vec<String> {
    rig.ask(runner, "tell", Some("how is #7?"));
    rig.claude
        .script_for(crate::ports::Role::Pm, [Scripted::Say(reply)]);
    let (_, _, dropped) = reported(Some(step_until(runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    dropped
}

/// The wake that follows, which nothing but the ask itself brings, answering `none`
fn next_wake(rig: &Rig, runner: &Mutex<Runner>) -> Seen {
    rig.claude
        .script_for(crate::ports::Role::Pm, [Scripted::Say(super::rig::NOTHING)]);
    reported(Some(step_until(runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    pm_seen(rig).pop().unwrap()
}

const ASK: &str = "{\"pick\": null, \"run_log\": {\"item\": 7, \"run\": 102}}";

#[test]
fn a_green_runs_log_is_not_in_the_case_until_asked() {
    let (rig, runner) = with_green_run("case-green-unasked");

    wake(&rig, &runner);

    let case = case(&rig, 7).unwrap();
    assert!(case.contains("- `test`: success, run 102\n"), "{case}");
    assert!(!case.contains("GREEN-LOG-LINE"), "{case}");
    assert_eq!(rig.forge.run_log_reads(102), 0);
}

#[test]
fn the_next_wake_carries_the_log_asked_for() {
    let (rig, runner) = with_green_run("case-green-asked");

    let dropped = wake_saying(&rig, &runner, ASK);
    let seen = next_wake(&rig, &runner);

    assert_eq!(dropped, Vec::<String>::new());
    let case = case(&rig, 7).unwrap();
    assert!(case.contains("> GREEN-LOG-LINE"), "{case}");
    assert!(seen.call.prompt.contains("run 102"), "{}", seen.call.prompt);
    assert!(
        seen.call.prompt.contains("cases/7.md"),
        "{}",
        seen.call.prompt
    );
    assert_eq!(rig.forge.run_log_reads(102), 1);
}

#[test]
fn a_log_read_stays_in_later_cases() {
    let (rig, runner) = with_green_run("case-green-kept");
    wake_saying(&rig, &runner, ASK);
    next_wake(&rig, &runner);

    wake(&rig, &runner);

    let case = case(&rig, 7).unwrap();
    assert!(case.contains("> GREEN-LOG-LINE"), "{case}");
    assert_eq!(rig.forge.run_log_reads(102), 1, "read once, kept after");
}

#[test]
fn a_run_off_the_list_is_dropped_and_named() {
    let (rig, runner) = with_green_run("case-green-off-list");
    rig.forge.set_run_log(999, LogPart::Whole, "OFF-LIST-LINE");

    wake(&rig, &runner);
    let dropped = wake_saying(
        &rig,
        &runner,
        "{\"pick\": null, \"run_log\": {\"item\": 7, \"run\": 999}}",
    );
    wake(&rig, &runner);

    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(dropped[0].contains("run 999"), "{dropped:?}");
    assert!(
        dropped[0].contains("not on its `## CI` list"),
        "{dropped:?}"
    );
    assert_eq!(rig.forge.run_log_reads(999), 0);
    assert!(!case(&rig, 7).unwrap().contains("OFF-LIST-LINE"));
}

#[test]
fn a_run_the_case_holds_is_dropped_and_makes_no_read() {
    let (rig, runner) = with_green_run("case-green-held");
    wake(&rig, &runner);
    assert_eq!(rig.forge.run_log_reads(103), 1);

    let dropped = wake_saying(
        &rig,
        &runner,
        "{\"pick\": null, \"run_log\": {\"item\": 7, \"run\": 103}}",
    );
    wake(&rig, &runner);

    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(dropped[0].contains("already holds the log"), "{dropped:?}");
    assert_eq!(
        rig.forge.run_log_reads(103),
        1,
        "the held log is not read again"
    );
}

/// Run 102 is green on the head of #7's pull request 71, whose ruling the project
/// manager answers, and its whole log is `GREEN-LOG-LINE`
fn ruling_with_green_run(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = left_to_pm(project);
    let head = rig.forge.head_of("kelpie/7").expect("a head");
    rig.forge
        .set_head_runs(&head, vec![head_run("test", 102, Some("success"))]);
    rig.forge.set_run_log(102, LogPart::Whole, "GREEN-LOG-LINE");
    (rig, runner)
}

/// How many wakes put ruling 71 to the project manager to answer
fn put_to_it(rig: &Rig) -> usize {
    (pm_seen(rig).iter())
        .filter(|s| s.call.prompt.contains("waits on your answer"))
        .count()
}

#[test]
fn a_ruling_unanswered_while_a_log_is_asked_for_stays_held_and_is_asked_again() {
    let (rig, runner) = ruling_with_green_run("case-ruling-asks-log");
    rig.claude.script_for(
        Role::Pm,
        [
            Scripted::Say(ASK),
            Scripted::Say(super::super::rulings::answers('A', false)),
        ],
    );

    settled(&rig, &runner);

    assert_eq!(put_to_it(&rig), 2, "asked again after the log");
    assert!(
        pm_seen(&rig)[1].call.prompt.contains("run 102"),
        "{}",
        pm_seen(&rig)[1].call.prompt
    );
    assert_eq!(asking(&rig), None, "never held out to the maintainer");
    assert!(
        answered(&rig).is_some(),
        "it answered the second time: {:?}",
        rig.github.comments()
    );
    assert_eq!(rig.forge.run_log_reads(102), 1);
}

#[test]
fn a_question_unanswered_while_a_log_is_asked_for_stays_held_and_is_asked_again() {
    let (rig, runner) = question::asking_on_a_pull_request("case-question-asks-log");
    let head = rig.push_by_hand("kelpie/7", "work.txt");
    rig.forge
        .set_head_runs(&head, vec![head_run("test", 102, Some("success"))]);
    rig.forge.set_run_log(102, LogPart::Whole, "GREEN-LOG-LINE");
    rig.claude.script_for(
        Role::Pm,
        [
            Scripted::Say(ASK),
            Scripted::Say(
                "{\"answer\": {\"item\": 7, \"text\": \"Use --check.\", \"why\": \"the docs\"}}",
            ),
        ],
    );

    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));

    let calls = pm_seen(&rig);
    assert_eq!(calls.len(), 2, "the question was put to it again");
    assert!(calls[1].call.prompt.contains("worker asks"), "second wake");
    assert_eq!(rig.forge.comments(), [], "the question never went out");
    assert!(
        rig.alerts.posts().is_empty(),
        "the maintainer was never asked"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"], serde_json::json!([]), "answered");
}

#[test]
fn a_wake_with_no_new_log_to_ask_for_lets_the_ruling_go_to_the_maintainer() {
    let (rig, runner) = ruling_with_green_run("case-ruling-no-new-log");
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(ASK), Scripted::Say(ASK)]);

    settled(&rig, &runner);

    assert_eq!(put_to_it(&rig), 2, "asked again once, for the one log");
    assert!(asking(&rig).is_some(), "the maintainer is asked at last");
    assert_eq!(answered(&rig), None);
    assert_eq!(rig.forge.run_log_reads(102), 1);
}

#[test]
fn an_ask_for_an_item_the_same_answer_settles_is_dropped_and_makes_no_read() {
    let (rig, runner) = question::asking_on_a_pull_request("case-question-settled-with-ask");
    let head = rig.push_by_hand("kelpie/7", "work.txt");
    rig.forge
        .set_head_runs(&head, vec![head_run("test", 102, Some("success"))]);
    rig.forge.set_run_log(102, LogPart::Whole, "GREEN-LOG-LINE");
    rig.claude.script_for(
        Role::Pm,
        [Scripted::Say(
            "{\"answer\": {\"item\": 7, \"text\": \"Use --check.\", \"why\": \"the docs\"}, \
             \"run_log\": {\"item\": 7, \"run\": 102}}",
        )],
    );

    let (_, _, dropped) = reported(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));

    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(dropped[0].contains("run 102"), "{dropped:?}");
    assert!(dropped[0].contains("no longer parked"), "{dropped:?}");
    assert_eq!(rig.forge.run_log_reads(102), 0);
}

#[test]
fn a_log_that_did_not_reach_the_case_can_be_asked_for_again() {
    let (rig, runner) = with_green_run("case-green-no-list");
    wake_saying(&rig, &runner, ASK);
    rig.forge.set_head_runs_error(Some("GitHub is down"));
    next_wake(&rig, &runner);
    assert!(!case(&rig, 7).unwrap().contains("GREEN-LOG-LINE"));
    rig.forge.set_head_runs_error(None);

    let dropped = wake_saying(&rig, &runner, ASK);
    next_wake(&rig, &runner);

    assert_eq!(dropped, Vec::<String>::new());
    assert!(case(&rig, 7).unwrap().contains("> GREEN-LOG-LINE"));
}

#[test]
fn the_log_is_read_with_the_lock_let_go() {
    let (rig, runner) = with_green_run("case-green-off-lock");
    wake_saying(&rig, &runner, ASK);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(super::rig::NOTHING)]);
    let read = rig.forge.hold_next_run_log_read();

    let (meanwhile, pass) = rig.during_read(
        read,
        || advance(&runner),
        || rig.ask(&runner, "status", None),
    );

    let status = meanwhile.expect("the trigger waited for the lock the read held");
    assert!(status.is_object(), "{status}");
    assert!(matches!(pass, Ok(Pass::Started)), "{pass:?}");
    assert!(case(&rig, 7).unwrap().contains("> GREEN-LOG-LINE"));
}

#[test]
fn a_log_that_could_not_be_read_is_not_asked_for_again() {
    let (rig, runner) = with_green_run("case-green-unread");
    rig.forge.set_run_log_error(102, "GitHub is down");
    wake_saying(&rig, &runner, ASK);
    next_wake(&rig, &runner);

    let case = case(&rig, 7).unwrap();
    assert!(case.contains("could not be read just now"), "{case}");
    assert!(case.contains("It is not asked for again."), "{case}");
    let dropped = wake_saying(&rig, &runner, ASK);

    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert!(dropped[0].contains("could not be"), "{dropped:?}");
    assert_eq!(rig.forge.run_log_reads(102), 1);
}
