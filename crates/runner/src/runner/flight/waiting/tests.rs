//! A turn waiting for its model, and a first turn falling back, driven a
//! pass at a time through the runner's stand-ins

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use super::super::tests::{CEILING, CODER, Driven};
use crate::ports::{AgentError, CallActivity, Session, Timestamp};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::ticket;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted};
use crate::ticket::TicketId;

/// The time of day `at` is, as the board writes it
fn clock(at: u64) -> String {
    let seconds = at % 86_400;
    format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
}

/// A rig listing `listed`, `coder` and `coder2` on a local model, with
/// `fallback_after` set to `minutes` when given
fn rig(project: &str, listed: &[&str], minutes: Option<u32>) -> Rig {
    let rig = Rig::new(project);
    rig.write_agent("coder", CODER);
    rig.write_agent("coder2", CODER);
    rig.implementers(listed);
    if let Some(minutes) = minutes {
        let at = "\nimplementers = ";
        rig.edit_settings(|s| s.replace(at, &format!("\nfallback_after = {minutes}{at}")));
    }
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
    let rig = rig("acme", &["sonnet-high", "coder"], None);
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
fn a_first_turn_waiting_past_fallback_after_moves_to_the_next_implementer() {
    let rig = rig("acme", &["coder", "sonnet-high"], Some(15));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let first = item(&rig, &runner, 7)["session"].clone();
    let queue = queued(&rig, Scripted::Say("done"));
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None);
    rig.clock.advance(15 * 60 - 1);
    driven.pass();
    assert_eq!(item(&rig, &runner, 7)["agent"], "coder");
    rig.clock.advance(1);
    driven.pass();
    let moved = StepReport::FellBack {
        issue: ticket(7),
        from: "coder".to_owned().try_into().unwrap(),
        to: "sonnet-high".to_owned().try_into().unwrap(),
        waited: 15 * 60,
    };
    assert_eq!(driven.landed(), Some(moved));
    let now = item(&rig, &runner, 7);
    assert_eq!(
        (&now["agent"], &now["turn"]["state"]),
        (&json!("sonnet-high"), &json!("due"))
    );
    assert_ne!(
        now["session"], first,
        "its next turn starts a session of its own"
    );
    assert_eq!(
        rig.claude.calls(),
        [],
        "the first turn never reached its model"
    );

    rig.claude.script([Scripted::Say("done")]);
    driven.started();
    driven.landed();
    let call = &rig.claude.calls()[0];
    assert_eq!(call.model, "claude-sonnet-5-5");
    assert_eq!(
        call.session,
        Session::New(serde_json::from_value(now["session"].clone()).unwrap())
    );
    queue.release();
}

#[test]
fn a_pinned_item_never_falls_back() {
    let rig = rig("acme", &["coder", "sonnet-high"], Some(15));
    rig.forge.label(7, "agent:coder!");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let queue = queued(&rig, Scripted::Say("done"));
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None);
    rig.clock.advance(20 * 60);
    driven.pass();
    let now = item(&rig, &runner, 7);
    assert_eq!(
        (&now["agent"], &now["pinned"]),
        (&json!("coder"), &json!(true))
    );
    assert_eq!(now["waiting"]["why"], "lease");
    let wake = runner.lock().unwrap().next_ceiling();
    assert_eq!(
        wake,
        Some(Duration::from_secs(CEILING - 20 * 60)),
        "no fallback wake"
    );
    rig.clock.advance(CEILING);
    driven.pass();
    assert!(matches!(
       driven.landed(),
       Some(StepReport::TimedOut { issue, .. }) if issue == ticket(7)
    ));
    queue.release();
}

#[test]
fn an_unlabelled_issues_first_turn_waiting_past_fallback_after_moves_to_the_next() {
    let rig = rig("acme", &["coder", "sonnet-high"], Some(15));
    rig.forge.list_ready(7, false);
    let runner = rig.open().unwrap();
    let report = step(&runner).unwrap();
    assert!(
        matches!(&report, Some(StepReport::Dispatched { issue, agent, .. })
            if *issue == ticket(7) && agent.as_str() == "coder"),
        "an unlabelled issue opens on the first listed: {report:?}"
    );
    let first = item(&rig, &runner, 7)["session"].clone();
    let queue = queued(&rig, Scripted::Say("done"));
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None);
    rig.clock.advance(15 * 60);
    driven.pass();
    let moved = StepReport::FellBack {
        issue: ticket(7),
        from: "coder".to_owned().try_into().unwrap(),
        to: "sonnet-high".to_owned().try_into().unwrap(),
        waited: 15 * 60,
    };
    assert_eq!(driven.landed(), Some(moved));
    let now = item(&rig, &runner, 7);
    assert_eq!(
        (&now["agent"], &now["turn"]["state"]),
        (&json!("sonnet-high"), &json!("due"))
    );
    assert_ne!(now["session"], first, "its next turn starts a new session");
    queue.release();
}

