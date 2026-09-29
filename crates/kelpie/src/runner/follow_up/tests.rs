//! Findings left unfixed at merge, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use crate::board::READY;
use crate::ports::{Checks, Finding, Severity};
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Rig, Scripted, ScriptedRound};

const LINE: &str = "HIGH|src/lib.rs:9|looks racy|two threads write the same field\n";

// The worker's deferred findings file, as it leaves it in the build folder
fn defer(rig: &Rig, text: &str) {
    std::fs::write(rig.build_7().join("deferred-findings.md"), text).unwrap();
}

// The same project restarted under `auto`, as the maintainer would switch it
fn under_auto(rig: &Rig, runner: Mutex<Runner>) -> Mutex<Runner> {
    drop(runner);
    rig.merge_auto();
    rig.open().unwrap()
}

// A pull request with green CI whose review settled, on a project under
// `auto`, with `text` in the worker's deferred findings file
fn auto_ready_to_merge(text: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    let runner = under_auto(&rig, runner);
    defer(&rig, text);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    (rig, runner)
}

// The next step's report, past the notice a merge under `auto` queues
fn after_merge(runner: &Mutex<Runner>) -> Option<StepReport> {
    match step(runner).unwrap() {
        Some(StepReport::Noticed { .. }) => step(runner).unwrap(),
        other => other,
    }
}

fn filed(opened: &[u64], commented: &[u64], skipped: usize) -> Option<StepReport> {
    Some(StepReport::FollowUpsFiled {
        issue: 7,
        pull_request: 71,
        opened: opened.to_vec(),
        commented: commented.to_vec(),
        skipped,
    })
}

fn finished(report: Option<StepReport>) -> bool {
    matches!(
        report,
        Some(StepReport::Finished {
            issue: 7,
            pull_request: Some(71),
            merged: true,
            ..
        })
    )
}

#[test]
fn under_auto_an_unfixed_confirmed_finding_files_one_issue_on_the_board() {
    let (rig, runner) = auto_ready_to_merge(LINE);

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.title, "looks racy");
    assert_eq!(issue.labels, [READY]);
    assert!(issue.body.contains("#71"), "{}", issue.body);
    assert!(issue.body.contains("`src/lib.rs:9`"), "{}", issue.body);
    assert!(
        issue.body.contains("two threads write the same field"),
        "{}",
        issue.body
    );
    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    assert_eq!(rig.forge.created().len(), 1, "filed once, not again");
}

#[test]
fn a_finding_an_open_issue_already_holds_gets_a_comment_and_no_new_issue() {
    let (rig, runner) = auto_ready_to_merge(LINE);
    rig.forge.open_issue(50, "Looks racy", "Filed by hand.");

    assert_eq!(after_merge(&runner), filed(&[], &[50], 0));
    assert_eq!(rig.forge.created(), []);
    let [(number, body)] = rig.forge.comments().try_into().unwrap();
    assert_eq!(number, 50);
    assert!(body.contains("#71"), "{body}");
    assert!(body.contains("`src/lib.rs:9`"), "{body}");
    assert!(finished(after_merge(&runner)));
}

#[test]
fn an_issue_that_names_the_file_and_says_the_same_thing_is_a_duplicate_too() {
    let (rig, runner) =
        auto_ready_to_merge("HIGH|src/lib.rs:9|writes the field without a lock|races\n");
    let body = "In src/lib.rs, the writer writes the field without a lock.";
    rig.forge.open_issue(51, "Race in the writer", body);
    rig.forge.open_issue(52, "Unrelated", "src/lib.rs is long.");

    assert_eq!(after_merge(&runner), filed(&[], &[51], 0));
    assert_eq!(rig.forge.created(), []);
    assert!(finished(after_merge(&runner)));
}

#[test]
fn the_same_finding_twice_files_once_and_comments_once() {
    let (rig, runner) = auto_ready_to_merge(&LINE.repeat(2));

    assert_eq!(after_merge(&runner), filed(&[900], &[900], 0));
    assert_eq!(rig.forge.created().len(), 1);
}

