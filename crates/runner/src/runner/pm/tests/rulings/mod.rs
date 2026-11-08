//! The rulings `rulings.pm_decides` leaves to the project manager: its answer
//! is applied and posted, anything else goes to the maintainer with its pick,
//! and a `no <note>` on its answer reopens the ruling

use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::asked::posted;
use super::{NOTHING, name_pm, pm_seen, step_until, with_pm};
use crate::ports::{
    AgentError, Checks, Cost, Finding, ForgeError, Role, SessionId, Severity, Timestamp, Usage,
};
use crate::runner::flight::advance;
use crate::runner::ruling::{Answerer, Record};
use crate::runner::{Answer, CHECKS_SETTLE, Runner, StepReport, step};
use crate::settings::{Harness, RulingName};
use crate::state::{Ruling, RulingKind, Stuck};
use crate::test::ticket;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted, git};
use crate::work_item::{Awaited, Phase, Review, ReviewStage, Turn, WorkItem};

mod gated;

/// A project whose project manager answers merge rulings, with CI green and
/// the worker parked on merge ruling 71
///
/// Under `merge` `true` a head every gate passes merges unasked, and one no
/// reviewer read goes back through a pass of the review, so the gate raises a
/// merge ruling only over rivals, a borrowed head or a refused comment.
pub(super) fn left_to_pm(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = Rig::with_pull_request_set(project, |rig| {
        rig.with_app();
        rig.maintainer("octocat");
        name_pm(rig);
    });
    leave_merges(&runner);
    rig.forge.set_checks(&head, Checks::Passed);
    runner
        .lock()
        .unwrap()
        .raise(71, merge_ruling(&head))
        .unwrap();
    assert_eq!(
        runner.lock().unwrap().state.rulings.len(),
        1,
        "the item is parked on it"
    );
    (rig, runner)
}

/// A merge ruling with nothing to warn of, as the gate raises one for a head
/// every gate passed
fn merge_ruling(head: &str) -> RulingKind {
    RulingKind::Merge {
        head: head.to_owned(),
        unreviewed: None,
        open_threads: None,
        held_by: Vec::new(),
        unread_head: false,
        note_fix: false,
        late_fix: false,
        nits: None,
        open_found: Vec::new(),
        disputes: None,
        rivals: Vec::new(),
    }
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
    let (rig, runner) = left_to_pm("pm-answers");
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
    let (rig, runner) = left_to_pm("pm-design");
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
    let (rig, runner) = left_to_pm("pm-none");
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    settled(&rig, &runner);

    assert!(pending(&rig));
    let card = asking(&rig).expect("the ruling is posted for the maintainer");
    assert!(!card.contains("**Project manager"), "{card}");
}

/// Sets a flag or note of the merge ruling's kind
fn on_merge(ruling: &mut Ruling, set: impl FnOnce(&mut RulingKind)) {
    assert!(matches!(ruling.kind, RulingKind::Merge { .. }));
    set(&mut ruling.kind);
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
    // A head every gate passes merges unasked under `merge`, and one no reviewer
    // read goes back through a pass of the review, so the drain raises its ruling
    // over another open pull request covering the same issue.
    rig.forge.open_pull_request(72, "someone/7", &[7]);
    rig.forge.set_from_fork(72);
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
    let (rig, runner) = left_to_pm(project);
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
    let (rig, runner) = left_to_pm("pm-restart");
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
    let (rig, runner) = left_to_pm("pm-dropped");
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
    let (rig, runner) = left_to_pm("pm-drained");
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
    let (rig, runner) = left_to_pm("pm-late");
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

// An over-budget ruling the map leaves to the maintainer: the project manager is woken to
// recommend, and nothing is applied

const DOLLAR: u64 = 1_000_000_000;

/// The project manager's vote on over-budget ruling 7, choosing `choice`
fn votes_on_7(choice: char) -> &'static str {
    let text = format!(
        "{{\"pick\": null, \"votes\": [{{\"ruling\": 7, \"choice\": \"{choice}\", \
         \"why\": \"PM-WHY\", \"design\": false}}]}}"
    );
    Box::leak(text.into_boxed_str())
}