#[test]
fn an_item_on_an_unlisted_implementer_never_falls_back() {
    let rig = rig("acme", &["coder", "sonnet-high"], Some(15));
    let runner = rig.open().unwrap();
    add_on(&rig, &runner, 7, "coder2");
    let queue = queued(&rig, Scripted::Say("done"));
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None);
    rig.clock.advance(20 * 60);
    driven.pass();
    let now = item(&rig, &runner, 7);
    assert_eq!(
        now["agent"], "coder2",
        "the label was the choice, so it stays"
    );
    assert!(now.get("pinned").is_none(), "it is no `!` label");
    assert_eq!(now["waiting"]["why"], "lease");
    assert_eq!(
        runner.lock().unwrap().next_ceiling(),
        Some(Duration::from_secs(CEILING - 20 * 60)),
        "no fallback wake"
    );
    assert_eq!(runner.lock().unwrap().take_notes(), Vec::<String>::new());
    rig.clock.advance(CEILING);
    driven.pass();
    assert!(matches!(
       driven.landed(),
       Some(StepReport::TimedOut { issue, .. }) if issue == ticket(7)
    ));
    assert_eq!(item(&rig, &runner, 7)["agent"], "coder2");
    queue.release();
}

#[test]
fn a_first_turn_with_output_sets_no_fallback_wake() {
    let rig = rig("acme", &["sonnet-high", "opus-high"], Some(15));
    let runner = rig.open().unwrap();
    add_on(&rig, &runner, 7, "sonnet-high");
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    rig.claude.set_activity(CallActivity::Nothing);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(hold.entered(CALL_PATIENCE), "the turn never began");
    let wake = runner.lock().unwrap().next_ceiling();
    assert_eq!(
        wake,
        Some(Duration::from_secs(15 * 60)),
        "its fallback is due first"
    );

    let now = Rig::EPOCH + 60;
    rig.claude.set_activity(CallActivity::At(Timestamp(now)));
    rig.clock.advance(20 * 60);
    driven.pass();
    assert_eq!(item(&rig, &runner, 7)["agent"], "sonnet-high");
    let wake = runner.lock().unwrap().next_ceiling();
    assert_eq!(
        wake,
        Some(Duration::from_secs(CEILING - 20 * 60)),
        "only its ceiling"
    );
    hold.release();
    assert!(driven.landed().is_some());
}

#[test]
fn a_later_turn_never_falls_back() {
    let rig = rig("acme", &["coder", "sonnet-high"], Some(15));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say("done")]);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(matches!(
       driven.landed(),
       Some(StepReport::Ended { issue, .. }) if issue == ticket(7)
    ));
    let queue = queued(&rig, Scripted::Say("done"));
    driven.started();
    assert_eq!(driven.heard(), None);
    rig.clock.advance(20 * 60);
    driven.pass();
    let now = item(&rig, &runner, 7);
    assert_eq!(
        (&now["agent"], &now["turn"]["state"]),
        (&json!("coder"), &json!("running"))
    );
    assert_eq!(now["waiting"]["why"], "lease");
    rig.clock.advance(CEILING);
    driven.pass();
    assert!(matches!(
       driven.landed(),
       Some(StepReport::TimedOut { issue, .. }) if issue == ticket(7)
    ));
    queue.release();
}

// Two first turns on `agents`, both queued for their lease past
// `fallback_after`, and the moves the one pass that follows makes
fn both_waiting(agents: [&str; 2]) -> (Rig, Mutex<Runner>, BTreeSet<(TicketId, String, String)>) {
    let rig = rig("acme", &["coder", "coder2", "sonnet-high"], Some(15));
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    let runner = rig.open().unwrap();
    add_on(&rig, &runner, 7, agents[0]);
    add_on(&rig, &runner, 8, agents[1]);
    let queues = [
        queued(&rig, Scripted::Say("done")),
        queued(&rig, Scripted::Say("done")),
    ];
    let moves = {
        let driven = Driven::new(&runner);
        // A pass hears the news that came before it, so each start is heard first.
        driven.started();
        assert_eq!(driven.heard(), None);
        driven.started();
        assert_eq!(driven.heard(), None);
        rig.clock.advance(15 * 60);
        driven.pass();
        let fell = |report| match report {
            Some(StepReport::FellBack {
                issue, from, to, ..
            }) => (issue, from.to_string(), to.to_string()),
            other => panic!("no fallback: {other:?}"),
        };
        let mut moves = BTreeSet::from([fell(driven.landed())]);
        if agents[0] == agents[1] {
            moves.insert(fell(driven.landed()));
        }
        moves
    };
    for queue in queues {
        queue.release();
    }
    (rig, runner, moves)
}

