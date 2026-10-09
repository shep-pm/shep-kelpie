//! Findings left unfixed at merge, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use crate::board::READY;
use crate::ports::{Checks, Finding, Severity};
use crate::runner::coderabbit::tests::{fixed, hold_a_finding, summoned};
use crate::runner::{CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Rig, Scripted, ScriptedRound};

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

// A pull request whose qwen round found `found`, the worker fixed with one
// push, and a clean Claude round then read. CI has not reported.
fn reviewed_in(rig: Rig, found: &[Finding], auto: bool) -> (Rig, Mutex<Runner>, String) {
    if auto {
        rig.merge_auto();
        rig.issues("file");
    }
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.reviewer
        .script([ScriptedRound::Findings(found.to_vec())]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..20 {
        step(&runner).unwrap();
        if rig.ask(&runner, "status", None)["work_item"]["phase"]["state"] == "ci" {
            let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
            return (rig, runner, head);
        }
    }
    panic!("the review never settled");
}

// On a project under `auto` that files deferred findings at once, CI green
// and the draft ready, with `deferred`
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
fn a_line_kelpie_never_sent_files_nothing() {
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
    rig.forge
        .open_issue(50, "Looks racy", "Filed by hand, in src/lib.rs.");

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
        .open_issue(60, &stem, &format!("In src/lib.rs: {first}"));

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

// A version 13 state file could hold a nit among the findings sent, from a
// round above a nit; deferred, it is not filed either.
#[test]
fn a_nit_an_older_state_file_held_is_not_filed() {
    let nit = Finding {
        severity: Severity::Low,
        ..finding("src/main.rs", "unused import")
    };
    let found = [racy()];
    let deferred = format!(
        "{}LOW|{}:{}|{}|{}\n",
        lines(&found),
        nit.file,
        nit.line,
        nit.what,
        nit.why
    );
    let (rig, runner) = auto_ready_to_merge(&found, &deferred);
    runner.lock().unwrap().state.work_items[0].held.push(nit);

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.title, "looks racy");
}

#[test]
fn a_nit_the_worker_deferred_files_nothing() {
    let rig = Rig::new("shep");
    rig.merge_auto();
    rig.issues("file");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    let nit = Finding {
        severity: Severity::Low,
        ..racy()
    };
    rig.reviewer
        .script([ScriptedRound::Findings(vec![nit.clone()])]);
    rig.claude.script([
        Scripted::Push("work.txt", "work\n"),
        Scripted::Text("Left the nit as out of scope."),
        Scripted::Text("CLEAN"),
    ]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: one nit
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::ReviewFindingsSent { held: 1, .. })
    ));
    // The worker copies the nit into the file and pushes nothing.
    let line = format!("LOW|{}:{}|{}|{}\n", nit.file, nit.line, nit.what, nit.why);
    defer(&rig, &line);
    step(&runner).unwrap(); // the worker's fix turn
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::NitsDeclined { nits: 1, .. })
    ));
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
fn an_issue_with_the_same_title_that_never_names_the_file_is_not_a_duplicate() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    rig.forge.open_issue(53, "Looks racy", "Somewhere else.");

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
}

#[test]
fn a_forge_that_keeps_refusing_is_retried_for_hours_and_then_the_maintainer_is_asked() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    rig.forge.set_issues_down(true);

    for _ in 0..20 {
        assert!(matches!(
            after_merge(&runner),
            Some(StepReport::GateFailed { .. })
        ));
    }
    rig.clock.advance(5 * 60 * 60);
    assert!(
        matches!(after_merge(&runner), Some(StepReport::GateFailed { .. })),
        "still inside the window: passes made no difference, only time does"
    );
    rig.clock.advance(60 * 60);
    let Some(StepReport::Ruling { id, question, .. }) = after_merge(&runner) else {
        panic!("the maintainer was not asked after the window");
    };
    assert!(question.contains("- src/lib.rs:9 looks racy"), "{question}");
    assert!(question.contains("issues are down"), "{question}");
    assert!(question.contains("tries again"), "{question}");
    assert_eq!(rig.forge.created(), [], "nothing is lost while it waits");

    rig.forge.set_issues_down(false);
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    assert!(ends_beside_900(&runner));
}

