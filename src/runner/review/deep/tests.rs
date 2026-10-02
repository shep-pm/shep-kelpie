//! A new project's review, through the runner and its stand-ins

use std::sync::Mutex;

use serde_json::Value;

use crate::ports::{Role, Tools};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Effort;
use crate::test::{Rig, Scripted, Seen};

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
const HIGH: &str = "HIGH|work.txt:1|the value is read before it is set|a caller gets nothing back";

#[test]
fn a_new_projects_review_is_one_deep_round_one_fix_turn_and_one_recheck_of_the_fix() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text(OTHER),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("FIXED|1|work.txt:1 sets it first\nFIXED|2|the empty list returns early"),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);

    assert_eq!(
        state(&rig, &runner),
        "ci",
        "the loop ends after the re-check"
    );
    assert_eq!(
        roles(&rig),
        [
            Role::Worker,       // opens the pull request
            Role::DeepReviewer, // the first reader
            Role::DeepReviewer, // the second reader, shown the first's findings
            Role::Worker,       // the one fix turn, for both lists
            Role::DeepReviewer, // the re-check of the fix
        ]
    );
    assert!(
        reports.iter().any(|r| matches!(
            r,
            StepReport::DeepRechecked {
                fixed: 2,
                unfixed: 0,
                ..
            }
        )),
        "{reports:#?}"
    );

    let deep = deep_calls(&rig);
    for seen in &deep {
        assert_eq!(seen.call.model, "claude-opus-5-5");
        assert_eq!(seen.call.effort, Effort::High);
    }
    let sessions: Vec<String> = deep.iter().map(|s| s.call.session.id().0.clone()).collect();
    assert_eq!(
        sessions
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "every session is a fresh one"
    );
    assert_eq!(deep[0].call.tools, Tools::Review);
    assert_eq!(deep[1].call.tools, Tools::Review);
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
    assert!(fix.prompt.contains("held 2 finding(s)"), "{}", fix.prompt);
    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        held.contains(MEDIUM) && held.contains(OTHER),
        "both lists go to the fix turn: {held}"
    );

    assert_eq!(
        deep[2].call.tools,
        Tools::Work,
        "the re-check runs commands"
    );
    let shown = deep[2]
        .call
        .prompt
        .split_once("--- the fix, diff since")
        .unwrap()
        .1;
    assert!(
        shown.contains("+++ b/fixed.txt"),
        "it is shown the fix commits: {shown}"
    );
    assert!(!shown.contains("work.txt"), "and only those: {shown}");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["by_role"]["deep_reviewer"]["calls"], 3);
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
            clean: true,
        }),
        "{reports:#?}"
    );
    let second = &deep_calls(&rig)[1].call.prompt;
    assert!(second.contains("--- the first reader's findings ---\nCLEAN\n--- end ---"));
}

#[test]
fn a_high_is_confirmed_with_a_failing_test_that_the_fix_is_rechecked_by_running() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::Write(
            "tests/high.rs",
            "#[test]\nfn fails() { panic!() }\n",
            "CONFIRMED|tests/high.rs|cargo test --test high",
        ),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("FIXED|1|`cargo test --test high` passes: 1 passed"),
    ]);
    until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci");
    assert_eq!(
        roles(&rig),
        [
            Role::Worker,
            Role::DeepReviewer,
            Role::DeepReviewer,
            Role::DeepReviewer, // the confirmation
            Role::Worker,
            Role::DeepReviewer, // the re-check
        ]
    );

    let deep = deep_calls(&rig);
    let confirm = &deep[2].call;
    assert_eq!(confirm.tools, Tools::Work, "it may run commands");
    assert!(confirm.prompt.contains(HIGH), "{}", confirm.prompt);
    assert!(confirm.prompt.contains("failing test"));
    let fence = confirm.reach.fence.as_ref().expect("it is fenced");
    let (worktree, build) = (confirm.cwd.clone(), rig.build_7());
    assert_eq!(
        fence.write,
        [worktree, build],
        "it writes its test and builds, and can commit nothing"
    );

    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        held.contains("failing test, in tests/high.rs: `cargo test --test high`"),
        "{held}"
    );
    let recheck = &deep[3].call;
    assert_eq!(
        recheck.tools,
        Tools::Work,
        "it runs the test, which it may do"
    );
    assert!(
        recheck.prompt.contains("`cargo test --test high`")
            && recheck.prompt.contains("tests/high.rs"),
        "it is told which test to run: {}",
        recheck.prompt
    );
    assert!(recheck.prompt.contains("run that test now"));
}

