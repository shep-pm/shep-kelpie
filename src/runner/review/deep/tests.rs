//! A new project's review, through the runner and its stand-ins

use std::sync::Mutex;

use serde_json::Value;

use crate::ports::{Cost, Role, SessionId, Tools, Usage};
use crate::runner::report::{ReviewResult, Spent};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Effort;
use crate::test::{Rig, Scripted, Seen};
use crate::work_item::CallKind;

// A new project whose worker opened pull request 71 and whose local round
// found nothing, so the deep round is next.
fn at_the_deep_round() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.deep_review();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: clean by default
    (rig, runner)
}

fn state(rig: &Rig, runner: &Mutex<Runner>) -> String {
    let status = rig.ask(runner, "status", None);
    status["work_item"]["phase"]["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

// Steps until the work item leaves review, or for any ruling, and returns what
// the steps reported.
fn until_it_leaves_review(rig: &Rig, runner: &Mutex<Runner>) -> Vec<StepReport> {
    let mut reports = Vec::new();
    for _ in 0..40 {
        if state(rig, runner) != "review" {
            return reports;
        }
        let report = step(runner).unwrap();
        let ruling = matches!(report, Some(StepReport::Ruling { .. }));
        reports.extend(report);
        if ruling {
            return reports;
        }
    }
    panic!("the review never ended: {reports:#?}");
}

fn roles(rig: &Rig) -> Vec<Role> {
    rig.claude.all_calls().iter().map(|c| c.role).collect()
}

fn deep_calls(rig: &Rig) -> Vec<Seen> {
    let seen = rig.claude.all_seen();
    seen.into_iter()
        .filter(|s| s.call.role == Role::DeepReviewer)
        .collect()
}

const MEDIUM: &str =
    "MEDIUM|work.txt:1|the flag is read before it is set|a caller sees the old value";
const OTHER: &str = "MEDIUM|src/other.rs:9|an empty list panics|the reviewer sees a crash";

#[test]
fn a_new_projects_review_is_two_deep_reads_and_one_fix_turn() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text(OTHER),
        Scripted::Push("fixed.txt", "fixed\n"),
    ]);
    until_it_leaves_review(&rig, &runner);

    assert_eq!(state(&rig, &runner), "ci", "the review ends after the fix");
    assert_eq!(
        roles(&rig),
        [
            Role::Worker,       // opens the pull request
            Role::DeepReviewer, // the first reader
            Role::DeepReviewer, // the second reader, shown the first's findings
            Role::Worker,       // the one fix turn, for both lists
        ]
    );

    let deep = deep_calls(&rig);
    for seen in &deep {
        assert_eq!(seen.call.model, "claude-opus-5-5");
        assert_eq!(seen.call.effort, Effort::High);
        assert_eq!(seen.call.tools, Tools::Review, "it runs no command");
    }
    assert_ne!(
        deep[0].call.session.id(),
        deep[1].call.session.id(),
        "every session is a fresh one"
    );
    assert!(
        deep[0]
            .call
            .prompt
            .contains("for defects: bugs a careful maintainer")
    );
    assert!(
        !deep[0]
            .call
            .prompt
            .contains("Another reader has already read")
    );
    assert!(
        deep[1]
            .call
            .prompt
            .contains("Another reader has already read")
    );
    assert!(
        deep[1].call.prompt.contains(MEDIUM),
        "the second reader is shown the first's"
    );
    assert!(
        deep[0].call.prompt.contains("#7 "),
        "the issue it checks the change against: {}",
        deep[0].call.prompt
    );

    let fix = rig.claude.calls().pop().unwrap();
    assert!(fix.prompt.contains("found 2 finding(s)"), "{}", fix.prompt);
    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        held.contains(MEDIUM) && held.contains(OTHER),
        "both lists go to the fix turn: {held}"
    );
    assert!(
        held.contains("deferred-findings.md"),
        "a finding out of scope may be deferred: {held}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["by_role"]["deep_reviewer"]["calls"], 2);
}

#[test]
fn two_readers_that_find_nothing_end_the_review_with_no_fix_turn() {
    let (rig, runner) = at_the_deep_round();
    rig.claude
        .script([Scripted::Text("CLEAN"), Scripted::Text("CLEAN")]);
    let reports = until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci");
    assert_eq!(
        roles(&rig),
        [Role::Worker, Role::DeepReviewer, Role::DeepReviewer]
    );
    assert!(
        reports.contains(&StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 2,
            held: 0,
        }),
        "{reports:#?}"
    );
    let second = &deep_calls(&rig)[1].call.prompt;
    assert!(second.contains("--- the first reader's findings ---\nCLEAN\n--- end ---"));
}

#[test]
fn nits_alone_from_both_readers_are_not_worth_a_fix_turn() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text("LOW|work.txt:1|a name is misleading|a reader is confused"),
        Scripted::Text("CLEAN"),
    ]);
    until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci");
    assert_eq!(
        roles(&rig),
        [Role::Worker, Role::DeepReviewer, Role::DeepReviewer]
    );
}

#[test]
fn a_fix_turn_that_pushes_nothing_parks_like_any_reviewers() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text("CLEAN"),
        Scripted::Say("I could not read the findings."),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    let Some(StepReport::Ruling { question, .. }) = reports.last() else {
        panic!("a fix with nothing pushed raised no ruling: {reports:#?}");
    };
    assert!(
        question.contains("ended its fix for round 2 of the review without pushing"),
        "{question}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["kind"], "fix-not-pushed");
}

#[test]
fn the_deep_round_is_its_own_phase_in_status_and_timings() {
    let (rig, runner) = at_the_deep_round();
    rig.claude
        .script([Scripted::Text("CLEAN"), Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // the first reader
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"]["stage"],
        serde_json::json!({ "stage": "deep", "step": "missed", "first": [] }),
        "{status}"
    );
    let timings = rig.ask(&runner, "timings", None);
    let seconds: &Value = &timings["seconds"];
    assert!(seconds.get("deep_round").is_some(), "{timings}");
}

#[test]
fn a_deep_call_that_ends_after_its_stage_moved_on_still_keeps_its_cost_and_its_end() {
    let (rig, runner) = at_the_deep_round(); // its stage is the next round's, not a deep step
    let report = {
        let mut runner = runner.lock().unwrap();
        runner.mark_review_call_running(CallKind::Deep).unwrap();
        let spent = Spent::Claude {
            role: Role::DeepReviewer,
            session: SessionId("late".into()),
            usage: Usage::default(),
            session_cost: Some(Cost(7)),
        };
        runner
            .end_deep(ReviewResult::Deep(Ok("CLEAN".into())), Some(spent))
            .unwrap()
    };
    assert_eq!(report, None, "there is no step left for it to answer");
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item["by_role"]["deep_reviewer"]["calls"], 1, "{item}");
    assert_ne!(item["review_call"]["state"], "running", "{item}");
}