#[test]
fn a_yes_while_the_forge_still_refuses_starts_a_fresh_window_not_an_instant_ruling() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    rig.forge.set_issues_down(true);
    after_merge(&runner);
    rig.clock.advance(6 * 60 * 60);
    let Some(StepReport::Ruling { id, .. }) = after_merge(&runner) else {
        panic!("the maintainer was not asked after the window");
    };

    // Still down when the maintainer says yes.
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert!(matches!(
        after_merge(&runner),
        Some(StepReport::GateFailed { .. })
    ));
    rig.clock.advance(5 * 60 * 60);
    assert!(
        matches!(after_merge(&runner), Some(StepReport::GateFailed { .. })),
        "the yes bought a whole window"
    );
    rig.clock.advance(60 * 60);
    assert!(matches!(
        after_merge(&runner),
        Some(StepReport::Ruling { .. })
    ));
}

#[test]
fn a_finding_the_forge_takes_starts_the_refusal_window_again() {
    let found = [racy(), finding("src/main.rs", "leaks a handle")];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    rig.forge.set_issues_down(true);
    after_merge(&runner);
    rig.clock.advance(5 * 60 * 60);

    // It takes one finding, then refuses the other.
    rig.forge.set_issues_down(false);
    rig.forge.set_creates_left(1);
    assert!(matches!(
        after_merge(&runner),
        Some(StepReport::GateFailed { .. })
    ));
    assert_eq!(rig.forge.created().len(), 1);
    rig.clock.advance(60 * 60);
    assert!(
        matches!(after_merge(&runner), Some(StepReport::GateFailed { .. })),
        "six hours since the first refusal, but the forge took one since"
    );
    rig.clock.advance(5 * 60 * 60);
    assert!(matches!(
        after_merge(&runner),
        Some(StepReport::Ruling { .. })
    ));
}

#[test]
fn what_a_ruling_says_of_the_forges_refusal_is_cut_short() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    rig.forge.set_issues_error(&"x".repeat(5000));
    after_merge(&runner);
    rig.clock.advance(6 * 60 * 60);

    let Some(StepReport::Ruling { question, .. }) = after_merge(&runner) else {
        panic!("the maintainer was not asked after the window");
    };
    assert!(question.len() < 1000, "{}", question.len());
    assert!(question.contains("xxx"), "{question}");
}

#[test]
fn a_finding_with_no_line_is_asked_about_without_one() {
    let found = Finding { line: 0, ..racy() };
    let (_, _, _, question) = ask_after_the_merge(Rig::new("shep"), &[found]);

    assert!(question.contains("- src/lib.rs looks racy"), "{question}");
    assert!(!question.contains("lib.rs:0"), "{question}");
}

#[test]
fn drop_leaves_a_merged_pull_request_waiting_on_its_follow_up_ruling_alone() {
    let (rig, runner, _, _) = ask_after_the_merge(Rig::new("shep"), &[racy()]);

    let reply = rig.ask(&runner, "drop", None);
    assert!(
        reply["error"].as_str().unwrap().contains("is merging"),
        "{reply}"
    );
    assert!(rig.ask(&runner, "status", None)["work_item"].is_object());
    assert!(
        rig.worktree_7().exists(),
        "the merged item is not cleaned up as an unmerged one"
    );
}

#[test]
fn a_no_to_a_forge_that_kept_refusing_drops_the_findings() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    rig.forge.set_issues_down(true);
    after_merge(&runner);
    rig.clock.advance(6 * 60 * 60);
    let Some(StepReport::Ruling { id, .. }) = after_merge(&runner) else {
        panic!("the maintainer was not asked after the window");
    };

    rig.ask(&runner, "rule", Some(&format!("{id} no leave it")));
    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.forge.created(), []);
}

