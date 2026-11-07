//! An item at the gate asks the project manager what becomes of each open
//! thread: the ask a step makes, what its answer does, and what an answer
//! that rules on nothing costs

use std::sync::Mutex;

use super::{answered, name_pm, pm_seen, step_until};
use crate::ports::{AgentError, Clock, Timestamp};
use crate::runner::pm::DOWN_FOR;
use crate::runner::pm::Wake;
use crate::runner::pm::wakes::quiet_backoff;
use crate::runner::review_bot::RuledThread;
use crate::runner::{Runner, StepReport};
use crate::settings::Harness;
use crate::test::ticket;
use crate::test::{Rig, Scripted};

const NOTHING: &str = "{\"pick\": null}";

/// A project with a project manager and #7 at the gate, its pull request 71
/// open at the returned head
fn at_the_gate(project: &str) -> (Rig, Mutex<Runner>, String) {
    Rig::with_pull_request_set(project, name_pm)
}

fn thread(n: u32) -> RuledThread {
    RuledThread {
        id: format!("PRRT_{n}"),
        path: "src/lib.rs".to_owned(),
        line: Some(n),
        author: "coderabbitai".to_owned(),
        severity: None,
        what: format!("Finding {n}."),
        maintainer_wrote: false,
    }
}

/// The wake for #7's pull request with `threads` open
fn wake(head: &str, threads: Vec<RuledThread>) -> Wake {
    Wake::Threads {
        ticket: ticket(7),
        pull_request: 71,
        head: head.to_owned(),
        threads,
    }
}

fn ask(runner: &Mutex<Runner>, head: &str, threads: u32) -> bool {
    let threads = (1..=threads).map(thread).collect();
    runner.lock().unwrap().pm_ask_threads(wake(head, threads))
}

/// Scripts `say` as the project manager's answer, and returns what kelpie
/// acted on and dropped
fn answers(rig: &Rig, runner: &Mutex<Runner>, say: &'static str) -> (Vec<String>, Vec<String>) {
    rig.claude.script([Scripted::Say(say)]);
    let report = step_until(runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));
    (acted, dropped)
}

fn out(runner: &Mutex<Runner>) -> bool {
    runner.lock().unwrap().pm_threads_out(ticket(7))
}

#[test]
fn a_failed_wake_answers_nothing_and_clears_the_ask() {
    let (rig, runner, head) = at_the_gate("threads-failed");
    assert!(ask(&runner, &head, 2));
    assert!(out(&runner), "the ask is noted");
    rig.claude.script([Scripted::Fail(AgentError::Failed(
        Harness::ClaudeCode,
        "it crashed".to_owned(),
    ))]);

    let report = step_until(&runner, |r| matches!(r, StepReport::PmFailed { .. }));

    assert!(matches!(report, StepReport::PmFailed { .. }), "{report:?}");
    assert!(!out(&runner), "the failed wake answered for nothing");
    assert!(runner.lock().unwrap().pm.due.is_empty());
    // Once it is back, the next step asks again, and no back-off holds it.
    rig.clock.advance(DOWN_FOR);
    assert!(ask(&runner, &head, 2));
    assert!(out(&runner));
}

#[test]
fn no_ask_is_made_while_the_project_manager_is_down() {
    let (rig, runner, head) = at_the_gate("threads-down");
    {
        let mut held = runner.lock().unwrap();
        let until = Timestamp(rig.clock.now().0 + DOWN_FOR);
        held.pm.down_until = Some(until);
    }

    assert!(!ask(&runner, &head, 1));

    assert!(!out(&runner));
    assert!(
        runner.lock().unwrap().pm.due.is_empty(),
        "no wake is queued"
    );
    assert!(pm_seen(&rig).is_empty(), "no call was made");
}

#[test]
fn each_why_reaches_the_journal() {
    let (rig, runner, head) = at_the_gate("threads-journal");
    assert!(ask(&runner, &head, 3));

    let (acted, dropped) = answers(
        &rig,
        &runner,
        "{\"threads\": [\
         {\"item\": 7, \"thread\": 1, \"decision\": \"fix\", \"why\": \"the guard is missing\"}, \
         {\"item\": 7, \"thread\": 2, \"decision\": \"close\", \"why\": \"the caller checks it\"}, \
         {\"item\": 7, \"thread\": 3, \"decision\": \"follow-up\", \"why\": \"too big for here\"}]}",
    );

    assert_eq!(dropped, Vec::<String>::new());
    assert_eq!(
        acted,
        [
            "ruled thread 1 of #7 as fix: the guard is missing",
            "ruled thread 2 of #7 as close: the caller checks it",
            "ruled thread 3 of #7 as follow-up: too big for here",
        ]
    );
    let notes = std::fs::read_to_string(rig.paths().pm.join("pm-notes.md")).unwrap();
    for why in [
        "the guard is missing",
        "the caller checks it",
        "too big for here",
    ] {
        assert!(notes.contains(why), "{why:?} missing from\n{notes}");
    }
    assert!(!out(&runner), "an answer ends the ask");
}

