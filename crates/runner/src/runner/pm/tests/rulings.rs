//! The rulings `rulings.pm_decides` leaves to the project manager: its answer
//! is applied and posted, anything else goes to the maintainer with its pick,
//! and a `no <note>` on its answer reopens the ruling

use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{NOTHING, name_pm, pm_seen, step_until};
use crate::ports::{AgentError, Checks, Role, Timestamp};
use crate::runner::flight::advance;
use crate::runner::ruling::{Answerer, Record};
use crate::runner::{Answer, CHECKS_SETTLE, Runner, StepReport, step};
use crate::settings::{Merging, RulingName};
use crate::state::{Ruling, RulingKind, Stuck};
use crate::test::ticket;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};
use crate::work_item::{Phase, Turn};

/// A project whose project manager answers merge rulings, with CI green and
/// the worker parked on merge ruling 71, which is judged under `merging`
pub(super) fn left_to_pm(project: &str, merging: Merging) -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = Rig::with_pull_request_set(project, |rig| {
        rig.with_app();
        rig.maintainer("octocat");
        name_pm(rig);
    });
    // The settings file refuses `merge` as `true`, so a test sets it here.
    leave_merges(&runner);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. }) if id == ticket(71)
    ));
    // The ruling was raised under `ask`, and is judged under `merging`.
    runner.lock().unwrap().settings.git.merging = merging;
    (rig, runner)
}

/// Leaves merge rulings to the project manager
fn leave_merges(runner: &Mutex<Runner>) {
    let mut runner = runner.lock().unwrap();
    runner
        .settings
        .rulings
        .pm_decides
        .insert(RulingName::Merge, true);
}

/// The project manager's reply answering ruling 71 with `choice`
pub(super) fn answers(choice: char, design: bool) -> &'static str {
    let text = format!(
        "{{\"pick\": null, \"votes\": [{{\"ruling\": 71, \"choice\": \"{choice}\", \
         \"why\": \"PM-WHY\", \"design\": {design}}}]}}"
    );
    Box::leak(text.into_boxed_str())
}

/// The comments posted on pull request 71, oldest first
fn comments(rig: &Rig) -> Vec<String> {
    rig.github.comments().into_iter().map(|(_, b)| b).collect()
}

/// The comment that asks the maintainer, if one was posted
pub(super) fn asking(rig: &Rig) -> Option<String> {
    (comments(rig).into_iter()).find(|body| body.starts_with("### Merge this pull request"))
}

/// The comment that says the project manager answered, if one was posted
pub(super) fn answered(rig: &Rig) -> Option<String> {
    (comments(rig).into_iter()).find(|body| body.starts_with("**Answered by the project manager"))
}

/// Steps until one of the two comments is posted
pub(super) fn settled(rig: &Rig, runner: &Mutex<Runner>) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while Instant::now() < deadline {
        step(runner).unwrap();
        if asking(rig).is_some() || answered(rig).is_some() {
            return;
        }
        std::thread::yield_now();
    }
    panic!("no comment was posted: {:?}", comments(rig));
}

/// Whether ruling 71 still waits
fn pending(rig: &Rig) -> bool {
    let text = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: serde_json::Value = serde_json::from_str(&text).unwrap();
    state["rulings"].as_array().is_some_and(|r| !r.is_empty())
}

/// How many wakes asked the project manager for an answer to a ruling
fn asked(rig: &Rig) -> usize {
    (pm_seen(rig).iter())
        .filter(|s| s.call.prompt.contains("waits on your answer"))
        .count()
}

#[test]
fn a_kind_left_to_it_is_answered_by_the_project_manager_and_not_asked() {
    let (rig, runner) = left_to_pm("pm-answers", Merging::Auto);
    assert_eq!(asking(&rig), None, "the comment waits on the answer");
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(answers('A', false))]);

    settled(&rig, &runner);

    assert!(!pending(&rig), "the answer was applied");
    let said = answered(&rig).expect("the answer is posted");
    assert!(
        said.starts_with("**Answered by the project manager: A, Merge**"),
        "{said}"
    );
    assert!(
        said.contains("Reply `71 no <note>` to reopen it."),
        "{said}"
    );
    assert!(
        said.contains("**Project manager: Merge**. PM-WHY"),
        "{said}"
    );
    assert_eq!(asking(&rig), None, "the maintainer was never asked");
    assert_eq!(asked(&rig), 1);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["pm_answers"]["merge"], 1, "{status}");
}

#[test]
fn a_design_flag_goes_to_the_maintainer_with_the_project_managers_pick() {
    let (rig, runner) = left_to_pm("pm-design", Merging::Auto);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(answers('B', true))]);

    settled(&rig, &runner);

    assert!(pending(&rig), "the maintainer answers");
    assert_eq!(answered(&rig), None);
    let card = asking(&rig).expect("the ruling is posted for the maintainer");
    assert!(
        card.contains("\n\n**Project manager: Send back**. PM-WHY\n\n"),
        "{card}"
    );
}