/// A project whose issue 7 has spent $6 against a $5 cap, `map` the text after its settings, with
/// a project manager named when `pm`, the worker's next turn not yet started
fn spent_over(project: &str, map: &str, pm: bool) -> (Rig, Mutex<Runner>) {
    spent_over_with(project, map, pm, |_| {})
}

/// [`spent_over`], with `forge` set up on the rig before its runner opens
fn spent_over_with(
    project: &str,
    map: &str,
    pm: bool,
    forge: impl FnOnce(&Rig),
) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    forge(&rig);
    rig.with_app();
    rig.maintainer("octocat");
    if pm {
        name_pm(&rig);
    }
    rig.edit_settings(|s| {
        format!(
            "{}\n{map}",
            s.replace("per_item_usd = 10", "per_item_usd = 5")
        )
    });
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let usage = Usage {
        input: 1,
        cache_write: 10,
        cache_write_5m: 0,
        cache_read: 100,
        output: 1000,
    };
    (rig.claude).script([Scripted::Reply(usage, Cost(6 * DOLLAR))]);
    step(&runner).unwrap();
    (rig, runner)
}

const TRUE_MAP: &str = "[app.dogs.kelpie.rulings]\npm_decides = { over-budget = true }\n";

/// The comment that asks the maintainer about issue 7's cap, if one was posted
fn cap_card(rig: &Rig) -> Option<String> {
    (comments(rig).into_iter())
        .find(|body| body.starts_with("### The work item is over its budget"))
}

/// Steps until the cap's card or the project manager's answer is posted
fn cap_settled(rig: &Rig, runner: &Mutex<Runner>) {
    let deadline = Instant::now() + CALL_PATIENCE;
    while Instant::now() < deadline {
        step(runner).unwrap();
        if cap_card(rig).is_some() || answered(rig).is_some() {
            return;
        }
        std::thread::yield_now();
    }
    panic!("nothing was posted: {:?}", comments(rig));
}

/// The cap issue 7 holds, in dollars
fn cap_usd(rig: &Rig, runner: &Mutex<Runner>) -> f64 {
    let status = rig.ask(runner, "status", None);
    status["work_item"]["budget"]["cap_usd"].as_f64().unwrap()
}

/// Has the project manager read the wake of a project spent over its cap, and says what its
/// board held
fn wake_board(rig: &Rig, runner: &Mutex<Runner>) -> String {
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    cap_settled(rig, runner);
    std::fs::read_to_string(rig.paths().pm.join("board.md")).unwrap()
}

/// [`wake_board`] for a project whose ruling is raised before `then` changes what the wake reads
fn wake_board_after(rig: &Rig, runner: &Mutex<Runner>, then: impl FnOnce()) -> String {
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    step(runner).unwrap(); // raises the ruling
    then();
    cap_settled(rig, runner);
    std::fs::read_to_string(rig.paths().pm.join("board.md")).unwrap()
}

#[test]
fn an_over_budget_ruling_wakes_the_project_manager_and_holds_its_comment() {
    let (rig, runner) = spent_over("ob-wakes", "", true);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(votes_on_7('A'))]);

    step(&runner).unwrap();

    assert_eq!(cap_card(&rig), None, "the comment waits on the answer");
    assert!(unposted_7(&runner), "held for the wake");
    cap_settled(&rig, &runner);
    assert_eq!(asked(&rig), 1);
}

/// Whether ruling 7's comment is still to be posted
fn unposted_7(runner: &Mutex<Runner>) -> bool {
    runner.lock().unwrap().state.rulings[0].unposted
}