#[test]
fn a_thread_the_maintainer_wrote_in_is_pushed_back_and_not_closed() {
    let (rig, runner, head) = at_the_gate("threads-maintainer");
    let (mut first, mut second) = (thread(1), thread(2));
    (first.maintainer_wrote, second.maintainer_wrote) = (true, true);
    let wake = wake(&head, vec![first, second, thread(3)]);
    assert!(runner.lock().unwrap().pm_ask_threads(wake));

    let (acted, dropped) = answers(
        &rig,
        &runner,
        "{\"threads\": [\
         {\"item\": 7, \"thread\": 1, \"decision\": \"close\", \"why\": \"it is wrong\"}, \
         {\"item\": 7, \"thread\": 2, \"decision\": \"push-back\", \"why\": \"the race is real\"}, \
         {\"item\": 7, \"thread\": 3, \"decision\": \"close\", \"why\": \"the caller checks it\"}, \
         {\"item\": 7, \"thread\": 4, \"decision\": \"fix\"}, \
         {\"item\": 9, \"thread\": 1, \"decision\": \"fix\"}]}",
    );

    assert_eq!(
        acted,
        [
            "ruled thread 2 of #7 as push-back: the race is real",
            "ruled thread 3 of #7 as close: the caller checks it",
        ]
    );
    assert_eq!(dropped.len(), 3, "{dropped:?}");
    assert!(
        dropped[0].starts_with("close #7 thread 1: the maintainer wrote in it"),
        "{dropped:?}"
    );
    assert!(
        dropped[1].contains("the wake lists no such thread"),
        "{dropped:?}"
    );
    assert!(
        dropped[2].contains("no ask of it waits on you"),
        "{dropped:?}"
    );
}

#[test]
fn an_item_with_an_ask_out_has_a_case_with_no_ruling() {
    let (rig, runner, head) = at_the_gate("threads-case");
    assert!(ask(&runner, &head, 1));

    answers(&rig, &runner, NOTHING);

    let pm = rig.paths().pm;
    let case = std::fs::read_to_string(pm.join("cases/7.md")).expect("the item has a case");
    assert!(case.contains("## Issue #7"), "{case}");
    assert!(case.contains("## Pull request #71"), "{case}");
    assert!(!case.contains("## Ruling "), "{case}");
    let board = std::fs::read_to_string(pm.join("board.md")).unwrap();
    assert!(board.contains(", case `cases/7.md`\n"), "{board}");
    let [seen] = pm_seen(&rig).try_into().unwrap();
    assert!(
        seen.call.prompt.contains("`cases/7.md`"),
        "{}",
        seen.call.prompt
    );
    assert!(
        seen.call
            .prompt
            .contains("1. src/lib.rs:1 (coderabbitai): Finding 1.")
    );
}

#[test]
fn a_wake_that_answers_nothing_backs_off_before_it_asks_again() {
    let (rig, runner, head) = at_the_gate("threads-backoff");
    let waits = [quiet_backoff(1), quiet_backoff(2), quiet_backoff(3)];
    for (nones, wait) in waits.into_iter().enumerate() {
        assert!(ask(&runner, &head, 1), "the ask {nones} is made");
        answers(&rig, &runner, NOTHING);
        assert!(!out(&runner));

        assert!(!ask(&runner, &head, 1), "held after {} nones", nones + 1);
        rig.clock.advance(wait - 1);
        assert!(!ask(&runner, &head, 1), "held until {wait}s pass");
        rig.clock.advance(1);
    }
    // An answered thread starts the count again.
    assert!(ask(&runner, &head, 2));
    answers(
        &rig,
        &runner,
        "{\"threads\": [{\"item\": 7, \"thread\": 1, \"decision\": \"fix\"}]}",
    );
    assert!(
        !ask(&runner, &head, 1),
        "thread 2 is left, so one none holds it"
    );
    rig.clock.advance(quiet_backoff(1));
    assert!(ask(&runner, &head, 1), "one none held it, not four");
}
