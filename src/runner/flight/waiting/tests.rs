//! A turn waiting for its model, and a first turn falling back, driven a
//! pass at a time through the runner's stand-ins

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use super::super::tests::{CEILING, CODER, Driven, PATIENCE};
use crate::lease::gpu::{Attempt, Claim, GpuLock};
use crate::ports::{AgentError, CallActivity, Session, Timestamp};
use crate::runner::{Runner, StepReport};
use crate::settings::Harness;
use crate::test::{Hold, Rig, Scripted};

/// The time of day `at` is, as the board writes it
fn clock(at: u64) -> String {
    let seconds = at % 86_400;
    format!("{:02}:{:02}", seconds / 3600, seconds % 3600 / 60)
}

/// A rig listing `listed`, `coder` and `coder2` on the GPU, with
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

/// Holds the GPU lock for someone else until the claim is released
fn gpu_taken(rig: &Rig) -> (GpuLock, Claim) {
    let lock = GpuLock::under(&rig.home.path().join("tmp"));
    let round = Claim {
        pid: std::process::id(),
        what: "qwen-review round 1".into(),
    };
    assert_eq!(lock.try_take(&round).unwrap(), Attempt::Taken);
    (lock, round)
}

/// The work item for `issue` as `status` shows it
fn item(rig: &Rig, runner: &Mutex<Runner>, issue: u64) -> Value {
    let status = rig.ask(runner, "status", None);
    let items = status["work_items"].as_array().unwrap();
    let found = items.iter().find(|i| i["issue"] == issue);
    found.expect("the work item is open").clone()
}

fn board(rig: &Rig) -> String {
    std::fs::read_to_string(&rig.paths().board).unwrap()
}

#[test]
fn a_turn_queued_for_its_lease_is_marked_waiting_until_it_begins() {
    let rig = rig("acme", &["sonnet-high", "coder"], None);
    rig.forge.label(7, "agent:coder");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let (lock, round) = gpu_taken(&rig);
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
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

    lock.release(round.pid).unwrap();
    assert_eq!(driven.heard(), None, "the grant is not a report");
    assert!(hold.entered(PATIENCE), "the turn never began");
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
    assert!(hold.entered(PATIENCE), "the turn never began");
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
    let (lock, round) = gpu_taken(&rig);
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None);
    rig.clock.advance(15 * 60 - 1);
    driven.pass();
    assert_eq!(item(&rig, &runner, 7)["agent"], "coder");
    rig.clock.advance(1);
    driven.pass();
    let moved = StepReport::FellBack {
        issue: 7,
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
    lock.release(round.pid).unwrap();
}

#[test]
fn a_pinned_item_never_falls_back() {
    let rig = rig("acme", &["coder", "sonnet-high"], Some(15));
    rig.forge.label(7, "agent:coder!");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let (lock, round) = gpu_taken(&rig);
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
        Some(StepReport::TimedOut { issue: 7, .. })
    ));
    lock.release(round.pid).unwrap();
}

#[test]
fn a_first_turn_with_output_sets_no_fallback_wake() {
    let rig = rig("acme", &["sonnet-high", "opus-high"], Some(15));
    rig.forge.label(7, "agent:sonnet-high");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    rig.claude.set_activity(CallActivity::Nothing);
    let driven = Driven::new(&runner);
    driven.started();
    assert!(hold.entered(PATIENCE), "the turn never began");
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
        Some(StepReport::Ended { issue: 7, .. })
    ));
    let (lock, round) = gpu_taken(&rig);
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
        Some(StepReport::TimedOut { issue: 7, .. })
    ));
    lock.release(round.pid).unwrap();
}

// Two first turns on `labels`' agents, both queued for the GPU past
// `fallback_after`, and the moves the one pass that follows makes
fn both_waiting(labels: [&str; 2]) -> (Rig, Mutex<Runner>, BTreeSet<(u64, String, String)>) {
    let rig = rig("acme", &["coder", "coder2", "sonnet-high"], Some(15));
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    rig.forge.label(7, labels[0]);
    rig.forge.label(8, labels[1]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.ask(&runner, "add", Some("8"));
    let (lock, round) = gpu_taken(&rig);
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
        if labels[0] == labels[1] {
            moves.insert(fell(driven.landed()));
        }
        moves
    };
    lock.release(round.pid).unwrap();
    (rig, runner, moves)
}