#[test]
fn a_fix_the_failing_test_still_fails_for_goes_back_to_the_worker_once_and_then_to_a_ruling() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::Write(
            "tests/high.rs",
            "#[test]\nfn fails() { panic!() }\n",
            "CONFIRMED|tests/high.rs|cargo test --test high",
        ),
        Scripted::Push("one.txt", "1\n"),
        Scripted::Text("UNFIXED|1|`cargo test --test high` still fails: the value is read first"),
        Scripted::Push("two.txt", "2\n"),
        Scripted::Text("UNFIXED|1|it still fails the same way"),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    let Some(StepReport::Ruling { id, question, .. }) = reports.last().cloned() else {
        panic!("the second unfixed re-check raised no ruling: {reports:#?}");
    };
    assert!(
        question.contains("re-checked the worker's fix twice and still finds these unfixed"),
        "{question}"
    );
    assert!(
        question.contains("it still fails the same way"),
        "{question}"
    );
    assert_eq!(
        roles(&rig).iter().filter(|r| **r == Role::Worker).count(),
        3,
        "the first turn, the fix, and the one trip back"
    );
    let again = rig.claude.calls()[2].prompt.clone();
    assert!(
        again.starts_with("Kelpie's re-check of your fix"),
        "{again}"
    );
    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        held.contains("still unfixed after your fix: it still fails the same way"),
        "the worker is told what is still wrong: {held}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["kind"], "deep-review");

    // A yes sends it the findings once more, and the re-check of that fix ends the loop.
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    rig.claude.script([
        Scripted::Push("three.txt", "3\n"),
        Scripted::Text("FIXED|1|`cargo test --test high` passes"),
    ]);
    until_it_leaves_review(&rig, &runner);
    assert_eq!(state(&rig, &runner), "ci");
    assert_eq!(
        roles(&rig).iter().filter(|r| **r == Role::Worker).count(),
        4
    );
}

#[test]
fn a_high_no_session_could_confirm_goes_to_the_fix_turn_marked_unconfirmed() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(HIGH),
        Scripted::Text("CLEAN"),
        Scripted::Text("UNCONFIRMED|a caller already guards it"),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("FIXED|1|the value is set first now"),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    assert!(
        reports.iter().any(|r| matches!(
            r,
            StepReport::DeepConfirmed { backed: false, finding, .. } if finding == "work.txt:1"
        )),
        "{reports:#?}"
    );
    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    assert!(
        held.contains("unconfirmed, no failing test could be made: a caller already guards it"),
        "{held}"
    );
    assert!(held.contains(HIGH), "it still goes to the worker: {held}");
}

#[test]
fn a_fix_turn_that_pushes_nothing_parks_instead_of_being_rechecked() {
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
        question.contains("ended its fix for round 2 of the qwen-review loop without pushing"),
        "{question}"
    );
    assert_eq!(
        roles(&rig).last(),
        Some(&Role::Worker),
        "no re-check of a fix that is not there"
    );
}

#[test]
fn a_recheck_that_says_nothing_of_the_findings_is_a_failed_gate_and_runs_again() {
    let (rig, runner) = at_the_deep_round();
    rig.claude.script([
        Scripted::Text(MEDIUM),
        Scripted::Text("CLEAN"),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("Looks good to me."),
        Scripted::Text("FIXED|1|it sets the flag first"),
    ]);
    let reports = until_it_leaves_review(&rig, &runner);
    assert!(
        reports.iter().any(
            |r| matches!(r, StepReport::GateFailed { reason, .. } if reason.contains("re-check"))
        ),
        "{reports:#?}"
    );
    assert_eq!(state(&rig, &runner), "ci");
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
