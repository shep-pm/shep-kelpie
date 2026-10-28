//! A turn waiting for its model, driven a pass at a time through the
//! runner's stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use super::super::tests::{CODER, Driven};
use crate::ports::{AgentError, CallActivity, Session, Timestamp};
use crate::runner::{Runner, StepReport};
use crate::settings::Harness;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted, ticket};

/// The time of day `at` is, as the board writes it
fn clock(at: u64) -> String {
    let seconds = at % 86_400;
    format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
}

/// A rig listing `listed`, and `coder` on a local model
fn rig(project: &str, listed: &[&str]) -> Rig {
    let rig = Rig::new(project);
    rig.write_agent("coder", CODER);
    rig.implementers(listed);
    rig
}

/// Queues the next call at its model's lease until the hold is released, then
/// has it do `then`
fn queued(rig: &Rig, then: Scripted) -> Hold {
    let queue = Hold::default();
    let call = Scripted::Queued(queue.clone(), Box::new(then));
    rig.claude.script([call]);
    queue
}

/// Adds `issue`, which the agent board ranks `agent` alone for
fn add_on(rig: &Rig, runner: &Mutex<Runner>, issue: u64, agent: &str) {
    rig.agent_board.ranks(&[agent]);
    rig.ask(runner, "add", Some(&issue.to_string()));
    rig.agent_board.ranks_listed();
}

/// The work item for `issue` as `status` shows it
fn item(rig: &Rig, runner: &Mutex<Runner>, issue: u64) -> Value {
    let status = rig.ask(runner, "status", None);
    let items = status["work_items"].as_array().unwrap();
    let named = issue.to_string();
    let found = items.iter().find(|i| i["issue"] == named);
    found.expect("the work item is open").clone()
}

fn board(rig: &Rig) -> String {
    std::fs::read_to_string(&rig.paths().board).unwrap()
}

#[test]
fn a_turn_queued_for_its_lease_is_marked_waiting_until_it_begins() {
    let rig = rig("acme", &["sonnet-high", "coder"]);
    let runner = rig.open().unwrap();
    add_on(&rig, &runner, 7, "coder");
    let hold = Hold::default();
    let queue = queued(&rig, Scripted::Hold(hold.clone()));
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None, "waiting is not a report");
    rig.clock.advance(120);
    driven.pass();
    let since = json!({ "since": Rig::EPOCH, "why": "lease" });
    assert_eq!(item(&rig, &runner, 7)["waiting"], since);
    let line = format!(
        "  - Waiting for its model since {} (2m): for a lease on its model\n",
        clock(Rig::EPOCH)
    );
    assert!(board(&rig).contains(&line), "{}", board(&rig));

    queue.release();
    assert_eq!(driven.heard(), None, "the grant is not a report");
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");
    driven.pass();
    assert_eq!(item(&rig, &runner, 7).get("waiting"), None);
    assert!(!board(&rig).contains("Waiting for its model"));
    hold.release();
    assert!(driven.landed().is_some());
}

#[test]
fn a_silent_turn_is_marked_after_ten_minutes_and_cleared_by_its_first_output() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    rig.claude.set_activity(CallActivity::Nothing);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");
    rig.clock.advance(10 * 60 - 1);
    driven.pass();
    assert_eq!(item(&rig, &runner, 7).get("waiting"), None);
    rig.clock.advance(1);
    driven.pass();
    let since = json!({ "since": Rig::EPOCH, "why": "silent" });
    assert_eq!(item(&rig, &runner, 7)["waiting"], since);
    assert!(board(&rig).contains("(10m): for its model's first output\n"));

    let now = Rig::EPOCH + 10 * 60;
    rig.claude.set_activity(CallActivity::At(Timestamp(now)));
    driven.pass();
    assert_eq!(item(&rig, &runner, 7).get("waiting"), None);
    hold.release();
    assert!(driven.landed().is_some());
}

