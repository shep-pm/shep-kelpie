//! A due turn whose own implementer finishes past the slack, handed to the
//! agent board's next pick, driven a step at a time through the stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use crate::ports::{AgentError, Session};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::{Rig, Scripted, ticket};

const CODER: &str = "---\nrole: implementer\nharness: stand-in\nmodel: qwen3-coder\n\
                     effort: low\nusage: none\n---\n";

/// Seconds to the soonest finish the board gives
const SOON: u64 = 600;

/// The slack an item with none of its own allows, in seconds
const SLACK: u64 = 60 * 60;

// Issue 7 open on `sonnet-high` beside `coder`, labelled `labels`, whose first
// turn committed `work.txt` and stopped with no pull request: the next step
// sends it back, a later turn
fn later_turn_due(labels: &[&str]) -> (Rig, Mutex<Runner>) {
    later_turn_in(Rig::new("acme"), labels)
}

// ... in a project whose settings hold `minutes` of slack
fn later_turn_slacking(minutes: u32) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("acme");
    let slack = format!("\nslack = {minutes}\nimplementers = ");
    rig.edit_settings(|s| s.replace("\nimplementers = ", &slack));
    later_turn_in(rig, &[])
}

// ... on `rig`
fn later_turn_in(rig: Rig, labels: &[&str]) -> (Rig, Mutex<Runner>) {
    rig.write_agent("coder", CODER);
    rig.write_codex_implementer();
    rig.implementers(&["sonnet-high", "coder"]);
    for label in labels {
        rig.forge.label(7, label);
    }
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Commit("work.txt", "work\n")]);
    step(&runner).unwrap();
    assert_eq!(item(&rig, &runner)["turn"]["state"], "ended");
    (rig, runner)
}

fn item(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    rig.ask(runner, "status", None)["work_item"].clone()
}

// Runs the due turn, and the model and session it ran on
fn next_turn(rig: &Rig, runner: &Mutex<Runner>) -> (String, Session) {
    rig.claude.script([Scripted::Say("done")]);
    step(runner).unwrap();
    let call = rig.claude.calls().pop().expect("a worker call");
    (call.model, call.session)
}

fn resumed(session: &Session) -> bool {
    matches!(session, Session::Resume(_))
}

fn handed_off(from_finish: u64) -> StepReport {
    StepReport::HandedOff {
        issue: ticket(7),
        from: "sonnet-high".to_owned().try_into().unwrap(),
        to: "coder".to_owned().try_into().unwrap(),
        from_finish: Some(from_finish),
        to_finish: SOON,
    }
}

#[test]
fn a_later_turn_whose_agent_finishes_within_the_slack_stays() {
    let (rig, runner) = later_turn_due(&[]);
    let own = SOON + SLACK;
    rig.agent_board
        .ranks_finishing(&[("coder", SOON), ("sonnet-high", own)]);
    let (model, session) = next_turn(&rig, &runner);
    assert_eq!(model, "claude-sonnet-5-5");
    assert!(resumed(&session), "{session:?}");
    assert_eq!(item(&rig, &runner)["agent"], "sonnet-high");
}

#[test]
fn a_later_turn_past_the_slack_hands_off_on_a_fresh_session_with_a_note() {
    let (rig, runner) = later_turn_due(&[]);
    let before = item(&rig, &runner)["session"].clone();
    let own = SOON + SLACK + 1;
    rig.agent_board
        .ranks_finishing(&[("coder", SOON), ("sonnet-high", own)]);

    let report = step(&runner).unwrap().expect("a report");
    assert_eq!(report, handed_off(own));
    let line = serde_json::to_value(&report).unwrap();
    assert_eq!(
        (&line["step"], &line["from_finish"], &line["to_finish"]),
        (&json!("handed-off"), &json!(own), &json!(SOON))
    );
    let now = item(&rig, &runner);
    assert_eq!(
        (&now["agent"], &now["turn"]["state"]),
        (&json!("coder"), &json!("due"))
    );
    assert_ne!(now["session"], before, "a session of its own");
    assert_eq!(rig.claude.calls().len(), 1, "the handoff calls no model");

    rig.agent_board
        .ranks_finishing(&[("coder", SOON), ("sonnet-high", own)]);
    let (model, session) = next_turn(&rig, &runner);
    assert_eq!(model, "qwen3-coder");
    let fresh = serde_json::from_value(now["session"].clone()).unwrap();
    assert_eq!(session, Session::New(fresh));
    let prompt = rig.claude.calls().pop().unwrap().prompt;
    for told in ["Body of #7.", "from sonnet-high", "work.txt", "committed"] {
        assert!(prompt.contains(told), "{told:?} is not in {prompt}");
    }
    let state = std::fs::read_to_string(rig.paths().state).unwrap();
    let saved: Value = serde_json::from_str(&state).unwrap();
    let events = saved["events"].as_array().unwrap();
    let event = "#7: handed off from sonnet-high to coder";
    assert!(events.iter().any(|e| e["what"] == event), "{events:?}");
}

