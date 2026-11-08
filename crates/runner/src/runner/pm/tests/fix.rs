//! What the project manager does with a stuck item whose cause lies in the
//! system and a fix for it is already open

use std::sync::Mutex;
use std::time::Instant;

use serde_json::Value;

use super::{answered, past_the_window, step_until, with_pm};
use crate::ports::AgentError;
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::{CALL_PATIENCE, Rig, Scripted};

/// What kelpie could not set up, in the forge's own words
const SETUP: &str = "cannot empty worker/settings-7.tmp: permission denied";

/// The project manager parking `item` on `on`, saying `why`
fn parks(item: u64, on: u64, why: &str) -> &'static str {
    let text = format!(
        "{{\"pick\": null, \"fix\": {{\"item\": {item}, \"on\": {on}, \"why\": \"{why}\"}}}}"
    );
    Box::leak(text.into_boxed_str())
}

/// The project manager parking #7 on the open fix #12, saying `why`
fn parks_7(why: &str) -> &'static str {
    parks(7, 12, why)
}

/// The project manager parking #7 and #9 both on the open fix #12
const PARKS_TWO: &str = "{\"pick\": null, \"fix\": [{\"item\": 7, \"on\": 12, \"why\": \"a \
                         machine fault\"}, {\"item\": 9, \"on\": 12, \"why\": \"the same fault\"}]}";

fn failed() -> AgentError {
    AgentError::Failed(Harness::ClaudeCode, "the parser fights the lexer".into())
}

/// A project with a project manager and #7's worker parked on ruling 7 by a
/// turn that failed, its wake not yet answered
fn parked(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = with_pm(project);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Fail(failed())]);
    step_until(&runner, |r| matches!(r, StepReport::Failed { .. }));
    (rig, runner)
}

/// ... and #9's worker parked on ruling 9 a minute later
fn two_parked(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = parked(project);
    rig.clock.advance(60);
    rig.claude.script([Scripted::Fail(failed())]);
    rig.ask(&runner, "add", Some("9"));
    step_until(
        &runner,
        |r| matches!(r, StepReport::Failed { issue, .. } if *issue == crate::test::ticket(9)),
    );
    (rig, runner)
}

/// A project with #7 parked on ruling 7 by a turn kelpie could not set up
fn parked_setup(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = with_pm(project);
    rig.ask(&runner, "add", Some("7"));
    rig.claude
        .script([Scripted::Fail(AgentError::Setup(SETUP.into()))]);
    step_until(&runner, |r| matches!(r, StepReport::Failed { .. }));
    (rig, runner)
}

/// The question of ruling `id`, as `status` shows it
fn asked(rig: &Rig, runner: &Mutex<Runner>, id: u64) -> String {
    let status = rig.ask(runner, "status", None);
    let named = id.to_string();
    let rulings = status["rulings"].as_array().expect("a list of rulings");
    let found: &Value = (rulings.iter())
        .find(|r| r["id"] == named)
        .expect("the ruling");
    found["question"].as_str().expect("a question").to_owned()
}

