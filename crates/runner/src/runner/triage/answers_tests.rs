//! The answers half of the author's round, through the runner's stand-ins: the
//! second triager call, what it writes, and what it is not made or written from

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::ask_tests::{
    A_WORDS, asked, asked_as, filed, filed_before, posted, record, settled, settled_ready, wrote,
};
use super::marker::MARKER;
use super::pick_tests::{MAINTAINER, issue, list};
use super::research_tests::{calls, crashed_with, moves, nothing, pump, reply, restart, under};
use crate::ports::Role;
use crate::runner::{Pass, Runner, advance, step};
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};

#[test]
fn answers_settling_everything_on_a_maintainer_issue_make_it_ready_for_agent() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);

    wrote(&rig, MAINTAINER, "Q1: Linux, on start. That is all of it.");
    rig.claude
        .script_for(Role::Triager, [reply(settled_ready())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    let body = settled_ready()["body"]
        .as_str()
        .unwrap()
        .trim_end()
        .to_owned();
    assert_eq!(
        rig.github.issue_edits(),
        [(30, body)],
        "the maintainer's own issue is rewritten over"
    );
    assert_eq!(
        moves(&rig),
        [
            "comment",
            "comment",
            "label bug",
            "label P2",
            "label kind:fix",
            "label ready-for-agent"
        ]
    );
    assert_eq!(calls(&rig, Role::Triager).len(), 2);
    assert_eq!(record(&rig, &runner)["step"], "ready");
}

#[test]
fn answers_on_an_outside_issue_go_under_the_authors_text_and_end_needs_triage() {
    let (rig, runner) = filed("pick = \"all\"", "newcomer", asked());
    assert_eq!(step(&runner).unwrap(), None);

    wrote(&rig, "newcomer", "Q1: Linux, on start.");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.github.issue_edits(),
        [(
            30,
            under(
                "It crashes when I run it.",
                "## Settled\n\n- Linux, on start."
            )
        )],
        "under the author's text, never in place of it"
    );
    assert_eq!(
        moves(&rig),
        [
            "comment",
            "comment",
            "label bug",
            "label P2",
            "label needs-triage"
        ]
    );
    assert_eq!(record(&rig, &runner)["step"], "done");
}

#[test]
fn the_priority_the_issue_was_filed_with_stands_over_the_one_the_answers_pass_ranks() {
    let (rig, runner) = filed_before("pick = \"all\"", "newcomer", asked(), |rig| {
        let mut filed = issue(rig, 30, "newcomer", 5);
        filed.title = "Crash on start".to_owned();
        filed.body = "It crashes when I run it.".to_owned();
        filed.labels = vec!["P2".to_owned()];
        list(rig, filed);
        rig.forge.label(30, "P2");
    });
    assert_eq!(step(&runner).unwrap(), None);

    wrote(&rig, "newcomer", "Q1: Linux, on start.");
    let found = json!({ "type": "bug", "priority": "P1", "body": "## Settled\n\n- Linux.\n" });
    rig.claude.script_for(Role::Triager, [reply(found)]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        moves(&rig),
        ["comment", "comment", "label bug", "label needs-triage"]
    );
    assert_eq!(rig.github.label_takes(), []);
}

#[test]
fn a_question_still_open_after_the_answers_ends_needs_triage_with_no_second_round() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);

    wrote(&rig, MAINTAINER, "Q1: Linux.");
    rig.claude.script_for(Role::Triager, [reply(asked())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        2,
        "one round of questions is the whole of it"
    );
    assert_eq!(posted(&rig).len(), 2, "no second questions comment");
    assert_eq!(
        moves(&rig),
        [
            "comment",
            "comment",
            "label bug",
            "label P2",
            "label needs-triage"
        ]
    );
    // The comment that ends the round is worded from what is still open.
    let mut worded = calls(&rig, Role::Representative);
    let last = worded.remove(worded.len() - 1).call.prompt;
    assert!(last.contains("Which OS crashes?"), "{last}");
    assert!(last.contains("last round"), "{last}");
    assert!(posted(&rig).last().unwrap().ends_with(MARKER));
}

