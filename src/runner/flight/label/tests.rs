//! The issue writer labelling an unlabelled issue, through the runner's stand-ins

use serde_json::{Value, json};

use std::time::Duration;

use crate::ports::{AgentError, Role, Tools};
use crate::runner::{Pass, StepReport, advance, step};
use crate::settings::Harness;
use crate::test::{Hold, Rig, Scripted};

// Real threads on real time, so every wait has this ceiling.
const PATIENCE: Duration = Duration::from_secs(30);
use crate::usage::{FILE, read};

/// A file for an issue writer on Sonnet, named for the test
const SCRIBE: &str = "---\nrole: issue-writer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
                      effort: low\n---\nLabel what you are asked to.\n";

fn ledger(rig: &Rig) -> Vec<Value> {
    let lines = read(&rig.paths().folder.join(FILE)).unwrap();
    (lines.iter())
        .map(|line| serde_json::to_value(line).unwrap())
        .collect()
}

fn dispatched_on(report: Option<StepReport>) -> String {
    match report {
        Some(StepReport::Dispatched {
            issue: 7, agent, ..
        }) => agent.to_string(),
        other => panic!("issue 7 was not dispatched: {other:?}"),
    }
}

#[test]
fn an_unlabelled_issue_gets_one_issue_writer_call_and_then_the_label_it_chose() {
    let rig = Rig::new("acme");
    rig.implementers(&["sonnet-high", "opus-high"]);
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    rig.claude.script([Scripted::Say(
        "Mechanical, but it moves state.\n{\"agent\": \"opus-high\"}",
    )]);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Labelled {
            issue: 7,
            agent: "opus-high".to_owned().try_into().unwrap(),
        })
    );
    assert!(
        rig.forge
            .issue_labels(7)
            .contains(&"agent:opus-high".to_owned())
    );
    let seen = rig.claude.all_seen();
    let [label] = seen.as_slice() else {
        panic!("one issue writer call: {seen:#?}")
    };
    let call = &label.call;
    assert_eq!((call.role, call.tools), (Role::IssueWriter, Tools::Review));
    assert_eq!((call.issue, call.model.as_str()), (7, "claude-opus-5-5"));
    assert!(
        call.prompt.contains("# Title of #7\n\nBody of #7."),
        "{}",
        call.prompt
    );
    assert!(
        call.prompt
            .contains("- `sonnet-high`: claude-sonnet-5-5 at high effort, the default")
    );
    let instructions = std::fs::read_to_string(call.instructions.as_ref().unwrap()).unwrap();
    assert!(
        instructions.contains("acceptance criteria"),
        "{instructions}"
    );

    let lines = ledger(&rig);
    let [line] = lines.as_slice() else {
        panic!("one line for the call: {lines:#?}")
    };
    let wanted =
        json!({ "issue": 7, "role": "issue-writer", "kind": "issues", "agent": "issue-writer" });
    for (key, value) in wanted.as_object().unwrap() {
        assert_eq!(&line[key], value, "{key}: {line:#}");
    }

    rig.claude.script([Scripted::Say("done")]);
    assert_eq!(dispatched_on(step(&runner).unwrap()), "opus-high");
    let writers = (rig.claude.all_calls().into_iter()).filter(|c| c.role == Role::IssueWriter);
    assert_eq!(writers.count(), 1, "no second issue writer call");
}

#[test]
fn agents_issue_writer_names_the_file_that_labels() {
    let rig = Rig::new("acme");
    rig.write_agent("scribe", SCRIBE);
    rig.implementers(&["sonnet-high", "opus-high"]);
    rig.edit_settings(|s| {
        s.replace(
            "\nimplementers = ",
            "\nissue_writer = \"scribe\"\nimplementers = ",
        )
    });
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    rig.claude
        .script([Scripted::Say("{\"agent\": \"sonnet-high\"}")]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Labelled { issue: 7, .. })
    ));
    let call = &rig.claude.all_calls()[0];
    assert_eq!(call.model, "claude-sonnet-5-5");
    let instructions = std::fs::read_to_string(call.instructions.as_ref().unwrap()).unwrap();
    assert_eq!(instructions.trim(), "Label what you are asked to.");
    assert_eq!(ledger(&rig)[0]["agent"], "scribe");
}

#[test]
fn with_one_implementer_listed_an_unlabelled_issue_opens_on_it_with_no_call() {
    let rig = Rig::new("acme");
    rig.write_agent("coder", super::super::tests::CODER);
    rig.implementers(&["coder"]);
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    assert_eq!(dispatched_on(step(&runner).unwrap()), "coder");
    assert_eq!(rig.claude.calls(), [], "no issue writer call");
    rig.claude.script([Scripted::Say("done")]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ended { issue: 7, .. })
    ));
    let call = &rig.claude.calls()[0];
    assert_eq!(
        (call.role, call.model.as_str()),
        (Role::Worker, "qwen3-coder")
    );
}