/// The wake answered, and what it did and dropped
fn answered_wake(rig: &Rig, runner: &Mutex<Runner>) -> (Vec<String>, Vec<String>) {
    past_the_window(rig);
    let (_, acted, dropped) = answered(Some(step_until(runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    (acted, dropped)
}

#[test]
fn a_fix_on_an_open_issue_parks_the_item_and_its_card_names_the_fix() {
    let (rig, runner) = parked("abra");
    rig.claude
        .script([Scripted::Say(parks_7("a machine fault, not the item's"))]);

    let (acted, dropped) = answered_wake(&rig, &runner);

    assert_eq!(acted, ["parked #7 on #12"]);
    assert_eq!(dropped, Vec::<String>::new());
    let question = asked(&rig, &runner, 7);
    assert!(
        question.ends_with(
            "\n\nThe project manager parks this on #12. PM says: a machine fault, not the item's"
        ),
        "{question}"
    );
    assert_eq!(rig.claude.calls().len(), 1, "the worker stays parked");
}

#[test]
fn two_items_may_park_on_one_fix() {
    let (rig, runner) = two_parked("abra2");
    rig.claude.script([Scripted::Say(PARKS_TWO)]);

    let (acted, dropped) = answered_wake(&rig, &runner);

    assert_eq!(acted, ["parked #7 on #12", "parked #9 on #12"]);
    assert_eq!(dropped, Vec::<String>::new());
    for id in [7, 9] {
        assert!(
            asked(&rig, &runner, id).contains("The project manager parks this on #12. PM says: "),
            "{id}"
        );
    }
}

#[test]
fn a_fix_on_a_closed_issue_is_dropped_and_named() {
    let (rig, runner) = parked("abra3");
    rig.forge.close_issue(12);
    rig.claude
        .script([Scripted::Say(parks_7("a machine fault"))]);

    let (acted, dropped) = answered_wake(&rig, &runner);

    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(dropped, ["fix #7: #12 is closed"]);
    assert!(
        !asked(&rig, &runner, 7).contains("parks this on"),
        "the ruling is left as it stands"
    );
}

#[test]
fn a_fix_on_the_item_itself_is_dropped_and_named() {
    let (rig, runner) = parked("abra4");
    rig.claude
        .script([Scripted::Say(parks(7, 7, "it is its own fix"))]);

    let (acted, dropped) = answered_wake(&rig, &runner);

    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(dropped, ["fix #7: it cannot be its own fix"]);
}

#[test]
fn a_fix_on_an_item_not_parked_is_dropped_and_named() {
    let (rig, runner) = parked("abra5");
    rig.claude
        .script([Scripted::Say(parks(12, 13, "no work item"))]);

    let (acted, dropped) = answered_wake(&rig, &runner);

    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(
        dropped,
        ["fix #12: it is no stuck work item parked on a ruling"]
    );
    assert!(
        !asked(&rig, &runner, 7).contains("parks this on"),
        "the stuck item is left alone too"
    );
}

#[test]
fn a_fix_on_a_setup_failure_parks_it() {
    let (rig, runner) = parked_setup("abra6");
    rig.claude
        .script([Scripted::Say(parks_7("the machine, not the item"))]);

    let (acted, dropped) = answered_wake(&rig, &runner);

    assert_eq!(acted, ["parked #7 on #12"]);
    assert_eq!(dropped, Vec::<String>::new());
    assert!(
        asked(&rig, &runner, 7).contains("The project manager parks this on #12"),
        "{}",
        asked(&rig, &runner, 7)
    );
    assert_eq!(rig.claude.calls().len(), 1, "no turn is tried again");
}

/// The card kelpie sends the maintainer for ruling 7 after the answer, which
/// names the fix the item waits on. The re-post is the webhook alert, which
/// carries the ruling's words as they now stand.
fn reposted_card(rig: &Rig, runner: &Mutex<Runner>) -> String {
    let deadline = Instant::now() + CALL_PATIENCE;
    loop {
        let named = (rig.alerts.posts()).into_iter().find_map(|(_, alert)| {
            alert
                .text
                .contains("parks this on")
                .then(|| alert.text.clone())
        });
        if let Some(card) = named {
            return card;
        }
        assert!(Instant::now() < deadline, "no card names the fix");
        step(runner).unwrap();
        std::thread::yield_now();
    }
}

#[test]
fn the_card_posted_again_names_the_fix_the_item_waits_on() {
    let (rig, runner) = parked("abra8");
    rig.claude
        .script([Scripted::Say(parks_7("a machine fault, not the item's"))]);

    let (acted, dropped) = answered_wake(&rig, &runner);
    assert_eq!(acted, ["parked #7 on #12"]);
    assert_eq!(dropped, Vec::<String>::new());

    let card = reposted_card(&rig, &runner);
    assert!(
        card.contains(
            "The project manager parks this on #12. PM says: a machine fault, not the item's"
        ),
        "{card}"
    );
}

#[test]
fn an_item_named_by_an_unstick_and_a_fix_acts_once() {
    let (rig, runner) = parked("abra9");
    const BOTH: &str = "{\"pick\": null, \"unstick\": {\"item\": 7, \"action\": \"ask\", \
                        \"why\": \"the machine, not the item\"}, \"fix\": {\"item\": 7, \
                        \"on\": 12, \"why\": \"and its fix\"}}";
    rig.claude.script([Scripted::Say(BOTH)]);

    let (acted, dropped) = answered_wake(&rig, &runner);

    assert_eq!(acted, ["put #7 to the maintainer on ruling 7"]);
    assert_eq!(dropped, ["fix #7: the answer names it twice"]);
    let question = asked(&rig, &runner, 7);
    assert_eq!(
        question.matches("PM says:").count(),
        1,
        "one line for one item: {question}"
    );
}

#[test]
fn a_retry_on_a_setup_failure_is_refused_and_named() {
    let (rig, runner) = parked_setup("abra7");
    let retry: &'static str = Box::leak(
        "{\"pick\": null, \"unstick\": {\"item\": 7, \"action\": \"retry\", \"why\": \"try again\"}}"
            .into(),
    );
    rig.claude.script([Scripted::Say(retry)]);

    let (acted, dropped) = answered_wake(&rig, &runner);

    assert_eq!(acted, Vec::<String>::new());
    assert_eq!(
        dropped,
        [
            "retry #7: kelpie could not set up that turn, and the fix is on this machine, so \
         another try meets the same fault"
        ]
    );
    assert_eq!(rig.claude.calls().len(), 1, "no turn is tried again");
}