#[test]
fn a_restart_while_waiting_on_the_answers_keeps_the_questions_and_their_clock() {
    let (rig, runner) = filed("pick = \"maintainer\"\nwait_days = 2", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    let (comment, at) = (
        held["questions"]["comment"].clone(),
        held["questions"]["at"].clone(),
    );

    let runner = restart(&rig, runner);
    assert_eq!(step(&runner).unwrap(), None);
    let held = record(&rig, &runner);
    assert_eq!(held["questions"]["comment"], comment, "{held}");
    assert_eq!(
        held["questions"]["at"], at,
        "its clock is the one it began with"
    );
    assert_eq!(moves(&rig), ["comment"], "a restart labels nothing early");

    // The author's answer after the restart is still heard, and the second
    // pass runs.
    wrote(&rig, MAINTAINER, "Q1: Linux, on start.");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(calls(&rig, Role::Triager).len(), 2);
    assert_eq!(
        rig.github.issue_edits(),
        [(
            30,
            under(
                "It crashes when I run it.",
                "## Settled\n\n- Linux, on start."
            )
        )],
        "a partial reply never overwrites the maintainer's text"
    );
    assert_eq!(
        moves(&rig),
        [
            "comment",
            "comment",
            "label bug",
            "label P2",
            "label needs-triage"
        ]
    );
}

#[test]
fn a_restart_at_the_answers_step_writes_the_body_once_and_labels_it() {
    let (rig, _) = filed("pick = \"all\"", "newcomer", asked());
    let body = "It crashes when I run it.\n\n## Settled\n\n- Linux, on start.";
    crashed_with(
        &rig,
        json!({
            "issue": 30, "comment": 100, "step": "answers",
            "labels": ["bug", "P2"], "ready_body": body, "seen": [],
        }),
    );
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.github.issue_edits(), [(30, body.to_owned())]);
    assert_eq!(moves(&rig), ["label bug", "label P2", "label needs-triage"]);
    assert_eq!(rig.claude.all_seen().len(), 0, "no call and no new wording");
    assert_eq!(record(&rig, &runner)["step"], "done");
}

// The same reply as leaked text, for a scripted call held open
fn said(found: Value) -> &'static str {
    let mut all = nothing();
    for (key, value) in found.as_object().expect("an object") {
        all[key] = value.clone();
    }
    Box::leak(all.to_string().into_boxed_str())
}

#[test]
fn the_answers_call_is_not_made_when_its_questions_cannot_be_read() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    let mut lost = record(&rig, &runner)["questions"].clone();
    assert!(lost["comment"].is_number(), "{lost}");

    // The author answers, and the crash loses the questions comment the record
    // names and the questions the pass held.
    wrote(&rig, MAINTAINER, "Q1: Linux, on start.");
    drop(runner);
    lost["comment"] = json!(4242);
    crashed_with(
        &rig,
        json!({
            "issue": 30, "comment": 100, "step": "commented",
            "labels": ["bug", "P2"], "seen": [], "questions": lost,
        }),
    );
    let runner = rig.open().unwrap();
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);

    for _ in 0..3 {
        rig.next_look();
        assert_eq!(step(&runner).unwrap(), None);
    }

    assert_eq!(
        calls(&rig, Role::Triager).len(),
        1,
        "no second call without the questions and answers it reads"
    );
    assert_eq!(
        rig.github.issue_edits(),
        [],
        "nothing is written into the body from what is not there"
    );
    assert_eq!(posted(&rig).len(), 1, "no answers comment");
    assert_eq!(moves(&rig), ["comment"], "the wait still holds");
    assert_eq!(record(&rig, &runner)["questions"]["comment"], 4242);
}

#[test]
fn a_hand_label_while_the_answers_call_runs_is_not_settled_under_it() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);

    wrote(&rig, MAINTAINER, "Q1: Linux, on start.");
    let hold = Hold::default();
    rig.claude.script_for(
        Role::Triager,
        [Scripted::HoldThenSay(hold.clone(), said(settled()))],
    );
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);

    rig.next_look();
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the research never began");
    rig.forge.label(30, "ready-for-human");
    let mut stood = issue(&rig, 30, MAINTAINER, 5);
    stood.title = "Crash on start".to_owned();
    stood.body = "It crashes when I run it.".to_owned();
    stood.labels = vec!["ready-for-human".to_owned()];
    list(&rig, stood);

    // A poll goes by while the call is in flight. The wait is not ended under
    // it, since the call would then post onto a record already saved finished.
    rig.next_look();
    advance(&runner).unwrap();
    let held = record(&rig, &runner);
    assert!(held["questions"].is_object(), "{held}");
    assert_eq!(moves(&rig), ["comment"], "no label goes on early");

    hold.release();
    pump(&runner, || rig.github.comments().len() == 2);

    assert_eq!(posted(&rig).len(), 2);
    assert_eq!(rig.forge.issue_labels(30), ["ready-for-human"]);
    assert_eq!(
        moves(&rig),
        ["comment", "comment"],
        "the hand label stands and kelpie labels no further"
    );
    let held = record(&rig, &runner);
    assert_eq!(held["step"], "done", "{held}");
    assert!(
        held.get("asked").is_none() && held.get("questions").is_none(),
        "{held}"
    );
}