#[test]
fn a_finding_the_judge_refuted_files_nothing() {
    let rig = Rig::new("shep");
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.reviewer.script([ScriptedRound::Findings(vec![Finding {
        severity: Severity::High,
        file: "src/lib.rs".into(),
        line: 9,
        what: "looks racy".into(),
        why: "two threads write the same field".into(),
    }])]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text(r#"{"holds": false, "severity": "high", "reason": "behind a mutex"}"#),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: one finding
    step(&runner).unwrap(); // the judge refutes it
    step(&runner).unwrap(); // the round holds nothing
    step(&runner).unwrap(); // round 2, claude: clean
    let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);

    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.forge.created(), []);
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn a_worker_that_deferred_nothing_merges_with_nothing_filed() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    let runner = under_auto(&rig, runner);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);

    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.forge.created(), []);
}

#[test]
fn a_finding_that_names_a_folder_on_this_machine_is_left_out_and_the_rest_are_filed() {
    let (rig, runner) = auto_ready_to_merge("");
    let local = rig.build_7().join("notes.rs");
    let text = format!(
        "HIGH|{}:3|names a build folder|it leaks\n{LINE}",
        local.display()
    );
    defer(&rig, &text);

    assert_eq!(after_merge(&runner), filed(&[900], &[], 1));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.title, "looks racy");
    assert!(finished(after_merge(&runner)));
}

#[test]
fn a_finding_named_by_its_worktree_path_is_filed_by_the_path_in_the_repo() {
    let (rig, runner) = auto_ready_to_merge("");
    let inside = rig.worktree_7().join("src/lib.rs");
    defer(
        &rig,
        &format!("HIGH|{}:9|looks racy|why\n", inside.display()),
    );

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert!(issue.body.contains("`src/lib.rs:9`"), "{}", issue.body);
    assert!(finished(after_merge(&runner)));
}

#[test]
fn a_forge_that_cannot_open_issues_holds_the_work_item_and_the_retry_files_once() {
    let (rig, runner) = auto_ready_to_merge(LINE);
    rig.forge.set_issues_down(true);

    let Some(StepReport::GateFailed { issue: 7, reason }) = after_merge(&runner) else {
        panic!("the failure was not reported");
    };
    assert!(reason.contains("issues are down"), "{reason}");
    assert!(
        rig.ask(&runner, "status", None)["work_item"].is_object(),
        "the merged work item stays until its findings are filed"
    );

    rig.forge.set_issues_down(false);
    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.forge.created().len(), 1);
}

#[test]
fn a_restart_after_the_merge_still_files_what_the_worker_deferred() {
    let (rig, runner) = auto_ready_to_merge(LINE);
    rig.forge.set_issues_down(true);
    assert!(matches!(
        after_merge(&runner),
        Some(StepReport::GateFailed { .. })
    ));
    drop(runner);
    rig.forge.set_issues_down(false);
    let runner = rig.open().unwrap();

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    assert!(finished(after_merge(&runner)));
}

// Under `ask`: the worker parked on merge ruling 1, then a yes, then the merge
fn ask_after_the_merge(text: &str) -> (Rig, Mutex<Runner>, u64) {
    let (rig, runner, _) = Rig::parked("shep");
    defer(&rig, text);
    rig.ask(&runner, "rule", Some("1 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    let Some(StepReport::Ruling { id, question, .. }) = step(&runner).unwrap() else {
        panic!("the maintainer was not asked about the findings");
    };
    assert!(question.contains("src/lib.rs:9 looks racy"), "{question}");
    assert_eq!(rig.forge.merges().len(), 1, "the pull request merged first");
    assert_eq!(rig.forge.created(), [], "nothing is filed before the yes");
    (rig, runner, id)
}

#[test]
fn under_ask_the_findings_are_a_ruling_first_and_a_yes_files_them() {
    let (rig, runner, id) = ask_after_the_merge(LINE);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["kind"], "follow-up");
    assert_eq!(
        rig.forge.comments(),
        [],
        "the merged pull request is left alone"
    );

    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    assert_eq!(rig.forge.created().len(), 1);
    assert!(finished(after_merge(&runner)));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        (&status["work_item"], &status["rulings"]),
        (&json!(null), &json!([]))
    );
}

#[test]
fn under_ask_a_no_drops_the_findings() {
    let (rig, runner, id) = ask_after_the_merge(LINE);

    rig.ask(
        &runner,
        "rule",
        Some(&format!("{id} no not worth an issue")),
    );
    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.forge.created(), []);
}
