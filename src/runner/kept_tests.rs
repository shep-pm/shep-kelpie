use std::sync::Mutex;

use serde_json::{Value, json};

use crate::runner::{Runner, StepReport, step};
use crate::settings::{AgentHarness, Effort};
use crate::test::{Rig, Scripted};

// The rig's runner with issue 7 open, as a build before agent files saved
// it: its worker on `worker`, its first turn due.
fn saved_before_agent_files(rig: &Rig, worker: Value) -> Mutex<Runner> {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    drop(runner);
    let path = rig.paths().state;
    let mut state: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let item = state["work_items"][0].as_object_mut().unwrap();
    item.remove("agent");
    item.insert("worker".into(), worker);
    std::fs::write(&path, state.to_string()).unwrap();
    rig.open().unwrap()
}

#[test]
fn an_item_on_a_claude_model_with_no_agent_file_gets_one_and_runs_on_it() {
    let rig = Rig::new("shep");
    let worker = json!({ "model": "claude-opus-5-5", "effort": "medium" });
    let runner = saved_before_agent_files(&rig, worker);
    let file = rig.paths().agents.join("opus-medium.md");
    assert!(file.is_file(), "the start writes the agent file");
    assert_eq!(
        runner.lock().unwrap().take_notes(),
        [
            "issue #7's worker ran claude-opus-5-5 at medium before agent files, so kelpie \
          wrote `agents/opus-medium.md` for it, on Claude Code"
        ]
    );

    rig.claude.script([Scripted::Say("done")]);
    step(&runner).unwrap();
    let [call] = rig.claude.calls().try_into().unwrap();
    assert_eq!(call.harness, AgentHarness::ClaudeCode);
    assert_eq!(
        (call.model.as_str(), call.effort),
        ("claude-opus-5-5", Effort::Medium)
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["agent"], "opus-medium");

    drop(runner);
    let again = rig.open().unwrap();
    assert_eq!(again.lock().unwrap().take_notes(), Vec::<String>::new());
}

#[test]
fn an_item_on_a_local_or_other_harness_model_keeps_its_failed_turn() {
    let cases = [
        (
            json!({ "model": "qwen3.8:27b", "effort": "low", "local": true }),
            "local",
        ),
        (
            json!({ "model": "gpt-6-sol", "effort": "medium" }),
            "gpt-6-sol-medium",
        ),
    ];
    for (worker, agent) in cases {
        let rig = Rig::new("shep");
        let runner = saved_before_agent_files(&rig, worker);
        assert_eq!(runner.lock().unwrap().take_notes(), Vec::<String>::new());
        assert!(!rig.paths().agents.join(format!("{agent}.md")).exists());
        let Some(StepReport::Failed { question, .. }) = step(&runner).unwrap() else {
            panic!("the turn on {agent} did not fail");
        };
        assert!(
            question.contains(&format!(
                "issue #7 runs on agent {agent}, which has no agent file now: write \
                 `agents/{agent}.md` in kelpie's home again, or drop and add the issue"
            )),
            "{question}"
        );
        assert_eq!(rig.claude.calls(), []);
    }
}
