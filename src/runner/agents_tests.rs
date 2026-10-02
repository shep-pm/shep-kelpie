use std::sync::Mutex;

use crate::ports::{Checks, Finding, Role, Severity};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Effort;
use crate::test::{Rig, Scripted, ScriptedRound};

const AGENTS: &str = "[agents.opus-high]\nharness = \"claude-code\"\n\
                      model = \"claude-opus-5-5\"\neffort = \"high\"\n\
                      [agents.sonnet-low]\nharness = \"claude-code\"\n\
                      model = \"claude-sonnet-5\"\neffort = \"low\"\n\
                      [agents.haiku]\nharness = \"claude-code\"\n\
                      model = \"claude-haiku-4-5-20251001\"\neffort = \"low\"\n";

// Issue 7 from dispatch to merge under `auto`: the worker's turn, a qwen
// round with one finding the judge holds, the fix, then two clean rounds.
fn whole_work_item(rig: &Rig, runner: &Mutex<Runner>) {
    rig.ask(runner, "start", None);
    rig.ask(runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
        severity: Severity::Medium,
        file: "work.txt".into(),
        line: 1,
        what: "a typo".into(),
        why: "it reads wrong".into(),
    }])]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text(r#"{"holds": true, "severity": "medium", "reason": "it is a typo"}"#),
        Scripted::Push("work.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..40 {
        if let Some(head) = rig.forge.head_of("kelpie/7") {
            rig.forge.set_checks(&head, Checks::Passed);
        }
        rig.verdict(runner);
        if !rig.forge.merges().is_empty() {
            return;
        }
    }
    panic!("issue 7 never merged: {:?}", rig.claude.all_calls());
}

fn roles(rig: &Rig) -> Vec<Role> {
    rig.claude.all_calls().iter().map(|c| c.role).collect()
}

#[test]
fn a_stand_in_agent_runs_a_whole_work_item() {
    let rig = Rig::new("shep");
    rig.merge_auto();
    let runner = rig.open().unwrap();
    whole_work_item(&rig, &runner);
    let [(number, _)] = rig.forge.merges().try_into().unwrap();
    assert_eq!(number, 71);
    assert_eq!(
        roles(&rig),
        [
            Role::Worker,
            Role::Judge,
            Role::Worker,
            Role::Reviewer,
            Role::Auditor
        ]
    );
    assert_eq!(
        rig.reviewer.seen().len(),
        2,
        "qwen before and after the fix"
    );
}

#[test]
fn a_project_naming_a_different_agent_per_role_reaches_each() {
    let rig = Rig::new("shep");
    rig.merge_auto();
    let kelpie = std::fs::read_to_string(rig.paths().kelpie_settings).unwrap();
    rig.set_kelpie_settings(&format!("{kelpie}\n{AGENTS}"));
    rig.edit_settings(|s| {
        let gate = "\n[app.dogs.kelpie.coderabbit]\n";
        let agents = "[app.dogs.kelpie.agents]\nworker = \"opus-high\"\n\
                      reviewer = \"sonnet-low\"\njudge = \"haiku\"\nauditor = \"haiku\"\n";
        s.replacen(gate, &format!("\n{agents}{gate}"), 1)
    });
    let runner = rig.open().unwrap();
    whole_work_item(&rig, &runner);
    let seen: Vec<(Role, String, Effort)> = rig
        .claude
        .all_calls()
        .into_iter()
        .map(|c| (c.role, c.model, c.effort))
        .collect();
    let on = |role, model: &str, effort| (role, model.to_owned(), effort);
    assert_eq!(
        seen,
        [
            on(Role::Worker, "claude-opus-5-5", Effort::High),
            on(Role::Judge, "claude-haiku-4-5-20251001", Effort::Low),
            on(Role::Worker, "claude-opus-5-5", Effort::High),
            on(Role::Reviewer, "claude-sonnet-5", Effort::Low),
            on(Role::Auditor, "claude-haiku-4-5-20251001", Effort::Low),
        ]
    );
}

#[test]
fn naming_an_agent_while_the_runner_runs_takes_effect_at_the_next_dispatch() {
    let rig = Rig::new("shep");
    let kelpie = std::fs::read_to_string(rig.paths().kelpie_settings).unwrap();
    rig.set_kelpie_settings(&format!("{kelpie}\n{AGENTS}"));
    let runner = rig.open().unwrap();
    rig.edit_settings(|s| {
        let gate = "\n[app.dogs.kelpie.coderabbit]\n";
        let agents = "[app.dogs.kelpie.agents]\nworker = \"haiku\"\n";
        s.replacen(gate, &format!("\n{agents}{gate}"), 1)
    });
    let line = runner
        .lock()
        .unwrap()
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: agents now in effect")
    );
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say("done")]);
    crate::runner::step(&runner).unwrap();
    let [call] = rig.claude.calls().try_into().unwrap();
    assert_eq!(
        (call.model.as_str(), call.effort),
        ("claude-haiku-4-5-20251001", Effort::Low)
    );
}

// The write comes before the turn is marked running, so a yes retries the
// red CI turn itself rather than resuming one that never began.
#[test]
fn a_settings_file_that_cannot_be_written_keeps_the_turn_for_its_retry() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::CiFailed { .. })
    ));
    let calls = rig.claude.calls().len();
    // The first turn's file makes way for a folder of the same name.
    let taken = rig.paths().worker.join("settings.json");
    std::fs::remove_file(&taken).unwrap();
    std::fs::create_dir(&taken).unwrap();
    let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
        panic!("the turn did not fail");
    };
    assert!(question.contains("cannot write "), "{question}");
    assert_eq!(rig.claude.calls().len(), calls, "no call ran");
    step(&runner).unwrap(); // the alert

    std::fs::remove_dir(&taken).unwrap();
    rig.ask(&runner, "rule", Some("1 yes"));
    rig.claude.script([Scripted::Say("fixed")]);
    step(&runner).unwrap();
    let retry = rig.claude.calls().pop().unwrap();
    assert!(
        retry.prompt.starts_with("/mattpocock:diagnosing-bugs "),
        "{}",
        retry.prompt
    );
    let written = std::fs::read_to_string(&taken).unwrap();
    assert!(written.contains("\"sandbox\""), "{written}");
}

#[test]
fn a_role_naming_an_agent_kelpie_lacks_stops_the_runner_naming_it() {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| {
        let gate = "\n[app.dogs.kelpie.coderabbit]\n";
        s.replacen(
            gate,
            &format!("\n[app.dogs.kelpie.agents]\njudge = \"fable\"\n{gate}"),
            1,
        )
    });
    let err = rig.open().unwrap_err().to_string();
    assert!(
        err.contains("`agents.judge` names fable, which is not defined"),
        "{err}"
    );
}