#[test]
fn a_later_turn_restarted_on_a_new_session_is_marked_silent() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say("done")]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(matches!(
       driven.landed(),
       Some(StepReport::Ended { issue, .. }) if issue == ticket(7)
    ));
    let session = serde_json::from_value(item(&rig, &runner, 7)["session"].clone()).unwrap();
    let (hold, again) = (Hold::default(), Hold::default());
    let gone = AgentError::NoSession(Harness::StandIn, session);
    let restart = Scripted::Hold(again.clone());
    rig.claude
        .script([Scripted::HoldThenFail(hold.clone(), gone), restart]);
    rig.claude.set_activity(CallActivity::Nothing);
    driven.started();
    assert!(hold.entered(CALL_PATIENCE), "the later turn never began");
    hold.release();
    assert_eq!(driven.heard(), None, "the restart is not a report");
    assert!(again.entered(CALL_PATIENCE), "the restart never began");
    let calls = rig.claude.all_calls();
    assert!(matches!(calls[2].session, Session::New(_)), "{calls:#?}");
    rig.clock.advance(10 * 60);
    driven.pass();
    assert_eq!(item(&rig, &runner, 7)["waiting"]["why"], "silent");
    again.release();
    assert!(matches!(
       driven.landed(),
       Some(StepReport::Ended { issue, .. }) if issue == ticket(7)
    ));
}

// An implementer on a model behind a gateway, whose calls take a lease on it
const BEHIND_PADDOCK: &str = "---\nrole: implementer\nharness: pi\nmodel: coder\neffort: low\n\
                              gateway: paddock\ncontext: 65536\n---\n";

const CODER_MODEL: [(&str, &str); 1] = [("model", "coder")];

// A rig whose `mine` implementer is behind a gateway nothing listens at, with `items` slots
fn behind_gateway(items: u32) -> Rig {
    let rig = Rig::new("acme");
    rig.write_agent("mine", BEHIND_PADDOCK);
    rig.implementers(&["sonnet-high", "mine"]);
    rig.edit_settings(|s| s.replace("active_items = 1", &format!("active_items = {items}")));
    let settings = std::fs::read_to_string(rig.kelpie_settings_file()).unwrap();
    let gateway =
        "[gateways.paddock]\nurl = \"http://127.0.0.1:9\"\nkey_env = \"CARGO_PKG_NAME\"\n";
    rig.set_kelpie_settings(&format!("{settings}{gateway}"));
    rig
}

#[test]
fn a_call_waiting_for_its_model_s_lease_is_counted_waiting_then_held_from_the_grant() {
    let rig = behind_gateway(1);
    let runner = rig.open().unwrap();
    add_on(&rig, &runner, 7, "mine");
    let hold = Hold::default();
    let queue = queued(&rig, Scripted::Hold(hold.clone()));
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None);
    assert_eq!(
        rig.samples
            .value("kelpie_model_lease_waiting", &CODER_MODEL),
        Some(1.0)
    );
    assert_eq!(
        rig.samples.value("kelpie_model_lease_held", &CODER_MODEL),
        Some(0.0)
    );

    queue.release();
    assert_eq!(driven.heard(), None);
    assert_eq!(
        rig.samples
            .value("kelpie_model_lease_waiting", &CODER_MODEL),
        Some(0.0)
    );
    assert_eq!(
        rig.samples.value("kelpie_model_lease_held", &CODER_MODEL),
        Some(1.0)
    );

    hold.release();
    assert!(driven.landed().is_some());
    driven.pass();
    assert_eq!(
        rig.samples.value("kelpie_model_lease_held", &CODER_MODEL),
        Some(0.0)
    );
}

#[test]
fn lease_wait_seconds_and_grants_add_up_per_model_across_calls() {
    let rig = behind_gateway(2);
    let runner = rig.open().unwrap();
    add_on(&rig, &runner, 7, "mine");
    add_on(&rig, &runner, 8, "mine");
    let queues = [
        queued(&rig, Scripted::Say("done")),
        queued(&rig, Scripted::Say("done")),
    ];
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None);
    driven.started();
    assert_eq!(driven.heard(), None);
    assert_eq!(
        rig.samples
            .value("kelpie_model_lease_waiting", &CODER_MODEL),
        Some(2.0)
    );

    rig.clock.advance(120);
    queues[0].release();
    driven.landed();
    rig.clock.advance(30);
    queues[1].release();
    driven.landed();

    let seconds = rig
        .samples
        .value("kelpie_model_lease_wait_seconds_total", &CODER_MODEL);
    assert_eq!(seconds, Some(120.0 + 150.0));
    assert_eq!(
        rig.samples
            .value("kelpie_model_lease_grants_total", &CODER_MODEL),
        Some(2.0)
    );
}
