//! A worker turn the API refuses, driven a step at a time through the stand-ins

use std::sync::Mutex;

use serde_json::Value;

use crate::api_refusal::Refusal;
use crate::ports::AgentError;
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::{Rig, Scripted, ticket};

// The wording is Anthropic's documentation's, not a recording.
const NO_CREDIT: &str = "Your credit balance is too low to access the Anthropic API. Please go \
                         to Plans & Billing to upgrade or purchase credits.";
const SPEND_LIMIT: &str = "You have reached your specified API usage limits. You will regain \
                           access on 2026-09-01 at 00:00 UTC.";

const CLAUDE_API: &str = "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
                          effort: medium\nlogin: api\n---\n";
const PI_API: &str = "---\nrole: implementer\nharness: pi\nmodel: claude-sonnet-4-5\neffort: low\n\
                      login: api\ncontext: 200000\n---\n";
const CODER: &str = "---\nrole: implementer\nharness: stand-in\nmodel: qwen3-coder\n\
                     effort: low\nusage: none\n---\n";

// Issue 7 open on `api`, whose file is `api`, ranked ahead of `coder` and the
// plan agent `sonnet-high` unless `others` leaves them out, labelled `labels`.
fn on_api(api: &str, others: &[&str], labels: &[&str]) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("acme");
    rig.write_agent("api", api);
    rig.write_agent("coder", CODER);
    let ranked = [&["api"], others].concat();
    rig.agent_board.ranks(&ranked);
    for label in labels {
        rig.forge.label(7, label);
    }
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    (rig, runner)
}

fn refused_with(harness: Harness, text: &str) -> Scripted {
    Scripted::Fail(AgentError::Failed(harness, text.into()))
}

fn item(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    rig.ask(runner, "status", None)["work_item"].clone()
}

fn rulings(rig: &Rig, runner: &Mutex<Runner>) -> usize {
    rig.ask(runner, "status", None)["rulings"]
        .as_array()
        .map_or(0, Vec::len)
}

// The turn parked as a failed one, with the API's message in the ruling.
fn parked_on(rig: &Rig, runner: &Mutex<Runner>, agent: &str, said: &str) {
    let item = item(rig, runner);
    assert_eq!(item["turn"]["state"], "failed", "{item}");
    assert_eq!(item["agent"], agent, "{item}");
    assert_eq!(rulings(rig, runner), 1);
    let ruling = rig.ask(runner, "status", None)["rulings"][0].to_string();
    assert!(ruling.contains(said), "{ruling}");
}

#[test]
fn a_refused_turn_hands_the_item_to_the_next_pick_with_no_ruling() {
    let (rig, runner) = on_api(CLAUDE_API, &["coder"], &[]);
    rig.claude
        .script([refused_with(Harness::ClaudeCode, NO_CREDIT)]);

    let report = step(&runner).unwrap();

    assert!(
        matches!(&report, Some(StepReport::HandedOff { issue, from, to, .. })
            if *issue == ticket(7) && from.as_str() == "api" && to.as_str() == "coder"),
        "{report:?}"
    );
    assert_eq!(item(&rig, &runner)["agent"], "coder");
    assert_eq!(rulings(&rig, &runner), 0);

    rig.claude.script([Scripted::Say("done")]);
    step(&runner).unwrap();
    let call = rig.claude.calls().pop().expect("the next pick's call");
    assert_eq!(call.model, "qwen3-coder");
    assert!(matches!(call.session, crate::ports::Session::New(_)));
    assert!(
        call.prompt.contains("the API refused the key"),
        "{}",
        call.prompt
    );
    assert!(
        call.prompt.contains("credit balance is too low"),
        "{}",
        call.prompt
    );
}

#[test]
fn a_refused_turn_tells_the_board_what_the_api_said() {
    let (rig, runner) = on_api(CLAUDE_API, &["coder"], &[]);
    rig.claude
        .script([refused_with(Harness::ClaudeCode, NO_CREDIT)]);
    step(&runner).unwrap();
    assert_eq!(rig.agent_board.refusals(), [Refusal::NoCredit]);

    let (rig, runner) = on_api(CLAUDE_API, &["coder"], &[]);
    rig.claude
        .script([refused_with(Harness::ClaudeCode, SPEND_LIMIT)]);
    step(&runner).unwrap();
    let told = rig.agent_board.refusals();
    assert!(
        matches!(told.as_slice(), [Refusal::SpendLimit { resumes: Some(_) }]),
        "{told:?}"
    );
}