fn moved(issue: u64, from: &str, to: &str) -> (u64, String, String) {
    (issue, from.to_owned(), to.to_owned())
}

#[test]
fn fallback_passes_over_an_implementer_another_item_waits_on_or_moves_to() {
    let (_, runner, moves) = both_waiting(["agent:coder", "agent:coder2"]);
    assert_eq!(moves, BTreeSet::from([moved(7, "coder", "sonnet-high")]));
    let notes = runner.lock().unwrap().take_notes();
    let waits = "#8: its first turn on coder2 waits for its model, and no implementer \
                 listed after coder2 is free to take it, so it waits on";
    assert!(notes.iter().any(|n| n == waits), "{notes:?}");
}

#[test]
fn two_first_turns_waiting_on_one_agent_move_to_two_others() {
    let (_, _, moves) = both_waiting(["agent:coder", "agent:coder"]);
    let two = [
        moved(7, "coder", "coder2"),
        moved(8, "coder", "sonnet-high"),
    ];
    assert_eq!(moves, BTreeSet::from(two));
}

#[test]
fn a_first_turn_with_no_implementer_after_its_own_waits_on() {
    let rig = rig("acme", &["sonnet-high", "coder"], Some(15));
    rig.forge.label(7, "agent:coder");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let (lock, round) = gpu_taken(&rig);
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
        Some(StepReport::TimedOut { issue: 7, .. })
    ));
    lock.release(round.pid).unwrap();
}

#[test]
fn a_first_turn_stopped_at_its_ceiling_times_out_rather_than_falling_back() {
    let rig = rig("acme", &["coder", "sonnet-high"], Some(60));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let (lock, round) = gpu_taken(&rig);
    let driven = Driven::new(&runner);
    driven.started();
    assert_eq!(driven.heard(), None);
    rig.clock.advance(CEILING);
    driven.pass();
    assert!(matches!(
        driven.landed(),
        Some(StepReport::TimedOut { issue: 7, .. })
    ));
    assert_eq!(item(&rig, &runner, 7)["agent"], "coder");
    lock.release(round.pid).unwrap();
}

#[test]
fn a_first_turn_whose_agent_is_no_longer_listed_waits_on() {
    let rig = rig("acme", &["coder", "sonnet-high"], Some(15));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let (lock, round) = gpu_taken(&rig);
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
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("#7: its first turn on coder waits")),
        "{notes:?}"
    );
    rig.clock.advance(CEILING);
    driven.pass();
    assert!(matches!(
        driven.landed(),
        Some(StepReport::TimedOut { issue: 7, .. })
    ));
    lock.release(round.pid).unwrap();
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
        Some(StepReport::Ended { issue: 7, .. })
    ));
    let session = serde_json::from_value(item(&rig, &runner, 7)["session"].clone()).unwrap();
    let (hold, again) = (Hold::default(), Hold::default());
    let gone = AgentError::NoSession(Harness::StandIn, session);
    let restart = Scripted::Hold(again.clone());
    rig.claude
        .script([Scripted::HoldThenFail(hold.clone(), gone), restart]);
    rig.claude.set_activity(CallActivity::Nothing);
    driven.started();
    // The lease is granted at once, and the call waits until the grant is heard.
    assert_eq!((driven.heard(), driven.heard()), (None, None));
    assert!(hold.entered(PATIENCE), "the later turn never began");
    hold.release();
    assert_eq!(driven.heard(), None, "the restart is not a report");
    assert_eq!((driven.heard(), driven.heard()), (None, None));
    assert!(again.entered(PATIENCE), "the restart never began");
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
        Some(StepReport::Ended { issue: 7, .. })
    ));
}
