//! A triage comment as a reply, through the runner's stand-ins: what the
//! answers round is worded against, how the ask says to answer, and the lists
//! the reader can already see on the issue

use serde_json::json;

use super::answers_tests::{answers_called, questions_posted, step_until};
use super::ask::REPLY_HINT;
use super::ask_tests::{A_WORDS, asked, asked_as, filed, posted, settled, wrote};
use super::marker::MARKER;
use super::pick_tests::MAINTAINER;
use super::research_tests::{APP, calls, facts, moves, reply};
use crate::ports::Role;
use crate::runner::step;
use crate::test::{Rig, Scripted};

// What the representative was asked to word on its `n`th call, oldest first
fn worded(rig: &Rig, n: usize) -> String {
    calls(rig, Role::Representative).remove(n).call.prompt
}

// The text of the prompt's "What @login wrote" block, unwrapped from its fence
fn wrote_block(prompt: &str) -> String {
    let (_, rest) = prompt
        .split_once("What @")
        .expect("a block naming who wrote");
    let (_, body) = rest.split_once(" begin\n").expect("its opening line");
    body.split_once("\nUNTRUSTED")
        .expect("its closing line")
        .0
        .to_owned()
}

// The kind line the call was given, which is all a post is told to do
fn kind_of(prompt: &str) -> String {
    let (_, rest) = prompt.split_once("Kind of post: ").expect("the kind line");
    rest.split_once("\n\nFacts:")
        .expect("the facts under it")
        .0
        .to_owned()
}

#[test]
fn an_answers_round_shows_the_representative_the_authors_answer() {
    let (rig, runner) = filed("pick = \"all\"", "newcomer", asked());
    questions_posted(&rig, &runner);

    let answer = "Q1: Linux, on start. It dies the moment I open it.";
    wrote(&rig, "newcomer", answer);
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    answers_called(&rig, &runner, 2);

    let prompt = worded(&rig, 1);
    assert!(prompt.contains("What @newcomer wrote"), "{prompt}");
    assert_eq!(
        wrote_block(&prompt),
        answer,
        "the comment answers their words"
    );
    assert!(
        !prompt.contains("It crashes when I run it."),
        "the issue body is what the triager reads, not what the comment answers: {prompt}"
    );
}

#[test]
fn an_ok_answer_reaches_the_representative_as_ok() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked_as(3));
    questions_posted(&rig, &runner);

    wrote(&rig, MAINTAINER, "ok");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    answers_called(&rig, &runner, 2);

    // The expanded answers go to the triager, which reads them against the
    // questions. The wording gets the reply as the author wrote it.
    let block = wrote_block(&worded(&rig, 1));
    assert_eq!(block, "ok", "{block}");
    assert!(!block.contains("Q1:"), "{block}");
}

#[test]
fn an_answers_round_on_an_app_issue_names_the_maintainer_who_answered() {
    let (rig, runner) = filed("pick = \"maintainer\"", APP, asked());
    questions_posted(&rig, &runner);

    wrote(&rig, MAINTAINER, "Q1: Linux, on start.");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    answers_called(&rig, &runner, 2);

    let prompt = worded(&rig, 1);
    assert!(
        prompt.contains(&format!("What @{MAINTAINER} wrote")),
        "{prompt}"
    );
    assert!(!prompt.contains(&format!("What @{APP} wrote")), "{prompt}");
    assert_eq!(
        wrote_block(&prompt),
        "Q1: Linux, on start.",
        "the maintainer's words, not the App's report"
    );
}

#[test]
fn an_ask_comment_still_says_how_to_answer_under_the_words() {
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, asked());
    assert_eq!(step(&runner).unwrap(), None);
    step_until(&runner, || posted(&rig).len() == 1);

    let comment = posted(&rig).remove(0);
    assert!(comment.contains(REPLY_HINT), "{comment}");
    assert!(comment.contains("<details>"), "{comment}");
    assert!(comment.ends_with(MARKER), "{comment}");

    let kind = kind_of(&facts(&rig));
    for forced in [
        "answer them all in one comment",
        "collapsed",
        "the maintainer triages",
    ] {
        assert!(
            !kind.contains(forced),
            "the kind still says {forced}: {kind}"
        );
    }
    assert!(
        kind.contains("Do not restate or renumber the questions"),
        "{kind}"
    );
}

#[test]
fn the_facts_mark_labels_files_and_criteria_as_already_on_the_issue() {
    let found = json!({
        "type": "bug",
        "priority": "P2",
        "files": ["README.md"],
        "tests": ["runner::triage::it_posts"],
        "criteria": ["The comment names the answer."],
    });
    let (rig, runner) = filed("pick = \"maintainer\"", MAINTAINER, found);
    assert_eq!(step(&runner).unwrap(), None);
    step_until(&runner, || posted(&rig).len() == 1);

    let said = facts(&rig);
    assert!(
        said.contains("The labels are already on the issue."),
        "{said}"
    );
    for heading in [
        "Files the work would touch, already on the issue:",
        "Tests that would pin it, already on the issue:",
        "Draft acceptance criteria, already on the issue:",
    ] {
        assert!(said.contains(heading), "{said}");
    }
    assert_eq!(said.matches("already on the issue").count(), 4, "{said}");

    // The claim holds for a pass that labels beside its comment: the labels the
    // facts name go on right after it.
    step_until(&runner, || moves(&rig).len() == 4);
    assert_eq!(
        moves(&rig),
        ["comment", "label bug", "label P2", "label needs-approval"],
        "the labels the facts name go on right after the comment"
    );
}

#[test]
fn an_ask_round_says_its_labels_come_when_the_triage_ends() {
    let (rig, runner) = filed("pick = \"all\"", "newcomer", asked());
    assert_eq!(step(&runner).unwrap(), None);
    step_until(&runner, || posted(&rig).len() == 1);

    let said = facts(&rig);
    assert!(
        said.contains("The labels are not on the issue yet."),
        "{said}"
    );
    assert!(said.contains(" when this triage ends."), "{said}");
    assert!(!said.contains("already on the issue"), "{said}");
    assert_eq!(
        moves(&rig),
        ["comment"],
        "the ask posts its comment and labels nothing"
    );
}