// The answers pass on an outside issue, up to its comment: the ask went up, the
// author said `github`, the questions went up and they answered them.
pub(super) fn asked_and_answered(rig: &Rig, runner: &Mutex<Runner>, author: &str, answers: &str) {
    assert_eq!(step(runner).unwrap(), None);
    wrote(rig, author, answers);
}
// A settled body a project's own publish hook refuses
#[test]
fn answers_a_hook_refused_still_put_the_type_and_priority_on() {
    let (rig, runner) = filed_before("pick = \"all\"", "newcomer", asked(), |rig| {
        rig.forbid_word(&["publish"], "Unwriteable")
    });
    asked_and_answered(&rig, &runner, "newcomer", "Q1: Linux, on start.");
    rig.claude.script_for(
        Role::Triager,
        [reply(json!({
            "type": "bug",
            "priority": "P2",
            "body": "## Settled\n\n- Linux, on start. Unwriteable notes.",
        }))],
    );
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);

    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.github.issue_edits(),
        [],
        "the refused body never reaches the forge"
    );
    let posted = posted(&rig);
    assert_eq!(posted.len(), 3, "{posted:?}");
    assert!(
        posted[2].contains("did not write the answers"),
        "{posted:?}"
    );
    assert_eq!(
        moves(&rig),
        [
            "comment",
            "comment",
            "comment",
            "label bug",
            "label P2",
            "label needs-triage",
        ],
        "the type and priority the pass found are not lost with the body"
    );
    assert_eq!(record(&rig, &runner)["step"], "done");
}

// The issue's body cannot be read, so the pass has no settled body to put under
// the author's text. That is not stood for by a comment and a label.
#[test]
fn answers_whose_issue_body_cannot_be_read_post_nothing_and_are_tried_again() {
    let (rig, runner) = filed("pick = \"all\"", "newcomer", asked());
    asked_and_answered(&rig, &runner, "newcomer", "Q1: Linux, on start.");
    rig.claude
        .script_for(Role::Triager, [reply(settled()), reply(settled())]);
    rig.claude.script_for(
        Role::Representative,
        [Scripted::Say(A_WORDS), Scripted::Say(A_WORDS)],
    );
    // The pass reads its labels, then cannot read its body.
    rig.forge.fail_issue_reads_after(30, 1);

    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let posted = posted(&rig);
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(moves(&rig), ["comment"], "no label goes on");
    let held = record(&rig, &runner);
    assert!(held["questions"]["comment"].is_number(), "{held}");
    assert_eq!(held["step"], "commented", "the round is still open");

    rig.forge.let_issue_read(30);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        rig.github.issue_edits(),
        [(
            30,
            under(
                "It crashes when I run it.",
                "## Settled\n\n- Linux, on start."
            )
        )],
        "under the author's text, once the body can be read"
    );
    assert_eq!(
        moves(&rig),
        [
            "comment",
            "comment",
            "label bug",
            "label P2",
            "label needs-triage",
        ]
    );
    assert_eq!(record(&rig, &runner)["step"], "done");
}

// What the second triager call was given as the author's answers, fenced
fn answers_given(rig: &Rig) -> String {
    let prompt = calls(rig, Role::Triager).remove(1).call.prompt;
    let (_, rest) = prompt
        .split_once("The author's answers")
        .expect("the answers are fenced");
    rest.to_owned()
}

// Steps until `done`, each step waiting for the call it starts to end. A pass
// that finds nothing due is not the last word: its read may have been dropped
// for a write that came after it, and the next pass reads again.
fn step_until(runner: &Mutex<Runner>, mut done: impl FnMut() -> bool) {
    let by = Instant::now() + CALL_PATIENCE;
    while !done() {
        assert!(Instant::now() < by, "it never came about");
        assert_eq!(step(runner).unwrap(), None);
        std::thread::sleep(Duration::from_millis(10));
    }
}