#[test]
fn an_over_budget_ruling_reaches_the_maintainer_with_the_recommendation_on_its_card() {
    let (rig, runner) = spent_over("ob-card", "", true);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(votes_on_7('B'))]);

    cap_settled(&rig, &runner);

    let card = cap_card(&rig).expect("the maintainer is asked");
    assert!(card.contains("**Project manager: No**. PM-WHY"), "{card}");
    assert_eq!(answered(&rig), None, "nothing was answered for them");
    assert!(pending_7(&runner), "the maintainer answers");
    assert_eq!(cap_usd(&rig, &runner), 5.0, "the cap is where it was");
    let status = rig.ask(&runner, "status", None);
    let question = status["rulings"][0]["question"].as_str().unwrap();
    assert!(
        question.ends_with("\n\nPM recommends: No\nPM says: PM-WHY"),
        "{question}"
    );
}

/// Whether ruling 7 still waits
fn pending_7(runner: &Mutex<Runner>) -> bool {
    !runner.lock().unwrap().state.rulings.is_empty()
}

#[test]
fn a_wake_with_no_answer_sends_an_over_budget_ruling_as_it_stands() {
    let (rig, runner) = spent_over("ob-none", "", true);
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);

    cap_settled(&rig, &runner);

    let card = cap_card(&rig).expect("the maintainer is asked");
    assert!(!card.contains("**Project manager"), "{card}");
    assert!(pending_7(&runner));
    assert_eq!(asked(&rig), 1);
}

#[test]
fn an_over_budget_ruling_set_true_is_answered_as_before() {
    let (rig, runner) = spent_over("ob-true", TRUE_MAP, true);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(votes_on_7('A'))]);

    cap_settled(&rig, &runner);

    let said = answered(&rig).expect("the project manager answered");
    assert!(
        said.starts_with("**Answered by the project manager: A, Yes**"),
        "{said}"
    );
    assert!(said.contains("Reply `7 no <note>` to reopen it."), "{said}");
    assert_eq!(cap_card(&rig), None, "the maintainer was never asked");
    assert!(!pending_7(&runner), "its answer was applied");
    assert_eq!(cap_usd(&rig, &runner), 10.0, "the cap doubled");
}

#[test]
fn an_over_budget_ruling_in_a_drain_goes_to_the_maintainer_at_once() {
    let (rig, runner) = spent_over("ob-drain", "", true);
    rig.ask(&runner, "drain", None);

    cap_settled(&rig, &runner);

    assert!(cap_card(&rig).is_some(), "{:?}", comments(&rig));
    assert_eq!(asked(&rig), 0, "no wake");
    assert_eq!(cap_usd(&rig, &runner), 5.0);
}

#[test]
fn a_project_with_no_project_manager_is_asked_with_no_wake() {
    let (rig, runner) = spent_over("ob-no-pm", "", false);

    cap_settled(&rig, &runner);

    let card = cap_card(&rig).expect("the maintainer is asked");
    assert!(!card.contains("**Project manager"), "{card}");
    assert!(pm_seen(&rig).is_empty(), "no wake");
}

/// A project whose issue 7 is in review of pull request 71 and has spent $6 against a $5 cap,
/// with `mark` set on the work item first
fn in_review_over(project: &str, mark: impl FnOnce(&mut WorkItem)) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.with_app();
    rig.maintainer("octocat");
    name_pm(&rig);
    rig.edit_settings(|s| s.replace("per_item_usd = 10", "per_item_usd = 5"));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap();
    let call = |item: &mut WorkItem| {
        let session = SessionId("spent".into());
        let cost = Some(Cost(6 * DOLLAR));
        item.record_call(Role::Worker, Timestamp(1), session, Usage::default(), cost);
        mark(item);
    };
    runner.lock().unwrap().update(call).unwrap();
    (rig, runner)
}

