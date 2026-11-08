//! Who a triage comment greets, and the marker it ends with, through the
//! runner's stand-ins

use serde_json::json;

use super::answers_tests::step_until;
use super::ask_tests::{A_WORDS, asked, filed, posted, settled, wrote};
use super::marker::MARKER;
use super::pick_tests::{MAINTAINER, list, rig_with};
use super::research_tests::{APP, WORDS, calls, crash, facts, handed, reply};
use crate::ports::Role;
use crate::runner::{Runner, step};
use crate::test::{Rig, Scripted};
use std::sync::Mutex;

// The first pass on an issue kelpie's own App opened, its questions comment up
fn asked_of_the_maintainer() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = filed("pick = \"maintainer\"", APP, asked());
    assert_eq!(step(&runner).unwrap(), None);
    step_until(&runner, || posted(&rig).len() == 1);
    (rig, runner)
}

#[test]
fn a_first_comment_on_an_app_opened_issue_greets_the_maintainer() {
    let (rig, _runner) = asked_of_the_maintainer();

    let prompt = facts(&rig);
    assert!(
        prompt.contains(&format!(
            "Greet @{MAINTAINER} by name and lead with what kelpie found."
        )),
        "{prompt}"
    );
    assert!(
        prompt.contains("Thank nobody for the report: kelpie's own App opened this issue."),
        "{prompt}"
    );
    assert!(!prompt.contains("At most one plain thanks"), "{prompt}");
    assert!(
        prompt.contains(&format!("What @{MAINTAINER} wrote")),
        "{prompt}"
    );
}

#[test]
fn an_answer_round_on_an_app_opened_issue_greets_no_one() {
    let (rig, runner) = asked_of_the_maintainer();

    wrote(&rig, MAINTAINER, "Q1: Linux, on start.");
    rig.claude.script_for(Role::Triager, [reply(settled())]);
    rig.claude
        .script_for(Role::Representative, [Scripted::Say(A_WORDS)]);
    rig.next_look();
    step_until(&runner, || calls(&rig, Role::Representative).len() == 2);

    let prompt = calls(&rig, Role::Representative).remove(1).call.prompt;
    assert!(
        prompt.contains(
            "The post goes on this issue, where you have posted before. Greet nobody, thank \
             nobody, and open with what changed since your last post."
        ),
        "{prompt}"
    );
    assert!(!prompt.contains("by name"), "{prompt}");
    assert!(!prompt.contains("At most one plain thanks"), "{prompt}");
    assert!(!prompt.contains(&format!("Greet @{APP}")), "{prompt}");
    assert!(
        !prompt.contains(&format!("Greet @{MAINTAINER}")),
        "{prompt}"
    );
    assert_eq!(posted(&rig).len(), 2, "the answers comment went up");
    assert_eq!(posted(&rig)[1], format!("{A_WORDS}\n\n{MARKER}"));
}

#[test]
fn a_comment_whose_words_hold_the_marker_posts_one_marker() {
    let echoed = format!("{WORDS}\n\n{MARKER}");
    let rig = rig_with("pick = \"all\"");
    list(&rig, crash(&rig, "It crashes when I run it."));
    rig.claude.script_for(
        Role::Representative,
        [Scripted::Say(Box::leak(echoed.into_boxed_str()))],
    );
    rig.claude.script_for(Role::Triager, [reply(json!({}))]);
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    let comments = posted(&rig);
    assert_eq!(comments, [handed(WORDS)], "{comments:?}");
    assert_eq!(comments[0].matches(MARKER).count(), 1, "{}", comments[0]);
}
