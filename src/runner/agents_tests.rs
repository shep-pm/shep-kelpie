use std::sync::Mutex;

use crate::ports::{Checks, Finding, Role, Severity};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Effort;
use crate::test::{Rig, Scripted, ScriptedRound};

const SONNET_LOW: &str = "---\nrole: reviewer\nharness: claude-code\n\
                          model: claude-sonnet-5\neffort: low\n---\nReview {{DIFF}}.\n";
const HAIKU: &str = "---\nrole: implementer\nharness: claude-code\n\
                     model: claude-haiku-4-5-20251001\neffort: low\n---\n";

/// The example's own `[agents]` table
const LISTED: &str = "[app.dogs.kelpie.agents]\nimplementers = [\"sonnet-high\"]\n";

// The rig's project with `names` as its `[agents]` table.
fn name_agents(rig: &Rig, names: &str) {
    rig.edit_settings(|s| {
        assert!(s.contains(LISTED), "the example's agents table moved");
        s.replace(LISTED, &format!("[app.dogs.kelpie.agents]\n{names}"))
    });
}

// Issue 7 from dispatch to merge under `auto`: the worker's turn, a qwen
// round whose one MEDIUM finding goes to the worker, the fix, and a clean
// Claude round that reads it, each reviewer once.
fn whole_work_item(rig: &Rig, runner: &Mutex<Runner>) {
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
    assert_eq!(roles(&rig), [Role::Worker, Role::Worker, Role::Reviewer]);
    assert_eq!(rig.reviewer.seen().len(), 1, "qwen once, before the fix");
}

#[test]
fn a_project_naming_a_different_agent_per_role_reaches_each() {
    let rig = Rig::new("shep");
    rig.merge_auto();
    rig.write_agent("sonnet-low", SONNET_LOW);
    name_agents(&rig, "implementers = [\"opus-high\"]\n");
    rig.reviewers(&["qwen", "sonnet-low"]);
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
            on(Role::Worker, "claude-opus-5-5", Effort::High),
            on(Role::Reviewer, "claude-sonnet-5", Effort::Low),
        ]
    );
}

#[test]
fn naming_an_agent_while_the_runner_runs_takes_effect_at_the_next_dispatch() {
    let rig = Rig::new("shep");
    rig.write_agent("haiku", HAIKU);
    let runner = rig.open().unwrap();
    rig.implementers(&["haiku"]);
    let line = runner
        .lock()
        .unwrap()
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: agents now in effect")
    );
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
    let taken = rig.paths().worker.join("settings-7.json");
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
    rig.reviewers(&["fable"]);
    let err = rig.open().unwrap_err().to_string();
    assert!(
        err.contains("`agents.reviewers` names fable, which has no agent file"),
        "{err}"
    );
    rig.edit_settings(|s| {
        s.replace("reviewers = [\"fable\"]\n", crate::test::RIG_REVIEWERS)
            .replace(
                "implementers = [\"sonnet-high\"]\n",
                "implementers = [\"fable\"]\n",
            )
    });
    let err = rig.open().unwrap_err().to_string();
    assert_eq!(
        err,
        "setting `agents`: `agents.implementers` names fable, which has no \
         agent file: write `agents/fable.md` in kelpie's home"
    );
}

#[test]
fn an_agent_file_that_cannot_be_used_stops_the_runner_naming_the_file_and_the_key() {
    let rig = Rig::new("shep");
    let file = rig.paths().agents.join("qwen.md");
    let cases = [
        ("role: implementer\n", "must start with a `---` line"),
        (
            "---\nrole: implementer\nharness: gemini\nmodel: m\neffort: low\n---\n",
            "`harness`: unknown variant `gemini`",
        ),
        (
            "---\nrole: implementer\nharness: pi\nmodel: m\neffort: low\n---\n",
            "runs on pi, which needs the model's server as `url` and its context size as \
             `context`",
        ),
    ];
    for (text, why) in cases {
        rig.write_agent("qwen", text);
        let err = rig.open().unwrap_err().to_string();
        assert!(
            err.starts_with(&format!("agent file {}: ", file.display())),
            "{err}"
        );
        assert!(err.contains(why), "{err}\nwanted: {why}");
    }
}

#[test]
fn an_md_file_named_for_no_agent_is_skipped_and_logged_and_the_runner_starts() {
    let rig = Rig::new("shep");
    rig.write_agent("README", "# What these are\n");
    let runner = rig.open().unwrap();
    let file = rig.paths().agents.join("README.md");
    assert_eq!(
        runner.lock().unwrap().take_notes(),
        [format!(
            "agent file {} is skipped: an agent's name is lowercase letters, digits and `-`",
            file.display()
        )]
    );
}