#[test]
fn the_settings_slack_decides_whether_a_later_turn_stays() {
    let (rig, runner) = later_turn_slacking(10);
    let own = SOON + 30 * 60;
    rig.agent_board
        .ranks_finishing(&[("coder", SOON), ("sonnet-high", own)]);
    step(&runner).unwrap();
    assert_eq!(item(&rig, &runner)["agent"], "coder");
    let ask = rig.agent_board.asks().pop().expect("an ask");
    assert_eq!(ask.slack, Some(10));

    let (rig, runner) = later_turn_slacking(120);
    let own = SOON + SLACK + 1;
    rig.agent_board
        .ranks_finishing(&[("coder", SOON), ("sonnet-high", own)]);
    next_turn(&rig, &runner);
    assert_eq!(item(&rig, &runner)["agent"], "sonnet-high");
    let ask = rig.agent_board.asks().pop().expect("an ask");
    assert_eq!(ask.slack, Some(120));
}

#[test]
fn an_items_own_slack_wins_over_the_settings() {
    let (rig, runner) = later_turn_slacking(120);
    runner
        .lock()
        .unwrap()
        .state
        .item_mut(ticket(7))
        .expect("the item")
        .slack = Some(0);
    rig.agent_board
        .ranks_finishing(&[("coder", SOON), ("sonnet-high", SOON + 1)]);
    step(&runner).unwrap();
    assert_eq!(item(&rig, &runner)["agent"], "coder");
    let ask = rig.agent_board.asks().pop().expect("an ask");
    assert_eq!(ask.slack, Some(0));
}

#[test]
fn the_agent_an_item_left_is_left_out_of_every_later_ask() {
    let (rig, runner) = later_turn_due(&[]);
    let own = SOON + SLACK + 1;
    rig.agent_board
        .ranks_finishing(&[("coder", SOON), ("sonnet-high", own)]);
    step(&runner).unwrap();
    next_turn(&rig, &runner);

    let ask = rig.agent_board.asks().pop().expect("an ask");
    let left = ["sonnet-high".to_owned().try_into().unwrap()];
    assert_eq!((ask.leave_out, ask.pin), (left.to_vec(), None));
}

#[test]
fn a_pinned_item_never_hands_off() {
    let (rig, runner) = later_turn_due(&["agent:sonnet-high!"]);
    rig.agent_board
        .ranks_finishing(&[("coder", SOON), ("sonnet-high", SOON + SLACK + 1)]);
    let (model, session) = next_turn(&rig, &runner);
    assert_eq!(model, "claude-sonnet-5-5");
    assert!(resumed(&session), "{session:?}");
}

#[test]
fn a_board_out_of_reach_leaves_the_turn_on_its_own_agent() {
    let (rig, runner) = later_turn_due(&[]);
    rig.agent_board.unreachable();
    let (model, session) = next_turn(&rig, &runner);
    assert_eq!(model, "claude-sonnet-5-5");
    assert!(resumed(&session), "{session:?}");
}

#[test]
fn a_board_ranking_nobody_leaves_the_turn_on_its_own_agent() {
    let (rig, runner) = later_turn_due(&[]);
    rig.agent_board.ranks(&[]);
    let (model, session) = next_turn(&rig, &runner);
    assert_eq!(model, "claude-sonnet-5-5");
    assert!(resumed(&session), "{session:?}");
}

#[test]
fn a_ranking_the_pacer_holds_leaves_the_turn_on_its_own_agent() {
    let (rig, runner) = later_turn_due(&[]);
    rig.agent_board
        .ranks_finishing(&[("codex", SOON), ("sonnet-high", SOON + SLACK + 1)]);
    // Codex's 5-hour window is past what the pacer lets a turn start on.
    rig.codex_meter.set(Rig::utilization(0, 55));
    let (model, session) = next_turn(&rig, &runner);
    assert_eq!(model, "claude-sonnet-5-5");
    assert!(resumed(&session), "{session:?}");
    assert_eq!(item(&rig, &runner)["agent"], "sonnet-high");
}

// The prompt of the last worker call
fn last_prompt(rig: &Rig) -> String {
    rig.claude.calls().pop().expect("a worker call").prompt
}

// Hands issue 7 off from `sonnet-high` to `coder`, as the board ranks them
fn handed_to_coder() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = later_turn_due(&[]);
    let own = SOON + SLACK + 1;
    rig.agent_board
        .ranks_finishing(&[("coder", SOON), ("sonnet-high", own)]);
    assert_eq!(step(&runner).unwrap(), Some(handed_off(own)));
    (rig, runner)
}