#[test]
fn a_wake_that_gives_no_answer_sends_the_ruling_to_the_maintainer_as_it_stands() {
    let (rig, runner) = left_to_pm("pm-none", Merging::Auto);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    settled(&rig, &runner);

    assert!(pending(&rig));
    let card = asking(&rig).expect("the ruling is posted for the maintainer");
    assert!(!card.contains("**Project manager"), "{card}");
}

#[test]
fn a_merge_under_ask_goes_to_the_maintainer_whatever_the_project_manager_answers() {
    let (rig, runner) = left_to_pm("pm-ask", Merging::Ask);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(answers('A', false))]);

    settled(&rig, &runner);

    assert!(pending(&rig));
    let card = asking(&rig).expect("the ruling is posted for the maintainer");
    assert!(
        card.contains("**Project manager: Merge**. PM-WHY"),
        "{card}"
    );
}

#[test]
fn a_kind_not_left_to_it_is_posted_to_the_maintainer_at_once() {
    let (rig, runner, head) = Rig::with_pull_request_set("pm-mine", |rig| {
        rig.with_app();
        rig.maintainer("octocat");
        name_pm(rig);
    });
    rig.forge.set_checks(&head, Checks::Passed);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    rig.verdict(&runner);

    assert!(asking(&rig).is_some(), "{:?}", comments(&rig));
    assert_eq!(asked(&rig), 0);
}

#[test]
fn a_ruling_raised_in_a_drain_goes_to_the_maintainer_at_once() {
    let (rig, runner, head) = Rig::with_pull_request_set("pm-drain", |rig| {
        rig.with_app();
        rig.maintainer("octocat");
        name_pm(rig);
    });
    leave_merges(&runner);
    rig.forge.set_checks(&head, Checks::Passed);
    rig.ask(&runner, "drain", None);

    rig.verdict(&runner);
    step(&runner).unwrap();

    assert!(asking(&rig).is_some(), "{:?}", comments(&rig));
    assert_eq!(asked(&rig), 0);
}

#[test]
fn a_turn_kelpie_could_not_set_up_goes_to_the_maintainer_even_when_left_to_it() {
    let rig = Rig::new("pm-setup");
    rig.with_app();
    rig.maintainer("octocat");
    name_pm(&rig);
    rig.edit_settings(|s| {
        format!("{s}\n[app.dogs.kelpie.rulings]\npm_decides = {{ turn-failed = true }}\n")
    });
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let why = "cannot empty worker/settings-7.tmp: permission denied";
    rig.claude
        .script([Scripted::Fail(AgentError::Setup(why.into()))]);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    let deadline = Instant::now() + CALL_PATIENCE;
    let comment = loop {
        step(&runner).unwrap();
        if let Some(body) = comments(&rig).into_iter().next() {
            break body;
        }
        assert!(Instant::now() < deadline, "no ruling was posted");
        std::thread::yield_now();
    };

    assert!(
        comment.starts_with("### Kelpie could not set up the worker's turn."),
        "{comment}"
    );
    assert_eq!(asked(&rig), 0);
}

#[test]
fn a_failure_the_project_manager_answered_on_the_item_is_not_left_to_it_again() {
    let (rig, runner) = super::with_pm("pm-again");
    let failing = |why: &str| {
        let kind = RulingKind::from(Stuck::TurnFailed {
            why: why.into(),
            phase: Phase::Implement,
            retry: Turn::Due,
            restore: Vec::new(),
            setup: false,
        });
        Ruling {
            issue: Some(ticket(7)),
            ..Ruling::new(ticket(7), why, kind)
        }
    };
    let mut runner = runner.lock().unwrap();
    (runner.settings.rulings.pm_decides).insert(RulingName::TurnFailed, true);
    let answered = failing("harness exited with 1");
    assert!(runner.pm_answers_ruling(&answered));
    let record = Record::of(&answered, &Answer::Yes, Timestamp(1), Answerer::Pm);
    record.append(&rig.paths().rulings).unwrap();

    assert!(
        !runner.pm_answers_ruling(&failing("harness exited with 1")),
        "the same failure on the same item is the maintainer's"
    );
    assert!(runner.pm_answers_ruling(&failing("harness exited with 2")));
}

// Reopening a ruling the project manager answered, from its thread

/// [`left_to_pm`], answered with option A, the maintainer able to write
fn answered_a(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = left_to_pm(project, Merging::Auto);
    rig.forge.threads.grant_write("octocat");
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(answers('A', false))]);
    settled(&rig, &runner);
    assert!(answered(&rig).is_some(), "{:?}", comments(&rig));
    rig.clock.advance(5);
    (rig, runner)
}

#[test]
fn a_no_note_reply_to_its_answer_sends_the_worker_the_note() {
    let (rig, runner) = answered_a("reopen");

    let reply = rig.writes_on(71, "octocat", "71 no add a test for the empty case");
    rig.claude.script([Scripted::Say(
        "<kelpie-question>\nWhich?\n</kelpie-question>\n",
    )]);
    rig.next_look();
    step(&runner).unwrap(); // reads the reply, reopens the ruling, and the worker takes its note

    assert_eq!(rig.github.reactions(), [reply], "kelpie took the reply");
    let note = "add a test for the empty case";
    assert!(
        rig.claude.calls().iter().any(|c| c.prompt.contains(note)),
        "the worker was never sent the note"
    );
}