#[test]
fn the_wake_shows_the_phase_round_findings_and_ci_of_an_over_budget_item() {
    let (rig, runner) = in_review_over("ob-review", |item| {
        let found = |severity, what: &str| Finding::new(severity, "src/a.rs", 1, what, "why");
        item.phase = Phase::Review(Review {
            round: 2,
            stage: ReviewStage::Found {
                findings: vec![
                    found(Severity::High, "one"),
                    found(Severity::Low, "two"),
                    found(Severity::Low, "three"),
                ],
                threads: Vec::new(),
            },
            ..Review::first()
        });
        item.red_head = Some("abcdef0123".into());
        item.held = vec![found(Severity::Medium, "four")];
        item.disputes = Vec::new();
    });

    let board = wake_board(&rig, &runner);

    let [_, section] = board.split("## Over-budget rulings").collect::<Vec<_>>()[..] else {
        panic!("no over-budget section: {board}");
    };
    for line in [
        "### Ruling 71 on #7\n",
        "- Whose call: the maintainer's: your option is a recommendation, and nothing is applied\n",
        "- How close: review round 2, the worker fixing its findings\n",
        "- CI: red at abcdef0, sent to the worker\n",
    ] {
        assert!(section.contains(line), "{line:?} is not in {section}");
    }
    let findings = "- Open findings: 1 high, 0 medium, 2 low in the round; ";
    assert!(section.contains(findings), "{section}");
    assert!(section.contains("; 0 disputed\n"), "{section}");
}

#[test]
fn the_wake_reads_the_run_a_round_awaited_before_an_older_red() {
    let (rig, runner) = in_review_over("ob-green", |item| {
        item.phase = Phase::Review(Review {
            round: 2,
            stage: ReviewStage::Found {
                findings: vec![Finding::new(Severity::Low, "src/a.rs", 1, "one", "why")],
                threads: Vec::new(),
            },
            ci: Some(Box::new(Awaited {
                head: "fedcba9876".into(),
                since: Timestamp(1),
                green: true,
            })),
            ..Review::first()
        });
        item.red_head = Some("abcdef0123".into());
    });

    let board = wake_board(&rig, &runner);

    assert!(board.contains("- CI: green at fedcba9\n"), "{board}");
}

#[test]
fn the_wake_says_when_ci_has_not_run_on_the_head() {
    let (rig, runner) = in_review_over("ob-no-ci", |_| {});

    let board = wake_board(&rig, &runner);

    assert!(board.contains("- CI: not run on this head\n"), "{board}");
}

#[test]
fn the_wake_shows_the_priority_touched_files_and_spend_against_the_cap() {
    let (rig, runner) = in_review_over("ob-critical", |item| {
        // One raise doubled the cap to $10, which $5 more passes.
        item.budget_raises = 1;
        let (session, cost) = (SessionId("more".into()), Some(Cost(5 * DOLLAR)));
        item.record_call(Role::Worker, Timestamp(2), session, Usage::default(), cost);
    });
    rig.forge.label(7, "priority: P1");

    let board = wake_board(&rig, &runner);

    let critical =
        "- How critical: priority P1; touches 1 file(s), full (diff touched work.txt): work.txt\n";
    assert!(board.contains(critical), "{board}");
    let spend = "- Spend: $11.00 against a cap of $10.00 (the worker $11.00, the reviewers $0.00), \
                 the cap raised 1 time before\n";
    assert!(board.contains(spend), "{board}");
}

#[test]
fn a_priority_that_cannot_be_read_says_so_and_the_wake_goes_on() {
    let (rig, runner) = spent_over("ob-unread", "", true);

    let board = wake_board_after(&rig, &runner, || {
        let down = ForgeError::Failed("the forge is down".into());
        rig.forge.set_issue_read_error(7, down);
    });

    assert!(
        board.contains("- How critical: priority not read; "),
        "{board}"
    );
    assert!(cap_card(&rig).is_some(), "the wake went on to its card");
}

#[test]
fn a_pm_decide_on_an_over_budget_ruling_set_false_applies_its_answer() {
    let (rig, runner) = spent_over("ob-decide", "", true);
    rig.forge.threads.grant_write("octocat");
    rig.claude.script_for(Role::Pm, [Scripted::Say(NOTHING)]);
    cap_settled(&rig, &runner);
    assert!(pending_7(&runner), "the maintainer is asked");
    rig.clock.advance(5);
    rig.claude
        .script_for(Role::Pm, [Scripted::Say(votes_on_7('A'))]);

    rig.writes_on(7, "octocat", "7 pm decide");
    rig.next_look();
    let said = posted(&rig, &runner, |c| {
        c.starts_with("**Answered by the project manager")
    });

    assert!(
        said.starts_with("**Answered by the project manager: A, Yes**"),
        "{said}"
    );
    assert!(!pending_7(&runner), "its answer was applied");
    assert_eq!(cap_usd(&rig, &runner), 10.0, "the cap doubled");
}