#[test]
fn a_session_that_dies_unborn_after_a_handoff_starts_over_with_the_note() {
    let (rig, runner) = handed_to_coder();
    let session = serde_json::from_value(item(&rig, &runner)["session"].clone()).unwrap();
    let gone = AgentError::NoSession(Harness::StandIn, session);
    rig.claude
        .script([Scripted::Fail(gone), Scripted::Say("done")]);
    for _ in 0..3 {
        if rig.claude.calls().len() == 3 {
            break;
        }
        step(&runner).unwrap();
    }
    let calls = rig.claude.calls();
    assert_eq!(calls.len(), 3, "the turn started over");
    assert!(matches!(calls[2].session, Session::New(_)), "{calls:#?}");
    assert!(
        calls[2].prompt.contains("from sonnet-high"),
        "{}",
        calls[2].prompt
    );
    assert_eq!(item(&rig, &runner)["agent"], "coder");
}

#[test]
fn a_second_handoff_before_the_new_agent_ran_keeps_the_first_note() {
    let (rig, runner) = handed_to_coder();
    let far = SOON + SLACK + 1;
    rig.agent_board
        .ranks_finishing(&[("codex", SOON), ("coder", far), ("sonnet-high", far)]);
    let report = step(&runner).unwrap();
    assert!(
        matches!(&report, Some(StepReport::HandedOff { from, to, .. })
            if from.as_str() == "coder" && to.as_str() == "codex"),
        "{report:?}"
    );
    let (model, _) = next_turn(&rig, &runner);
    assert_eq!(model, "gpt-5-codex");
    let prompt = last_prompt(&rig);
    assert!(prompt.contains("from sonnet-high"), "{prompt}");
    assert!(!prompt.contains("coder's last turn"), "{prompt}");
}

#[test]
fn an_own_agent_the_board_leaves_unranked_hands_off_with_no_finish() {
    let (rig, runner) = later_turn_due(&[]);
    rig.agent_board.ranks_finishing(&[("coder", SOON)]);
    let report = step(&runner).unwrap();
    let Some(StepReport::HandedOff { from_finish, .. }) = report else {
        panic!("no handoff: {report:?}");
    };
    assert_eq!(from_finish, None);
    assert_eq!(item(&rig, &runner)["agent"], "coder");
}

#[test]
fn every_ranked_agent_held_keeps_the_turn_and_the_held_agent_asks_no_more() {
    let (rig, runner) = later_turn_due(&[]);
    rig.agent_board
        .ranks_finishing(&[("codex", SOON), ("sonnet-high", SOON + SLACK + 1)]);
    rig.codex_meter.set(Rig::utilization(0, 55));
    rig.meter.set(Rig::utilization(0, 55));
    step(&runner).unwrap();
    assert_eq!(rig.claude.calls().len(), 1, "no turn starts");
    assert_eq!(item(&rig, &runner)["agent"], "sonnet-high");

    let asked = rig.agent_board.asks().len();
    step(&runner).unwrap();
    assert_eq!(rig.agent_board.asks().len(), asked, "the hold answers");
    assert_eq!(item(&rig, &runner)["agent"], "sonnet-high");
}

#[test]
fn a_pick_whose_usage_is_unread_is_read_once_then_takes_the_item() {
    let (rig, runner) = later_turn_due(&[]);
    rig.agent_board
        .ranks_finishing(&[("codex", SOON), ("sonnet-high", SOON + SLACK + 1)]);
    rig.codex_meter.set(Rig::utilization(0, 0));
    let report = step(&runner).unwrap();
    assert!(
        matches!(&report, Some(StepReport::HandedOff { to, .. }) if to.as_str() == "codex"),
        "{report:?}"
    );
    assert_eq!(rig.codex_meter.reads(), 1);
}

#[test]
fn a_first_turn_handed_off_before_its_worktree_exists_names_no_commits() {
    let rig = Rig::new("acme");
    rig.write_agent("coder", CODER);
    rig.implementers(&["sonnet-high", "coder"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    let own = SOON + SLACK + 1;
    rig.agent_board
        .ranks_finishing(&[("coder", SOON), ("sonnet-high", own)]);
    assert_eq!(step(&runner).unwrap(), Some(handed_off(own)));
    next_turn(&rig, &runner);
    let prompt = last_prompt(&rig);
    assert!(prompt.contains("from sonnet-high"), "{prompt}");
    assert!(!prompt.contains("commits"), "{prompt}");
}

#[test]
fn a_long_branch_lists_its_newest_commits_and_counts_the_rest() {
    let commits: Vec<String> = (0..23).map(|n| format!("c{n}")).collect();
    let listed = super::listed(&commits);
    assert_eq!(listed.lines().count(), 21);
    assert!(listed.starts_with("c0\nc1\n"), "{listed}");
    assert!(listed.ends_with("c19\nand 3 more"), "{listed}");
}
