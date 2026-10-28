//! A hold waits on the open work item it names, and on any when it names none

use std::sync::Mutex;

use serde_json::{Value, json};

use super::{answered, pm_on, step_until};
use crate::runner::{Runner, StepReport, step};
use crate::test::{FakeForge, Rig, Scripted};

/// A project with three slots and as many pending rulings, #7 and #8 open and stuck, #9 and #12 ready,
/// whose project manager answered `answer` to the wake for the free slot
fn held_by(answer: &'static str) -> (Rig, Mutex<Runner>, Vec<String>) {
    let rig = Rig::new("koji");
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 3"));
    rig.edit_settings(|s| s.replace("pending_rulings = 2", "pending_rulings = 3"));
    let (rig, runner) = pm_on(rig);
    for item in ["7", "8"] {
        rig.ask(&runner, "add", Some(item));
        step_until(&runner, |r| matches!(r, StepReport::Failed { .. }));
    }
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(answer)]);
    // The board reads again once its poll is due.
    rig.clock.advance(61);
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));
    assert_eq!(dropped, Vec::<String>::new());
    (rig, runner, acted)
}

/// The issues the project manager holds, as `status` shows them
fn held(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    rig.ask(runner, "status", None)["pm"]["held"].clone()
}

const UNTIL_7: &str = "{\"pick\": null, \"hold\": [{\"issue\": 9, \"until\": 7}]}";

#[test]
fn a_hold_until_one_item_survives_another_closing_and_ends_when_it_closes() {
    let (rig, runner, acted) = held_by(UNTIL_7);
    assert_eq!(acted, ["holds #9 until #7"]);
    assert_eq!(held(&rig, &runner), json!([{ "issue": "9", "until": "7" }]));

    rig.ask(&runner, "drop", Some("8"));
    assert_eq!(
        held(&rig, &runner),
        json!([{ "issue": "9", "until": "7" }]),
        "#8 closing is no close it waits on"
    );
    step(&runner).unwrap();
    let board = std::fs::read_to_string(rig.paths().board).unwrap();
    assert!(board.contains("held by you until #7 closes"), "{board}");

    rig.ask(&runner, "drop", Some("7"));
    assert_eq!(held(&rig, &runner), json!(null), "#7 closing ends it");
}

#[test]
fn a_later_answer_with_no_hold_leaves_the_hold_and_an_empty_list_clears_it() {
    let (rig, runner, _) = held_by(UNTIL_7);
    // With #9 the only ready issue, no worker starts to take a scripted reply.
    rig.forge.unlist_ready(12);
    rig.clock.advance(61);
    step(&runner).unwrap();
    rig.claude
        .script([Scripted::Say("{\"reply\": \"Noted.\"}")]);
    rig.ask(&runner, "tell", Some("go on"));
    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));
    assert_eq!((acted, dropped), (vec![], vec![]));

    assert_eq!(
        held(&rig, &runner),
        json!([{ "issue": "9", "until": "7" }]),
        "an answer that says nothing of holds leaves them"
    );

    rig.claude.script([Scripted::Say("{\"hold\": []}")]);
    rig.ask(&runner, "tell", Some("and now"));
    step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    assert_eq!(
        held(&rig, &runner),
        json!(null),
        "an empty list clears them"
    );
}

#[test]
fn a_hold_with_no_until_ends_at_any_close() {
    let (rig, runner, acted) = held_by("{\"pick\": null, \"hold\": [9]}");
    assert_eq!(acted, ["holds #9"]);
    assert_eq!(held(&rig, &runner), json!([{ "issue": "9" }]));

    rig.ask(&runner, "drop", Some("8"));
    assert_eq!(held(&rig, &runner), json!(null));
}

#[test]
fn a_hold_until_what_is_no_open_work_item_is_refused_with_a_note() {
    let rig = Rig::new("koji");
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 3"));
    let (rig, runner) = pm_on(rig);
    rig.ask(&runner, "add", Some("7"));
    step_until(&runner, |r| matches!(r, StepReport::Failed { .. }));
    rig.forge.list_ready(9, false);
    rig.forge.list_ready(12, false);
    rig.claude.script([Scripted::Say(
        "{\"pick\": null, \"hold\": [{\"issue\": 9, \"until\": 12}, {\"issue\": 12, \"until\": 7}]}",
    )]);

    let report = step_until(&runner, |r| matches!(r, StepReport::PmAnswered { .. }));
    let (_, acted, dropped) = answered(Some(report));

    assert_eq!(dropped, ["hold #9: #12 is no open work item"]);
    assert_eq!(acted, ["holds #12 until #7"]);
    assert_eq!(
        held(&rig, &runner),
        json!([{ "issue": "12", "until": "7" }])
    );
}

#[test]
fn a_hold_ends_when_the_held_issue_is_edited() {
    let (rig, runner, _) = held_by(UNTIL_7);

    let edited = format!("{}\nA new line.\n", FakeForge::passing_body(9));
    rig.forge.set_ready_body(9, &edited);
    // The board reads again once its look is due.
    rig.clock.advance(61);
    step(&runner).unwrap();

    assert_eq!(held(&rig, &runner), json!(null));
}
