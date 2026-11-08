//! A green run's log: the case lists the run and leaves the log, and the project
//! manager asks for it with `run_log`, which the next wake carries

use std::sync::Mutex;

use super::super::{answered, pm_seen, step_until};
use super::rig::{BODY, case, head_run, red, wake};
use crate::ports::LogPart;
use crate::runner::{Runner, StepReport};
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
    let (_, _, dropped) = answered(Some(step_until(runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    dropped
}

/// The wake that follows, which nothing but the ask itself brings, answering `none`
fn next_wake(rig: &Rig, runner: &Mutex<Runner>) -> Seen {
    rig.claude
        .script_for(crate::ports::Role::Pm, [Scripted::Say(super::rig::NOTHING)]);
    answered(Some(step_until(runner, |r| {
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