#[test]
fn a_reply_naming_no_listed_implementer_raises_a_ruling_and_the_board_waits() {
    let rig = Rig::new("acme");
    rig.implementers(&["sonnet-high", "opus-high"]);
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    rig.claude.script([Scripted::Say("{\"agent\": \"haiku\"}")]);
    let Some(StepReport::Unlabelled {
        issue: 7,
        id,
        question,
    }) = step(&runner).unwrap()
    else {
        panic!("no ruling");
    };
    assert!(
        question.starts_with(
            "The issue writer could not pick an implementer for issue #7: it named `haiku`, \
             which `agents.implementers` does not list."
        ),
        "{question}"
    );
    assert!(
        !rig.forge
            .issue_labels(7)
            .iter()
            .any(|l| l.starts_with("agent:"))
    );

    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id }));
    assert_eq!(step(&runner).unwrap(), None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["skipped"],
        json!([{ "reason": "unlabelled", "issue": 7, "ruling": id }])
    );
    assert_eq!(status["rulings"][0]["id"], id);
    assert_eq!(
        rig.claude.all_calls().len(),
        1,
        "the board does not ask again"
    );

    rig.forge.label(7, "agent:sonnet-high");
    rig.next_look();
    rig.claude.script([Scripted::Say("done")]);
    assert_eq!(dispatched_on(step(&runner).unwrap()), "sonnet-high");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["rulings"],
        json!([]),
        "a work item opening clears it"
    );
}

#[test]
fn a_failed_call_raises_a_ruling_and_its_answer_lets_the_issue_writer_try_again() {
    let rig = Rig::new("acme");
    rig.implementers(&["sonnet-high", "opus-high"]);
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    let failed = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude.script([Scripted::Fail(failed)]);
    let Some(StepReport::Unlabelled { id, question, .. }) = step(&runner).unwrap() else {
        panic!("no ruling");
    };
    assert!(
        question.contains("its call failed: claude failed: overloaded"),
        "{question}"
    );
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    rig.claude
        .script([Scripted::Say("{\"agent\": \"sonnet-high\"}")]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Labelled { issue: 7, .. })
    ));
}

#[test]
fn a_call_stopped_with_the_runner_raises_nothing_and_is_asked_again() {
    let rig = Rig::new("acme");
    rig.implementers(&["sonnet-high", "opus-high"]);
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    rig.claude.script([Scripted::Fail(AgentError::Stopped)]);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    rig.claude
        .script([Scripted::Say("{\"agent\": \"opus-high\"}")]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Labelled { issue: 7, .. })
    ));
    assert_eq!(rig.claude.all_calls().len(), 2);
}

#[test]
fn the_reply_is_read_from_its_last_line_of_json_whatever_braces_come_before() {
    let rig = Rig::new("acme");
    rig.implementers(&["sonnet-high", "opus-high"]);
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    let reply = "It touches `fn pick() { board.first() }` and `{ \"agent\": 1 }`.\n\
                 {\"agent\": \"sonnet-high\"}\n\
                 {\"agent\": \"opus-high\"}\n\
                 That is all.";
    rig.claude.script([Scripted::Say(reply)]);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Labelled {
            issue: 7,
            agent: "opus-high".to_owned().try_into().unwrap(),
        })
    );
}

#[test]
fn add_refuses_an_issue_the_issue_writer_is_labelling() {
    let rig = Rig::new("acme");
    rig.implementers(&["sonnet-high", "opus-high"]);
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(PATIENCE), "the call never began");
    assert_eq!(
        rig.ask(&runner, "add", Some("7")),
        json!({ "error": "the issue writer is labelling #7: add it once that call ends" })
    );
    rig.forge.label(7, "agent:sonnet-high");
    assert!(
        matches!(advance(&runner).unwrap(), Pass::Idle),
        "the board waits too"
    );
    hold.release();
    assert!(hold.answered(PATIENCE), "the call never answered");
    rig.claude.script([Scripted::Say("done")]);
    assert_eq!(
        dispatched_on(step(&runner).unwrap()),
        "sonnet-high",
        "the hand label stands"
    );
    let labels = rig.forge.issue_labels(7);
    let agents: Vec<&String> = labels.iter().filter(|l| l.starts_with("agent:")).collect();
    assert_eq!(agents, ["agent:sonnet-high"]);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
    let notes = runner.lock().unwrap().take_notes();
    let stands = "#7: labelled `agent:sonnet-high` while the issue writer ran, so that \
                  stands and its pick goes unused";
    assert!(notes.iter().any(|n| n == stands), "{notes:?}");
}

#[test]
fn an_unlabelled_ruling_takes_a_yes_and_refuses_a_no() {
    let rig = Rig::new("acme");
    rig.implementers(&["sonnet-high", "opus-high"]);
    let runner = rig.open().unwrap();
    rig.forge.list_ready(7, false);
    rig.claude.script([Scripted::Say("no idea")]);
    let Some(StepReport::Unlabelled { id, .. }) = step(&runner).unwrap() else {
        panic!("no ruling");
    };
    let refused = rig.ask(&runner, "rule", Some(&format!("{id} no pick opus")));
    let why = format!(
        "ruling {id} waits on an issue's `agent:` label, which no worker can take a note \
         for: label the issue, then answer yes"
    );
    assert_eq!(refused, json!({ "error": why }));
    assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], id);
}

#[test]
fn a_drain_waits_out_the_issue_writers_ceiling() {
    let rig = Rig::new("acme");
    rig.implementers(&["sonnet-high", "opus-high"]);
    rig.edit_settings(|s| s.replace("turn_timeout = 60", "turn_timeout = 5"));
    let runner = rig.open().unwrap();
    assert_eq!(rig.ask(&runner, "drain", None)["draining"]["ceiling"], 300);
    rig.ask(&runner, "undrain", None);
    rig.forge.list_ready(7, false);
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(PATIENCE), "the call never began");
    let draining = rig.ask(&runner, "drain", None)["draining"].clone();
    let calls = json!([{ "issue": 7, "role": "issue_writer" }]);
    assert_eq!(draining, json!({ "calls": calls, "ceiling": 15 * 60 }));
    hold.release();
    assert!(hold.answered(PATIENCE), "the call never answered");
}