#[test]
fn a_refused_turn_with_no_other_pick_parks_as_a_failed_turn() {
    // Only the plan agent the board never ranks is left beside the API agent.
    let (rig, runner) = on_api(CLAUDE_API, &[], &[]);
    rig.claude
        .script([refused_with(Harness::ClaudeCode, NO_CREDIT)]);

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Failed { .. })),
        "{report:?}"
    );
    parked_on(&rig, &runner, "api", "credit balance is too low");
    assert_eq!(rig.agent_board.refusals(), [Refusal::NoCredit]);
}

#[test]
fn a_pinned_item_parks_as_a_failed_turn() {
    let (rig, runner) = on_api(CLAUDE_API, &["coder"], &["agent:api!"]);
    rig.claude
        .script([refused_with(Harness::ClaudeCode, SPEND_LIMIT)]);

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Failed { .. })),
        "{report:?}"
    );
    parked_on(&rig, &runner, "api", "specified API usage limits");
}

#[test]
fn a_refusal_on_a_pi_api_agent_holds_as_one_on_claude_code_does() {
    let (rig, runner) = on_api(PI_API, &["coder"], &[]);
    rig.claude.script([refused_with(Harness::Pi, NO_CREDIT)]);

    let report = step(&runner).unwrap();

    assert!(
        matches!(&report, Some(StepReport::HandedOff { to, .. }) if to.as_str() == "coder"),
        "{report:?}"
    );
    assert_eq!(rig.agent_board.refusals(), [Refusal::NoCredit]);
    assert_eq!(rulings(&rig, &runner), 0);
}

#[test]
fn a_failure_that_is_no_refusal_parks_as_it_did() {
    let (rig, runner) = on_api(CLAUDE_API, &["coder"], &[]);
    rig.claude
        .script([refused_with(Harness::ClaudeCode, "overloaded")]);

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Failed { .. })),
        "{report:?}"
    );
    parked_on(&rig, &runner, "api", "overloaded");
    assert_eq!(rig.agent_board.refusals(), []);
}

#[test]
fn a_plan_agents_failure_that_reads_as_a_refusal_tells_the_board_nothing() {
    let rig = Rig::new("acme");
    rig.write_agent("coder", CODER);
    rig.agent_board.ranks(&["sonnet-high", "coder"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude
        .script([refused_with(Harness::ClaudeCode, NO_CREDIT)]);

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Failed { .. })),
        "{report:?}"
    );
    parked_on(&rig, &runner, "sonnet-high", "credit balance is too low");
    assert_eq!(rig.agent_board.refusals(), []);
}

#[test]
fn a_refused_turn_after_a_handoff_adds_the_refusal_to_the_note_it_carried() {
    let rig = Rig::new("acme");
    rig.write_agent("api", CLAUDE_API);
    rig.write_agent("coder", CODER);
    rig.agent_board.ranks(&["sonnet-high", "api", "coder"]);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Commit("work.txt", "work\n")]);
    step(&runner).unwrap();
    // The plan agent's next turn goes to the API agent, which then refuses.
    let far = 3 * 60 * 60;
    rig.agent_board.waits("sonnet-high", far);
    rig.agent_board
        .ranks_finishing(&[("api", 600), ("sonnet-high", far), ("coder", far)]);
    rig.claude
        .script([refused_with(Harness::ClaudeCode, NO_CREDIT)]);
    let moved = |report: Option<StepReport>, to: &str| {
        assert!(
            matches!(&report, Some(StepReport::HandedOff { to: got, .. }) if got.as_str() == to),
            "{report:?}"
        );
    };
    moved(step(&runner).unwrap(), "api");
    moved(step(&runner).unwrap(), "coder");

    rig.claude.script([Scripted::Say("done")]);
    step(&runner).unwrap();
    let prompt = rig
        .claude
        .calls()
        .pop()
        .expect("the next pick's call")
        .prompt;
    assert!(prompt.contains("from sonnet-high"), "{prompt}");
    assert!(prompt.contains("the API refused the key"), "{prompt}");
    assert!(prompt.contains("credit balance is too low"), "{prompt}");
}

#[test]
fn a_read_the_pacer_asks_for_names_the_agent_it_is_for_not_one_it_held_ahead() {
    let rig = Rig::new("acme");
    rig.write_agent("api", CLAUDE_API);
    rig.write_codex_implementer();
    rig.agent_board.ranks(&["api", "codex", "sonnet-high"]);
    // The Codex account is past its window when the item opens, the Claude one unread.
    rig.codex_meter.set(Rig::utilization(0, 55));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude
        .script([refused_with(Harness::ClaudeCode, NO_CREDIT)]);

    let report = step(&runner).unwrap();

    assert!(
        matches!(&report, Some(StepReport::HandedOff { to, .. }) if to.as_str() == "sonnet-high"),
        "{report:?}"
    );
}
