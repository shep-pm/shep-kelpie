//! A new project's review, through the runner and its stand-ins: the qwen
//! round, then `defect-hunter`, which looks twice before one fix turn

use std::sync::Mutex;

use serde_json::{Value, json};

use crate::ports::{Cost, Role, SessionId, Tools, Usage};
use crate::runner::report::{ReviewResult, Spent};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Effort;
use crate::test::{Rig, Scripted, Seen};
use crate::work_item::CallKind;

// A new project whose worker opened pull request 71 and whose qwen round
// found nothing, so `defect-hunter` is next.
fn at_defect_hunter() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.default_review();
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

fn looks(rig: &Rig) -> Vec<Seen> {
    let seen = rig.claude.all_seen();
    seen.into_iter()
        .filter(|s| s.call.role == Role::Reviewer)
        .collect()
}

const MEDIUM: &str =
    "MEDIUM|work.txt:1|the flag is read before it is set|a caller sees the old value";
const OTHER: &str = "MEDIUM|src/other.rs:9|an empty list panics|the reviewer sees a crash";

#[test]
fn defect_hunter_looks_twice_and_the_worker_gets_one_fix_turn() {
    let (rig, runner) = at_defect_hunter();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text(OTHER),
        Scripted::Push("fixed.txt", "fixed\n"),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);

    assert_eq!(state(&rig, &runner), "ci", "the review ends after the fix");
    assert_eq!(
        roles(&rig),
        [
            Role::Worker,   // opens the pull request
            Role::Reviewer, // the first look
            Role::Reviewer, // the second, shown the first's findings
            Role::Worker,   // the one fix turn, for both lists
        ]
    );
    assert!(
        reports.contains(&StepReport::FirstLook {
            issue: 7,
            pull_request: 71,
            round: 2,
            reviewer: "defect-hunter".to_owned().try_into().unwrap(),
            findings: 1,
        }),
        "{reports:#?}"
    );

    let looks = looks(&rig);
    for seen in &looks {
        assert_eq!(seen.call.model, "claude-opus-5-5");
        assert_eq!(seen.call.effort, Effort::High);
        assert_eq!(seen.call.tools, Tools::Review, "it runs no command");
    }
    assert_ne!(
        looks[0].call.session.id(),
        looks[1].call.session.id(),
        "every session is a fresh one"
    );
    let (first, second) = (&looks[0].call.prompt, &looks[1].call.prompt);
    assert!(first.contains("for defects: bugs a careful maintainer"));
    assert!(!first.contains("Another reader has already read"));
    assert!(second.contains("Another reader has already read"));
    assert!(second.contains(MEDIUM), "the second is shown the first's");
    assert!(
        first.contains("#7 ") && first.contains("asks for the following"),
        "the issue it checks the change against: {first}"
    );

    let fix = rig.claude.calls().pop().unwrap();
    assert!(fix.prompt.contains("found 2 finding(s)"), "{}", fix.prompt);
    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        held.contains(MEDIUM) && held.contains(OTHER),
        "both lists go to the fix turn: {held}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["by_role"]["reviewer"]["calls"], 2);
}

#[test]
fn two_looks_that_find_nothing_end_the_review_with_no_fix_turn() {
    let (rig, runner) = at_defect_hunter();
    rig.claude
        .script([Scripted::Text("CLEAN"), Scripted::Text("CLEAN")]);
    let reports = until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci");
    assert_eq!(roles(&rig), [Role::Worker, Role::Reviewer, Role::Reviewer]);
    assert!(
        reports.contains(&StepReport::ReviewFindingsSent {
            issue: 7,
            pull_request: 71,
            round: 2,
            held: 0,
        }),
        "{reports:#?}"
    );
    let second = &looks(&rig)[1].call.prompt;
    assert!(second.contains("--- the first reader's findings ---\nCLEAN\n--- end ---"));
}

#[test]
fn nits_alone_from_both_looks_are_not_worth_a_fix_turn() {
    let (rig, runner) = at_defect_hunter();
    rig.claude.script([
        Scripted::Text("LOW|work.txt:1|a name is misleading|a reader is confused"),
        Scripted::Text("CLEAN"),
    ]);
    until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci");
    assert_eq!(roles(&rig), [Role::Worker, Role::Reviewer, Role::Reviewer]);
}

#[test]
fn a_fix_turn_that_pushes_nothing_parks_like_any_reviewers() {
    let (rig, runner) = at_defect_hunter();
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
fn the_second_look_is_its_own_stage_in_status_and_a_reviewers_time() {
    let (rig, runner) = at_defect_hunter();
    rig.claude
        .script([Scripted::Text("CLEAN"), Scripted::Text("CLEAN")]);
    step(&runner).unwrap(); // the first look
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"]["stage"],
        json!({ "stage": "second-look", "first": [] }),
        "{status}"
    );
    let timings = rig.ask(&runner, "timings", None);
    let seconds: &Value = &timings["seconds"];
    assert!(seconds.get("review").is_some(), "{timings}");
    assert!(seconds.get("deep_round").is_none(), "{timings}");
}

#[test]
fn a_session_that_ends_after_the_review_moved_on_still_keeps_its_cost_and_its_end() {
    let (rig, runner) = at_defect_hunter();
    rig.claude
        .script([Scripted::Text("CLEAN"), Scripted::Text("CLEAN")]);
    until_it_leaves_review(&rig, &runner);
    let report = {
        let mut runner = runner.lock().unwrap();
        runner.mark_review_call_running(CallKind::Claude).unwrap();
        let spent = Spent::Claude {
            role: Role::Reviewer,
            session: SessionId("late".into()),
            usage: Usage::default(),
            session_cost: Some(Cost(7)),
        };
        let reviewed = crate::runner::report::Reviewed {
            result: ReviewResult::Findings(Ok(Vec::new())),
            spent: Some(spent),
        };
        runner.end_review(reviewed).unwrap()
    };
    assert_eq!(report, None, "there is no round left for it to answer");
    let item = &rig.ask(&runner, "status", None)["work_item"];
    assert_eq!(item["by_role"]["reviewer"]["calls"], 3, "{item}");
    assert_ne!(item["review_call"]["state"], "running", "{item}");
}
