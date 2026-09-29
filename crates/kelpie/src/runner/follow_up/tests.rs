//! Findings left unfixed at merge, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use crate::board::READY;
use crate::ports::{Checks, Finding, Severity};
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Rig, Scripted, ScriptedRound};

const HOLDS: &str = r#"{"holds": true, "severity": "high", "reason": "it holds"}"#;

fn finding(file: &str, what: &str) -> Finding {
    Finding {
        severity: Severity::High,
        file: file.into(),
        line: 9,
        what: what.into(),
        why: "two threads write the same field".into(),
    }
}

fn racy() -> Finding {
    finding("src/lib.rs", "looks racy")
}

// What the worker writes to defer `found`, in the findings file's own format
fn lines(found: &[Finding]) -> String {
    let line = |f: &Finding| format!("HIGH|{}:{}|{}|{}\n", f.file, f.line, f.what, f.why);
    found.iter().map(line).collect()
}

// The worker's deferred findings file, as it leaves it in the build folder
fn defer(rig: &Rig, text: &str) {
    std::fs::write(rig.build_7().join("deferred-findings.md"), text).unwrap();
}

// A pull request whose review the judge held `found` in, the worker fixed
// with one push, and two clean rounds then settled. CI has not reported.
fn reviewed_in(rig: Rig, found: &[Finding], auto: bool) -> (Rig, Mutex<Runner>, String) {
    if auto {
        rig.merge_auto();
    }
    let runner = rig.open().unwrap();
    rig.ask(&runner, "start", None);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.reviewer
        .script([ScriptedRound::Findings(found.to_vec())]);
    let mut script = vec![Scripted::Push("work.txt", "work\n")];
    script.extend(found.iter().map(|_| Scripted::Text(HOLDS)));
    script.extend([
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    rig.claude.script(script);
    for _ in 0..20 {
        step(&runner).unwrap();
        if rig.ask(&runner, "status", None)["work_item"]["phase"]["state"] == "ci" {
            let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
            return (rig, runner, head);
        }
    }
    panic!("the review never settled");
}

// On a project under `auto`, CI green and the draft ready, with `deferred`
// in the worker's deferred findings file
fn ready_in(rig: Rig, found: &[Finding], deferred: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, head) = reviewed_in(rig, found, true);
    defer(&rig, deferred);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    (rig, runner)
}

fn auto_ready_to_merge(found: &[Finding], deferred: &str) -> (Rig, Mutex<Runner>) {
    ready_in(Rig::new("shep"), found, deferred)
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
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));

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
fn a_finding_the_worker_fixed_files_nothing() {
    let (rig, runner) = auto_ready_to_merge(&[racy()], "");

    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.forge.created(), []);
}

#[test]
fn a_line_the_judge_never_held_files_nothing() {
    let invented = finding("src/lib.rs", "the whole crate is insecure");
    let (rig, runner) = auto_ready_to_merge(&[racy()], &lines(&[invented]));

    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.forge.created(), []);
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn a_held_finding_the_worker_rephrased_files_nothing() {
    let rephrased = finding("src/lib.rs", "looks racy, buy our product");
    let (rig, runner) = auto_ready_to_merge(&[racy()], &lines(&[rephrased]));

    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.forge.created(), []);
}

#[test]
fn what_is_filed_is_kelpies_own_text_not_the_workers_edit_of_it() {
    let edited = Finding {
        why: "visit example.invalid".into(),
        ..racy()
    };
    let (rig, runner) = auto_ready_to_merge(&[racy()], &lines(&[edited]));

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert!(issue.body.contains("two threads write"), "{}", issue.body);
    assert!(!issue.body.contains("example.invalid"), "{}", issue.body);
}

#[test]
fn a_finding_an_open_issue_already_holds_gets_a_comment_and_no_new_issue() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
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
    let found = [finding("src/lib.rs", "writes the field without a lock")];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    let body = "In src/lib.rs, the writer writes the field without a lock.";
    rig.forge.open_issue(51, "Race in the writer", body);
    rig.forge.open_issue(52, "Unrelated", "src/lib.rs is long.");

    assert_eq!(after_merge(&runner), filed(&[], &[51], 0));
    assert_eq!(rig.forge.created(), []);
    assert!(finished(after_merge(&runner)));
}