// A timed-out or failed turn's ruling keeps the commit its worktree stood at,
// and a head git cannot read leaves none with the reason in the notes

// Where a turn's last change sits, past the ten minutes a turn may be quiet for
const STALE: u64 = Rig::EPOCH - 11 * 60;

/// A project with a project manager, #7 open, and its worker's turn ended by
/// `end`, which parks it on a ruling. The rig, its runner, and the commit its
/// worktree stood at.
fn parked_at(project: &str, end: Scripted) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner) = with_pm(project);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([end]);
    step_until(&runner, |r| {
        matches!(r, StepReport::TimedOut { .. } | StepReport::Failed { .. })
    });
    let head = git(&rig.worktree_7(), &["rev-parse", "HEAD"]);
    (rig, runner, head)
}

/// The one ruling the state file holds, as JSON
fn raised(rig: &Rig) -> serde_json::Value {
    let text = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: serde_json::Value = serde_json::from_str(&text).unwrap();
    state["rulings"].as_array().expect("a ruling")[0].clone()
}

#[test]
fn a_timed_out_turns_ruling_keeps_the_worktrees_head() {
    let (rig, _runner, head) = parked_at(
        "stuck-head-timeout",
        Scripted::TimeOutPlanting("stale.rs", "fn stale() {}\n", STALE),
    );

    assert_eq!(
        raised(&rig)["stuck_head"],
        head,
        "the commit it was raised at"
    );
}

#[test]
fn a_failed_turns_ruling_keeps_the_worktrees_head() {
    let (rig, _runner, head) = parked_at(
        "stuck-head-failed",
        Scripted::Fail(AgentError::Failed(Harness::ClaudeCode, "overloaded".into())),
    );

    assert_eq!(
        raised(&rig)["stuck_head"],
        head,
        "the commit it was raised at"
    );
}

#[test]
fn a_commit_the_worker_never_pushed_is_the_head_it_keeps() {
    let (rig, _runner, head) = parked_at(
        "stuck-head-unpushed",
        Scripted::TimeOutCommitting("done.rs", "fn done() {}\n", STALE),
    );
    let main = git(&rig.repo(), &["rev-parse", "origin/main"]);

    assert_eq!(raised(&rig)["stuck_head"], head);
    assert_ne!(head, main, "the worker's own commit is the head");
    assert_eq!(rig.forge.head_of("kelpie/7"), None, "it pushed nothing");
}

#[test]
fn a_head_git_cannot_read_leaves_the_ruling_standing_with_none() {
    let (rig, runner) = with_pm("stuck-head-unreadable");
    rig.ask(&runner, "add", Some("7"));
    std::fs::create_dir_all(rig.worktree_7()).unwrap();
    std::fs::write(rig.worktree_7().join("stray.txt"), "not a worktree\n").unwrap();

    step_until(&runner, |r| matches!(r, StepReport::Failed { .. }));

    let ruling = raised(&rig);
    assert_eq!(ruling["kind"]["reason"], "turn-failed", "{ruling}");
    assert_eq!(ruling.get("stuck_head"), None, "the head was not read");
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("could not read the head ruling")),
        "{notes:?}"
    );
}

#[test]
fn another_kind_of_ruling_keeps_no_head() {
    let (rig, _runner, _) = Rig::parked_set("stuck-head-merge", name_pm);

    let ruling = raised(&rig);

    assert_eq!(ruling["kind"]["kind"], "merge", "{ruling}");
    assert_eq!(ruling.get("stuck_head"), None);
}