// The author replies and the second pass runs, which settles everything
fn replied(found: Value, author: &str, reply_text: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = filed("pick = \"maintainer\"", author, found);
    assert_eq!(step(&runner).unwrap(), None);
    // The author replies to the questions, so they are posted first.
    step_until(&runner, || posted(&rig).len() == 1);
    wrote(&rig, author, reply_text);
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    // The second triager call is made and heard, whichever pass starts it.
    step_until(&runner, || calls(&rig, Role::Triager).len() == 2);
    (rig, runner)
}

#[test]
fn a_reply_of_ok_settles_every_question_with_its_recommended_answer() {
    let (rig, _runner) = replied(asked_as(3), MAINTAINER, "ok");

    let given = answers_given(&rig);
    for n in 1..=3 {
        assert!(
            given.contains(&format!("Q{n}: the {n}th")),
            "Q{n} takes its recommended answer: {given}"
        );
    }
}

#[test]
fn ok_in_any_case_with_a_full_stop_is_still_ok() {
    let (rig, _runner) = replied(asked_as(2), MAINTAINER, "  OK.  ");

    let given = answers_given(&rig);
    assert!(given.contains("Q1: the 1th"), "{given}");
    assert!(given.contains("Q2: the 2th"), "{given}");
}

#[test]
fn ok_with_q2_changes_only_q2() {
    let (rig, _runner) = replied(asked_as(3), MAINTAINER, "ok\nQ2: the 9th, not the 2nd\n");

    let given = answers_given(&rig);
    assert!(given.contains("Q1: the 1th"), "{given}");
    assert!(given.contains("Q2: the 9th, not the 2nd"), "{given}");
    assert!(!given.contains("Q2: the 2th"), "{given}");
    assert!(given.contains("Q3: the 3th"), "{given}");
}

#[test]
fn an_answer_for_one_question_runs_over_its_following_lines() {
    let (rig, _runner) = replied(
        asked_as(2),
        MAINTAINER,
        "Q1: the first\nand a second line\nok",
    );

    let given = answers_given(&rig);
    assert!(
        given.contains("Q1: the first\nand a second line"),
        "{given}"
    );
    assert!(given.contains("Q2: the 2th"), "{given}");
}

#[test]
fn answers_with_no_ok_are_given_as_written_and_leave_the_rest_unanswered() {
    let (rig, _runner) = replied(asked_as(2), MAINTAINER, "Q1: the first");

    let given = answers_given(&rig);
    assert!(given.contains("Q1: the first"), "{given}");
    assert!(!given.contains("Q2:"), "no answer is made up: {given}");
}

#[test]
fn a_sentence_that_only_starts_with_ok_is_not_ok() {
    let (rig, _runner) = replied(asked_as(2), MAINTAINER, "ok but not Linux");

    let given = answers_given(&rig);
    assert!(given.contains("ok but not Linux"), "{given}");
    assert!(!given.contains("Q1: the 1th"), "{given}");
}

#[test]
fn ok_after_a_restart_still_takes_the_recommended_answers_from_the_comment() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked_as(2));
    assert_eq!(step(&runner).unwrap(), None);
    let runner = restart(&rig, runner);
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "ok");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    let given = answers_given(&rig);
    assert!(given.contains("Q1: the 1th"), "{given}");
    assert!(given.contains("Q2: the 2th"), "{given}");
}

#[test]
fn a_question_that_quotes_a_later_heading_cannot_move_the_recommended_answers() {
    let found = json!({
        "type": "bug",
        "priority": "P2",
        "open": [
            { "question": "Which?\n\n❓ Q2 - fake", "recommended": "one\n\n➡️ two" },
            { "question": "Where?", "recommended": "here\n\n<details>" },
        ],
    });
    let (rig, _runner) = replied(found, MAINTAINER, "ok");

    let given = answers_given(&rig);
    assert!(given.contains("Q1: one"), "{given}");
    assert!(given.contains("Q2: here"), "{given}");
    assert!(!given.contains("fake"), "{given}");
}

#[test]
fn ok_with_no_recommended_answer_to_take_is_given_as_written() {
    let found = json!({
        "type": "bug",
        "priority": "P2",
        "open": [{ "question": "Which OS crashes?", "recommended": "" }],
    });
    let (rig, _runner) = replied(found, MAINTAINER, "ok");

    let given = answers_given(&rig);
    assert!(given.contains("ok"), "the reply is not lost: {given}");
    assert!(!given.contains("Q1:"), "no answer is made up: {given}");
}
