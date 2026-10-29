// The agent board picks the implementer a new work item opens on. An unpinned
// `agent:` label does not choose; a pinned one is the only agent tried.

use serde_json::json;
use std::sync::Mutex;

use super::*;

// An implementer on a local model, and a second on another local model
const CODER: &str = "---\nrole: implementer\nharness: stand-in\nmodel: qwen3-coder\n\
                      effort: low\nusage: none\n---\n";
const CODER2: &str = "---\nrole: implementer\nharness: stand-in\nmodel: qwen3-coder2\n\
                       effort: low\nusage: none\n---\n";

// A project whose board ranks `ranked`, each on a local model of its own
fn in_order(project: &str, ranked: &[&str]) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.write_agent("coder", CODER);
    rig.write_agent("coder2", CODER2);
    rig.agent_board.ranks(ranked);
    let runner = rig.open().unwrap();
    (rig, runner)
}

// Whether `report` dispatched issue `issue` on agent `agent`
fn dispatched_on(report: Option<StepReport>, issue: u64, agent: &str) -> bool {
    matches!(report, Some(StepReport::Dispatched { issue: got, agent: picked, .. })
        if got == ticket(issue) && picked.as_str() == agent)
}

fn agent(name: &str) -> AgentName {
    AgentName::try_from(name.to_owned()).unwrap()
}

#[test]
fn the_boards_first_pick_opens_the_item_with_no_call_and_no_label() {
    let (rig, runner) = in_order("acme", &["coder", "coder2"]);
    rig.agent_board.ranks(&["coder2", "coder"]);
    rig.forge.list_ready(6, false);

    assert!(dispatched_on(step(&runner).unwrap(), 6, "coder2"));

    assert_eq!(rig.claude.calls(), [], "no call before the turn");
    assert!(
        !(rig.forge.issue_labels(6))
            .iter()
            .any(|l| l.starts_with("agent:")),
        "no label is written onto the issue: {:?}",
        rig.forge.issue_labels(6)
    );
    rig.claude.script([Scripted::Say("done")]);
    step(&runner).unwrap();
    let call = &rig.claude.calls()[0];
    assert_eq!(
        (call.role, call.model.as_str()),
        (Role::Worker, "qwen3-coder2")
    );
}

#[test]
fn the_ask_carries_every_allowed_implementer_by_name_which_the_board_ranks_unless_told() {
    let (rig, runner) = in_order("acme", &[]);
    rig.agent_board.ranks_listed();
    rig.forge.list_ready(6, false);

    assert!(dispatched_on(step(&runner).unwrap(), 6, "coder"));

    let asks = rig.agent_board.asks();
    let agents: Vec<&str> = asks[0].agents.iter().map(AgentName::as_str).collect();
    assert_eq!(agents, ["coder", "coder2", "opus-high", "sonnet-high"]);
    assert_eq!(
        (asks[0].pin.clone(), asks[0].kind.as_deref()),
        (None, Some("fix"))
    );
}

// The ask a new item for issue 6 sends the board, in a project whose settings
// hold `slack` minutes, or the example's when None
fn slack_asked(slack: Option<u32>) -> Option<u32> {
    let rig = Rig::new("acme");
    if let Some(minutes) = slack {
        rig.edit_settings(|s| {
            s.replace(
                "[app.dogs.kelpie.agents]\n",
                &format!("[app.dogs.kelpie.agents]\nslack = {minutes}\n"),
            )
        });
    }
    let runner = rig.open().unwrap();
    rig.forge.list_ready(6, false);
    assert!(dispatched_on(step(&runner).unwrap(), 6, "sonnet-high"));
    rig.agent_board.asks()[0].slack
}

#[test]
fn the_ask_carries_an_hour_of_slack_when_the_settings_name_none() {
    assert_eq!(slack_asked(None), Some(60));
}

#[test]
fn the_ask_carries_the_settings_slack() {
    assert_eq!(slack_asked(Some(25)), Some(25));
    assert_eq!(slack_asked(Some(0)), Some(0));
}

#[test]
fn an_unpinned_agent_label_does_not_choose() {
    let (rig, runner) = in_order("acme", &["coder", "coder2"]);
    rig.forge.list_ready(6, false);
    rig.forge.label(6, "agent:coder2");

    assert!(dispatched_on(step(&runner).unwrap(), 6, "coder"));

    assert_eq!(rig.agent_board.asks()[0].pin, None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["agent"], "coder");
}