#[test]
fn a_finding_a_coderabbit_round_held_is_filed_when_the_worker_defers_it() {
    let (rig, runner, head) = summoned("shep");
    hold_a_finding(&rig, &runner, &head, "Name the flag.");
    // The worker defers what the findings file gave it, line for line.
    let held = std::fs::read_to_string(rig.build_7().join("review-findings.md")).unwrap();
    defer(&rig, &held);
    fixed(&rig, &runner, "fix.txt");
    rig.forge
        .set_state(71, crate::ports::PullRequestState::Merged);

    let Some(StepReport::Ruling { id, question, .. }) = step(&runner).unwrap() else {
        panic!("the maintainer was not asked about the findings");
    };
    assert!(question.contains("Name the flag."), "{question}");
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::FollowUpsFiled { .. })
    ));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert!(issue.title.contains("Name the flag."), "{}", issue.title);
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
fn a_merged_item_on_its_follow_up_ruling_holds_back_no_new_work() {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| s.replace("pending_rulings = 2", "pending_rulings = 1"));
    let (rig, runner, id, _) = ask_after_the_merge(rig, &[racy()]);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id }));

    // Neither `concurrency.pending_rulings` nor its merged branch's files hold #8 back.
    rig.forge.list_ready(8, false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue: 8, .. })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["parked"], json!([7]));
}

// Whether #7 finishes, once the board has opened #900, the issue just filed
// and ready: an item answered into its end holds no slot
fn ends_beside_900(runner: &Mutex<Runner>) -> bool {
    match after_merge(runner) {
        Some(StepReport::Dispatched { issue: 900, .. }) => finished(after_merge(runner)),
        other => finished(other),
    }
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
    assert!(ends_beside_900(&runner));
    let status = rig.ask(&runner, "status", None);
    let open = status["work_items"].as_array().unwrap();
    assert!(open.iter().all(|item| item["issue"] != 7), "{open:?}");
    assert_eq!(status["rulings"], json!([]));
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

// A project under `auto` with `git.issues` as `filing`, CI green on a
// reviewed pull request whose worker deferred `racy()`, the next step merging it
fn auto_merging_with_issues(filing: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.merge_auto();
    rig.issues(filing);
    let (rig, runner, head) = reviewed_in(rig, &[racy()], false);
    defer(&rig, &lines(&[racy()]));
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    (rig, runner)
}

#[test]
fn issues_ask_raises_the_follow_up_ruling_under_auto_merging_too() {
    let (rig, runner) = auto_merging_with_issues("ask");
    let Some(StepReport::Ruling { question, .. }) = after_merge(&runner) else {
        panic!("the maintainer was not asked about the findings");
    };
    assert_eq!(rig.forge.merges().len(), 1, "kelpie merged it itself");
    assert!(question.contains("src/lib.rs:9 looks racy"), "{question}");
    assert_eq!(rig.forge.created(), [], "nothing is filed before the yes");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["kind"], "follow-up");
}

#[test]
fn issues_skip_files_nothing_asks_nothing_and_the_work_item_finishes() {
    let (rig, runner) = auto_merging_with_issues("skip");
    assert!(finished(after_merge(&runner)));
    assert_eq!(rig.forge.merges().len(), 1);
    assert_eq!(rig.forge.created(), []);
    assert_eq!(rig.forge.comments(), []);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"], json!([]));
    assert_eq!(status["work_items"], json!([]));
}

#[test]
fn issues_file_files_at_once_under_ask_merging_too() {
    let rig = Rig::new("shep");
    rig.issues("file");
    let (rig, runner, head) = reviewed_in(rig, &[racy()], false);
    defer(&rig, &lines(&[racy()]));
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
    assert_eq!(step(&runner).unwrap(), filed(&[900], &[], 0));
    assert_eq!(rig.forge.merges().len(), 1);
    assert_eq!(rig.forge.created().len(), 1);
}