#[test]
fn a_reply_that_is_no_no_note_does_not_reopen_it_and_says_so() {
    let (rig, runner) = answered_a("keeps");

    rig.writes_on(71, "octocat", "71 yes");
    rig.next_look();
    step(&runner).unwrap();

    let said = comments(&rig).pop().expect("kelpie replied");
    assert!(said.contains("Ruling #71 was not reopened"), "{said}");
    assert!(said.contains("only a `no <note>` reopens it"), "{said}");
}

#[test]
fn an_answer_whose_work_item_has_finished_cannot_be_reopened() {
    let (rig, runner) = answered_a("finished");
    // The yes merges it.
    step(&runner).unwrap();
    rig.clock.advance(CHECKS_SETTLE);
    step(&runner).unwrap();
    rig.clock.advance(5);

    rig.writes_on(71, "octocat", "71 no too late");
    rig.next_look();
    step(&runner).unwrap();

    let said = comments(&rig).pop().expect("kelpie replied");
    assert!(said.contains("Ruling #71 was not reopened"), "{said}");
    assert!(said.contains("finished"), "{said}");
}

#[test]
fn a_no_that_names_no_ruling_says_how_to_reopen_it() {
    let (rig, runner) = answered_a("unnamed");

    rig.writes_on(71, "octocat", "no add a test");
    rig.next_look();
    step(&runner).unwrap();

    let said = comments(&rig).pop().expect("kelpie replied");
    assert!(said.contains("Ruling #71 was not reopened"), "{said}");
    assert!(said.contains("`71 no <note>`"), "{said}");
    assert!(rig.github.reactions().is_empty(), "nothing was taken");
}

// A ruling held for the project manager's answer

/// Whether ruling 71's comment is still to be posted, as the state keeps it
fn unposted(rig: &Rig) -> bool {
    let text = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: serde_json::Value = serde_json::from_str(&text).unwrap();
    state["rulings"][0]["unposted"] == true
}

/// Advances the runner until the project manager's call for ruling 71 is
/// held open
fn wake_held(rig: &Rig, runner: &Mutex<Runner>) -> Hold {
    let hold = Hold::default();
    rig.claude.script_for(
        Role::Pm,
        [Scripted::HoldThenSay(hold.clone(), answers('A', false))],
    );
    advanced_until(runner, || hold.entered(Duration::ZERO));
    hold
}

/// Advances the runner, without waiting on a call in flight, until `done`
fn advanced_until(runner: &Mutex<Runner>, done: impl Fn() -> bool) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "the runner never got there");
        advance(runner).unwrap();
        std::thread::yield_now();
    }
}

#[test]
fn a_runner_restarted_while_a_ruling_is_held_posts_its_comment() {
    let (rig, runner) = left_to_pm("pm-restart", Merging::Auto);
    assert_eq!(asking(&rig), None, "the comment waits on the answer");
    assert!(unposted(&rig));
    drop(runner);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    let runner = rig.open().unwrap();
    settled(&rig, &runner);

    assert!(asking(&rig).is_some(), "{:?}", comments(&rig));
    assert!(!unposted(&rig));
}

#[test]
fn a_ruling_whose_work_item_is_dropped_while_held_is_never_posted() {
    let (rig, runner) = left_to_pm("pm-dropped", Merging::Auto);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(answers('A', false))]);
    rig.ask(&runner, "drop", Some("7"));

    for _ in 0..5 {
        step(&runner).unwrap();
    }

    assert_eq!(asking(&rig), None, "{:?}", comments(&rig));
    assert_eq!(answered(&rig), None, "{:?}", comments(&rig));
}

#[test]
fn a_ruling_held_through_a_drain_goes_to_the_maintainer_and_says_why() {
    let (rig, runner) = left_to_pm("pm-drained", Merging::Auto);
    let hold = wake_held(&rig, &runner);

    rig.ask(&runner, "drain", None);
    advanced_until(&runner, || asking(&rig).is_some());

    let card = asking(&rig).expect("the ruling is posted for the maintainer");
    assert!(!card.contains("**Project manager"), "{card}");
    let notes = runner.lock().unwrap().take_notes().join("\n");
    assert!(
        notes.contains(
            "ruling 71 goes to the maintainer without the project manager's answer, since the \
             runner drains"
        ),
        "{notes}"
    );
    hold.release();
}

#[test]
fn an_answer_after_the_maintainer_was_alerted_is_dropped_and_the_ruling_still_waits() {
    let (rig, runner) = left_to_pm("pm-late", Merging::Auto);
    let hold = wake_held(&rig, &runner);
    rig.ask(&runner, "drain", None);
    advanced_until(&runner, || !rig.alerts.posts().is_empty());

    hold.release();
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));

    let (_, acted, _) = super::answered(Some(report));
    assert_eq!(acted, ["left ruling 71: it already went to the maintainer"]);
    assert!(pending(&rig), "the ruling still waits");
    assert_eq!(answered(&rig), None, "no answer was posted");
}