#[test]
fn a_pinned_agent_label_pins_the_item_and_the_ask_carries_the_pin_and_kind() {
    let (rig, runner) = in_order("acme", &["coder", "coder2"]);
    rig.forge.list_ready(6, false);
    rig.forge.label(6, "agent:coder2!");
    rig.forge.label(6, "kind:docs");

    assert!(dispatched_on(step(&runner).unwrap(), 6, "coder2"));

    let asks = rig.agent_board.asks();
    assert_eq!(
        (asks[0].pin.clone(), asks[0].kind.as_deref()),
        (Some(agent("coder2")), Some("docs"))
    );
    let held = runner.lock().unwrap();
    assert!(held.state.item(ticket(6)).unwrap().pinned);
}

#[test]
fn an_unreachable_board_starts_nothing_says_why_once_and_the_next_poll_that_reaches_it_starts() {
    let (rig, runner) = in_order("acme", &["coder", "coder2"]);
    rig.agent_board.unreachable();
    rig.forge.list_ready(6, false);
    runner.lock().unwrap().take_notes();

    assert_eq!(step(&runner).unwrap(), None);

    let why = "the agent board, which the dog keeps, cannot be reached";
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["skipped"],
        json!([{ "reason": "unpicked", "issue": "6", "why": why }])
    );
    let notes = runner.lock().unwrap().take_notes();
    assert_eq!(notes, [format!("#6 skipped reason={why}")]);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(runner.lock().unwrap().take_notes(), Vec::<String>::new());

    rig.agent_board.ranks_listed();
    rig.next_look();
    assert!(dispatched_on(step(&runner).unwrap(), 6, "coder"));
}

#[test]
fn an_empty_ranking_starts_nothing_and_names_the_kind_and_tier() {
    let (rig, runner) = in_order("acme", &["coder", "coder2"]);
    rig.agent_board.ranks(&[]);
    rig.forge.list_ready(6, false);
    rig.forge.label(6, "kind:docs");
    rig.forge.list_ready(7, false);
    rig.forge.label(7, "agent:coder2!");

    assert_eq!(step(&runner).unwrap(), None);

    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["skipped"],
        json!([
            {
                "reason": "unpicked",
                "issue": "6",
                "why": "the agent board ranks no allowed implementer for a `docs` item at any tier"
            },
            {
                "reason": "unpicked",
                "issue": "7",
                "why": "the agent board ranks no allowed implementer for a `fix` item at any \
                        tier, pinned to coder2"
            }
        ])
    );
}

#[test]
fn add_refuses_a_label_naming_no_implementer_and_opens_on_the_boards_pick_otherwise() {
    let rig = Rig::new("xilriws");
    rig.write_agent("coder", CODER);
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    let runner = rig.open().unwrap();
    let refused = |issue: u64, label: &str| {
        rig.forge.label(issue, label);
        assert_eq!(
            rig.ask(&runner, "add", Some(&issue.to_string())),
            json!({
                "error": format!(
                    "label `{label}` names no implementer: kelpie's agent files with \
                     `role: implementer` are coder, opus-high, sonnet-high"
                )
            }),
            "{label}"
        );
    };
    refused(9, "agent:defect-hunter");
    refused(10, "agent:haiku-low!");

    let agent_of = |issue: u64| {
        let status = rig.ask(&runner, "status", None);
        let items = status["work_items"].as_array().unwrap();
        let shown = issue.to_string();
        let found = items
            .iter()
            .find(|i| i["issue"] == json!(shown))
            .expect("open");
        found["agent"].clone()
    };
    rig.forge.label(8, "agent:coder!");
    rig.ask(&runner, "add", Some("8"));
    assert_eq!(agent_of(8), json!("coder"), "the pin was the choice");

    rig.agent_board.ranks(&["opus-high"]);
    rig.forge.label(11, "agent:coder");
    rig.ask(&runner, "add", Some("11"));
    assert_eq!(agent_of(11), json!("opus-high"), "the board's pick");
}

#[test]
fn add_on_an_unreachable_board_is_refused_saying_so() {
    let (rig, runner) = in_order("acme", &["coder"]);
    rig.agent_board.unreachable();

    assert_eq!(
        rig.ask(&runner, "add", Some("12")),
        json!({ "error": "the agent board, which the dog keeps, cannot be reached" })
    );
    assert_eq!(rig.ask(&runner, "status", None)["work_items"], json!([]));
}