fn moved(number: u64, from: &str, to: &str) -> (TicketId, String, String) {
    (ticket(number), from.to_owned(), to.to_owned())
}

#[test]
fn fallback_passes_over_an_implementer_another_item_waits_on_or_moves_to() {
    let (_, runner, moves) = both_waiting(["coder", "coder2"]);
    assert_eq!(moves, BTreeSet::from([moved(7, "coder", "sonnet-high")]));
    let notes = runner.lock().unwrap().take_notes();
    let waits = "#8: its first turn on coder2 waits for its model, and no implementer \
                 listed after coder2 is free to take it, so it waits on";
    assert!(notes.iter().any(|n| n == waits), "{notes:?}");
}

#[test]
fn two_first_turns_waiting_on_one_agent_move_to_two_others() {
    let (_, _, moves) = both_waiting(["coder", "coder"]);
    let two = [
        moved(7, "coder", "coder2"),
        moved(8, "coder", "sonnet-high"),
    ];
    assert_eq!(moves, BTreeSet::from(two));
}

#[test]
fn a_first_turn_with_no_implementer_after_its_own_waits_on() {
    let rig = rig("acme", &["sonnet-high", "coder"], Some(15));
    let runner = rig.open().unwrap();
    add_on(&rig, &runner, 7, "coder");
    let queue = queued(&rig, Scripted::Say("done"));
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None);
    rig.clock.advance(15 * 60);
    driven.pass();
    let notes = runner.lock().unwrap().take_notes();
    let waits = "#7: its first turn on coder waits for its model, and no implementer listed \
                 after coder is free to take it, so it waits on";
    assert!(notes.iter().any(|n| n == waits), "{notes:?}");
    assert_eq!(item(&rig, &runner, 7)["waiting"]["why"], "lease");
    rig.clock.advance(CEILING);
    driven.pass();
    assert!(matches!(
       driven.landed(),
       Some(StepReport::TimedOut { issue, .. }) if issue == ticket(7)
    ));
    queue.release();
}

#[test]
fn a_first_turn_stopped_at_its_ceiling_times_out_rather_than_falling_back() {
    let rig = rig("acme", &["coder", "sonnet-high"], Some(60));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let queue = queued(&rig, Scripted::Say("done"));
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None);
    rig.clock.advance(CEILING);
    driven.pass();
    assert!(matches!(
       driven.landed(),
       Some(StepReport::TimedOut { issue, .. }) if issue == ticket(7)
    ));
    assert_eq!(item(&rig, &runner, 7)["agent"], "coder");
    queue.release();
}

#[test]
fn a_first_turn_whose_agent_is_no_longer_listed_waits_on() {
    let rig = rig("acme", &["coder", "sonnet-high"], Some(15));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let queue = queued(&rig, Scripted::Say("done"));
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None);
    rig.implementers(&["sonnet-high", "coder2"]);
    let settings = rig.settings();
    (runner.lock().unwrap())
        .reread(settings, rig.kelpie_settings())
        .unwrap();
    rig.clock.advance(15 * 60);
    driven.pass();
    assert_eq!(item(&rig, &runner, 7)["agent"], "coder");
    assert_eq!(
        runner.lock().unwrap().take_notes(),
        Vec::<String>::new(),
        "an agent the project no longer lists is no fallback target"
    );
    rig.clock.advance(CEILING);
    driven.pass();
    assert!(matches!(
       driven.landed(),
       Some(StepReport::TimedOut { issue, .. }) if issue == ticket(7)
    ));
    queue.release();
}

#[test]
fn a_later_turn_restarted_on_a_new_session_never_falls_back() {
    let rig = rig("acme", &["coder", "sonnet-high"], Some(15));
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
    rig.clock.advance(20 * 60);
    driven.pass();
    let now = item(&rig, &runner, 7);
    assert_eq!(now["agent"], "coder");
    assert_eq!(now["waiting"]["why"], "silent");
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