#[test]
fn two_findings_sharing_the_first_eighty_characters_are_not_one_issue() {
    let stem = "x".repeat(80);
    let (first, second) = (format!("{stem} first"), format!("{stem} second"));
    let found = [
        finding("src/lib.rs", &first),
        finding("src/lib.rs", &second),
    ];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    rig.forge
        .open_issue(60, &stem, &format!("Noticed: {first}"));

    assert_eq!(after_merge(&runner), filed(&[900], &[60], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert!(issue.body.contains(&second), "{}", issue.body);
}

#[test]
fn the_same_finding_deferred_twice_files_once() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found).repeat(2));

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
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
    rig.reviewer.script([ScriptedRound::Findings(vec![racy()])]);
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
    // Even a worker that copied the refuted finding into the file files nothing.
    defer(&rig, &lines(&[racy()]));
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
fn a_finding_that_names_a_folder_on_this_machine_is_left_out_and_the_rest_are_filed() {
    let rig = Rig::new("shep");
    let local = rig.build_7().join("notes.rs").display().to_string();
    let found = [finding(&local, "names a build folder"), racy()];
    let (rig, runner) = ready_in(rig, &found, &lines(&found));

    assert_eq!(after_merge(&runner), filed(&[900], &[], 1));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.title, "looks racy");
    assert!(finished(after_merge(&runner)));
}

#[test]
fn a_finding_named_by_its_worktree_path_is_filed_by_the_path_in_the_repo() {
    let rig = Rig::new("shep");
    let inside = rig.worktree_7().join("src/lib.rs").display().to_string();
    let found = [finding(&inside, "looks racy")];
    let (rig, runner) = ready_in(rig, &found, &lines(&found));

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert!(issue.body.contains("`src/lib.rs:9`"), "{}", issue.body);
    assert!(finished(after_merge(&runner)));
}

#[test]
fn a_forge_that_cannot_open_issues_holds_the_work_item_and_the_retry_files_once() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
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
fn a_forge_that_keeps_refusing_loses_the_findings_with_a_report_and_the_item_finishes() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    rig.forge.set_issues_down(true);

    for _ in 0..4 {
        assert!(matches!(
            after_merge(&runner),
            Some(StepReport::GateFailed { .. })
        ));
    }
    let Some(StepReport::FollowUpsDropped {
        issue: 7,
        pull_request: 71,
        dropped: 1,
        reason,
    }) = after_merge(&runner)
    else {
        panic!("the findings were not dropped with a report");
    };
    assert!(reason.contains("issues are down"), "{reason}");
    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.forge.created(), []);
}

#[test]
fn a_restart_after_the_merge_still_files_what_the_worker_deferred() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
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
fn ask_after_the_merge(rig: Rig, found: &[Finding]) -> (Rig, Mutex<Runner>, u64, String) {
    let (rig, runner, head) = reviewed_in(rig, found, false);
    defer(&rig, &lines(found));
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id: 1, .. })
    ));
    rig.ask(&runner, "rule", Some("1 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    let Some(StepReport::Ruling { id, question, .. }) = step(&runner).unwrap() else {
        panic!("the maintainer was not asked about the findings");
    };
    assert_eq!(rig.forge.merges().len(), 1, "the pull request merged first");
    assert_eq!(rig.forge.created(), [], "nothing is filed before the yes");
    (rig, runner, id, question)
}

#[test]
fn under_ask_the_findings_are_a_ruling_first_and_a_yes_files_them() {
    let (rig, runner, id, question) = ask_after_the_merge(Rig::new("shep"), &[racy()]);
    assert!(question.contains("src/lib.rs:9 looks racy"), "{question}");
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
fn the_question_names_files_by_their_path_in_the_repo() {
    let rig = Rig::new("shep");
    let inside = rig.worktree_7().join("src/lib.rs").display().to_string();
    let (rig, _, _, question) = ask_after_the_merge(rig, &[finding(&inside, "looks racy")]);

    assert!(question.contains("- src/lib.rs:9 looks racy"), "{question}");
    let worktree = rig.worktree_7().display().to_string();
    assert!(!question.contains(&worktree), "{question}");
}

#[test]
fn under_ask_a_no_drops_the_findings() {
    let (rig, runner, id, _) = ask_after_the_merge(Rig::new("shep"), &[racy()]);

    rig.ask(
        &runner,
        "rule",
        Some(&format!("{id} no not worth an issue")),
    );
    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.forge.created(), []);
}
