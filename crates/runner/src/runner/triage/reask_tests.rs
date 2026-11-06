//! What a `shep kelpie triage` pass asked again over an issue kelpie already
//! triaged reads, and what it asks again, through the runner's stand-ins

use super::ask_tests::{
    A_WORDS, ASK_WORDS, asked, asked_as, asked_one, asked_second, filed, posted, record, settled,
    wrote,
};
use super::pick_tests::MAINTAINER;
use super::research_tests::{calls, reply};
use crate::ports::Role;
use crate::runner::step;
use crate::test::Scripted;

// #1194: `shep kelpie triage` over an issue kelpie already triaged asked the
// settled questions again. The pass is now given the earlier triage comments
// and their author's replies, and a question the author answered is not asked
// again.
#[test]
fn a_pass_asked_again_over_a_triaged_issue_reads_the_rounds_it_ran() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "ok");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(record(&rig, &runner)["step"], "done");

    // The verb asks again, and the triager answers with the same open question
    // and a recommendation against what the author took.
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(Role::Triager, [reply(asked())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let prompts: Vec<String> = (calls(&rig, Role::Triager))
        .into_iter()
        .map(|seen| seen.call.prompt)
        .collect();
    let last = prompts.last().unwrap();
    assert!(
        last.contains("Which OS crashes?"),
        "the earlier questions reach the triager: {last}"
    );
    assert!(
        last.contains(&format!("{MAINTAINER} wrote:")),
        "the author's reply reaches the triager: {last}"
    );
    assert!(
        last.contains("Do not ask again in `open` what those rounds settled"),
        "{last}"
    );
    let comments = posted(&rig);
    let again = comments.last().unwrap();
    assert!(
        !again.contains("Which OS crashes?"),
        "the settled question is not posted again: {again}"
    );
    assert!(!again.contains("Q1 - "), "no questions block: {again}");
}

// #1304: only what the author answered is settled. The first round asked two
// questions and the author answered one, so the re-ask puts the other back.
#[test]
fn a_reask_asks_again_the_question_its_author_left_unanswered() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked_as(2));
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "Q2: the second one");
    rig.claude.script_for(Role::Triager, [reply(asked_as(1))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(Role::Triager, [reply(asked_as(2))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let again = posted(&rig).last().expect("a comment").clone();
    assert!(
        again.contains("Which one, 1?"),
        "the question its author left unanswered is asked again: {again}"
    );
    assert!(
        !again.contains("Which one, 2?"),
        "the one it answered is not: {again}"
    );
}

// #1304 round 2: the author's reply to a later round answers that round alone.
// The questions an earlier round asked and never heard an answer to are still
// open, and the next pass asks them.
#[test]
fn a_reask_does_not_take_a_later_round_s_answer_for_an_earlier_one() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked_as(2));
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(
        Role::Triager,
        [reply(asked_one("Which display server?", "X11"))],
    );
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "Q1: X11, on Wayland.");
    rig.claude.script_for(Role::Triager, [reply(asked_as(1))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(Role::Triager, [reply(asked_as(2))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let again = posted(&rig).last().expect("a comment").clone();
    for n in 1..=2 {
        assert!(
            again.contains(&format!("Which one, {n}?")),
            "the question no round heard an answer to is asked again: {again}"
        );
    }
    assert!(
        !again.contains("Which display server?"),
        "the question its author answered to that round is not: {again}"
    );
}

// #1304 round 2: only the form the questions comment asked for settles a round.
// An author whose sentence starts like `ok` answered no question kelpie can
// name, and reading what they meant is the triager's job, not the filter's.
#[test]
fn an_author_s_words_that_are_not_an_ok_settle_nothing() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "Okay, any update on this?");
    rig.claude.script_for(Role::Triager, [reply(asked_as(1))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(Role::Triager, [reply(asked())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let again = posted(&rig).last().expect("a comment").clone();
    assert!(
        again.contains("Which OS crashes?"),
        "the question its sentence answered nothing for is asked again: {again}"
    );
}

// #1304 round 2: the settled questions are compared in one case, so a question
// holding a letter outside ASCII is known for the one its author answered.
#[test]
fn a_settled_question_holding_a_letter_outside_ascii_is_not_asked_again() {
    let (rig, runner) = filed(
        "pick = \"maintainer\"",
        MAINTAINER,
        asked_second("\u{00c7}a plante sous Wayland ?", "Oui"),
    );
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "Q1: Oui, sous Wayland.");
    rig.claude.script_for(Role::Triager, [reply(asked_as(1))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(
        Role::Triager,
        [reply(asked_second(
            "\u{00c7}a plante sous Wayland ?",
            "Oui",
        ))],
    );
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let again = posted(&rig).last().expect("a comment").clone();
    assert!(
        again.contains("Which one, 2?"),
        "the unanswered one is asked again: {again}"
    );
    assert!(
        !again.contains("plante sous Wayland"),
        "the question its author answered is not asked again: {again}"
    );
}

// #1304 round 2: a question is compared as the questions block wrote it, so the
// marks it took out and the line it broke at do not hide what was settled.
#[test]
fn a_settled_question_the_questions_block_rewrote_is_not_asked_again() {
    let (rig, runner) = filed(
        "pick = \"maintainer\"",
        MAINTAINER,
        asked_second("Does ```kelpie``` crash on start?", "No"),
    );
    assert_eq!(step(&runner).unwrap(), None);
    wrote(&rig, MAINTAINER, "Q1: No, only under Wayland.");
    rig.claude.script_for(Role::Triager, [reply(asked_as(1))]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.ask(&runner, "triage", Some("30"));
    rig.claude.script_for(
        Role::Triager,
        [reply(asked_second(
            "Does ```kelpie``` crash on start?",
            "No",
        ))],
    );
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(ASK_WORDS)]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    let again = posted(&rig).last().expect("a comment").clone();
    assert!(
        again.contains("Which one, 2?"),
        "the unanswered one is asked again: {again}"
    );
    assert!(
        !again.contains("Does kelpie crash on start?"),
        "the question its author answered is not asked again: {again}"
    );
}
