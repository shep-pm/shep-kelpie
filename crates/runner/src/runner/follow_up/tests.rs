//! Findings left unfixed at merge, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use crate::board::{READY, TRIAGE};
use crate::ports::{CLEAN, Checks, Finding, Severity};
use crate::runner::coderabbit::tests::{fixed, hold_a_finding, summoned};
use crate::runner::thread_replies::Settler;
use crate::runner::{Answer, CHECKS_SETTLE, Runner, StepReport, step};
use crate::test::{Rig, Scripted, ScriptedRound, ticket};
use crate::ticket::TicketId;

fn finding(file: &str, what: &str) -> Finding {
    Finding::new(
        Severity::High,
        file,
        9,
        what,
        "two threads write the same field",
    )
}

pub(in crate::runner) fn racy() -> Finding {
    finding("src/lib.rs", "looks racy")
}

// What the worker writes to defer `found`, in the findings file's own format
pub(in crate::runner) fn lines(found: &[Finding]) -> String {
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
    reviewed_pushing(rig, found, auto, "work\n")
}

// ... where the worker's first push writes `work` to `work.txt`
fn reviewed_pushing(
    rig: Rig,
    found: &[Finding],
    auto: bool,
    work: &'static str,
) -> (Rig, Mutex<Runner>, String) {
    if auto {
        rig.merge_auto();
        rig.issues("file");
    }
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    runner.lock().unwrap().plan_two_reads_for_the_rig();
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.reviewer
        .script([ScriptedRound::Findings(found.to_vec())]);
    rig.claude.script([
        Scripted::Push("work.txt", work),
        Scripted::Push("fixed.txt", "fixed\n"),
        Scripted::Text(CLEAN),
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
pub(in crate::runner) fn after_merge(runner: &Mutex<Runner>) -> Option<StepReport> {
    match step(runner).unwrap() {
        Some(StepReport::Noticed { .. }) => step(runner).unwrap(),
        other => other,
    }
}

fn filed(opened: &[u64], commented: &[u64], skipped: usize) -> Option<StepReport> {
    Some(StepReport::FollowUpsFiled {
        issue: ticket(7),
        pull_request: 71,
        opened: opened.iter().copied().map(ticket).collect(),
        commented: commented.iter().copied().map(ticket).collect(),
        skipped,
    })
}

fn finished(report: Option<StepReport>) -> bool {
    matches!(
        report,
        Some(StepReport::Finished {
            issue,
            pull_request: Some(71),
            merged: true,
            ..
        })
     if issue == ticket(7))
}

#[test]
fn under_file_an_unfixed_confirmed_finding_files_one_issue_for_triage() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.title, "looks racy");
    assert_eq!(issue.labels, [TRIAGE]);
    assert!(issue.body.contains("waits on triage"), "{}", issue.body);
    assert!(issue.body.contains("#71"), "{}", issue.body);
    assert!(issue.body.contains("`src/lib.rs:9`"), "{}", issue.body);
    assert!(
        issue.body.contains("two threads write the same field"),
        "{}",
        issue.body
    );
    assert!(
        finished(after_merge(&runner)),
        "#900 is not on the board, so nothing opens it"
    );
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    assert_eq!(rig.forge.created().len(), 1, "filed once, not again");
}

#[test]
fn with_an_app_the_follow_up_issue_and_the_label_it_needs_are_made_through_it() {
    let rig = Rig::new("shep");
    rig.with_app();
    let found = [racy()];
    let (rig, runner) = ready_in(rig, &found, &lines(&found));
    rig.forge.set_repo_labels(&["bug"]);

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));

    assert_eq!(
        rig.forge.created(),
        [],
        "not opened as the maintainer's login"
    );
    assert_eq!(rig.forge.repo_labels_now(), ["bug"]);
    let writes = rig.github.writes();
    let tokens: Vec<_> = writes.iter().map(|(token, _)| token.as_str()).collect();
    assert!(tokens.iter().all(|t| *t == "ghs_test1"), "{tokens:?}");
    let body = |call: &crate::github::Call| call.body.clone().unwrap_or_default();
    let made = |path: &str| {
        let at = format!("/repos/shep-pm/shep/{path}");
        (writes.iter())
            .filter(|(_, call)| call.path == at)
            .map(|(_, call)| body(call))
            .collect::<Vec<_>>()
    };
    let labels = made("labels");
    assert!(
        labels.last().unwrap().contains(r#""name":"needs-triage""#),
        "{labels:?}"
    );
    let [issue] = made("issues").try_into().unwrap();
    assert!(issue.contains(r#""labels":["needs-triage"]"#), "{issue}");
}

#[test]
fn with_no_app_the_follow_up_issue_is_opened_as_it_always_was() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));

    assert_eq!(rig.forge.created().len(), 1);
    assert_eq!(rig.github.writes(), []);
}

#[test]
fn a_repo_without_the_triage_label_gets_it_once_and_the_findings_are_filed() {
    let found = [racy(), finding("src/main.rs", "leaks a handle")];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    rig.forge.set_repo_labels(&["bug"]);

    assert_eq!(after_merge(&runner), filed(&[900, 901], &[], 0));
    assert_eq!(rig.forge.repo_labels_now(), ["bug", TRIAGE]);
    let labels: Vec<_> = rig.forge.created().into_iter().map(|i| i.labels).collect();
    assert_eq!(labels, [[TRIAGE], [TRIAGE]]);
}

#[test]
fn a_triage_label_the_repo_has_in_another_case_is_left_as_it_is() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    rig.forge.set_repo_labels(&["Needs-Triage"]);

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    assert_eq!(rig.forge.repo_labels_now(), ["Needs-Triage"]);
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

// Each `what` is a clause or two, as the reviewer prompt asks for
const WHATS: [(&str, &str); 5] = [
    (
        "Distinguish publication failures from directory-sync failures: On Unix, if the parent sync fails after the rename, the file is published.",
        "Distinguish publication failures from directory-sync failures",
    ),
    (
        "The issue's first acceptance criterion is not met. The diff never changes the Windows path.",
        "The issue's first acceptance criterion is not met",
    ),
    (
        "The issue says `Harness` is matched only in the facts table, the serde names and the docs; the diff also renames it.",
        "The issue says `Harness` is matched only in the facts table, the serde …",
    ),
    (
        "The issue is done only when a pi worker turn on a project with the code graph has run, which no test does.",
        "The issue is done only when a pi worker turn on a project with the …",
    ),
    (
        "The new Codex unreachable fixtures are explicitly written from recall rather than recorded from a run.",
        "The new Codex unreachable fixtures are explicitly written from recall …",
    ),
];

#[test]
fn a_title_is_the_findings_first_clause_cut_at_a_whole_word_within_72_characters() {
    let found: Vec<Finding> = (WHATS.iter().enumerate())
        .map(|(n, (what, _))| finding(&format!("src/f{n}.rs"), what))
        .collect();
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));

    assert_eq!(
        after_merge(&runner),
        filed(&[900, 901, 902, 903, 904], &[], 0)
    );
    let titles: Vec<String> = rig.forge.created().into_iter().map(|i| i.title).collect();
    assert_eq!(titles, WHATS.map(|(_, title)| title));
    assert!(titles.iter().all(|t| t.chars().count() <= 72), "{titles:?}");
}

#[test]
fn a_what_with_no_clause_break_over_72_characters_ends_on_a_whole_word_and_an_ellipsis() {
    let what = "Two threads write the same field without a lock so a reader may see half of it";
    let found = [finding("src/lib.rs", what)];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(
        issue.title,
        "Two threads write the same field without a lock so a reader may see …"
    );
    assert!(issue.body.contains(what), "the full what stays in the body");
}

#[test]
fn a_dotted_abbreviation_does_not_end_a_title() {
    let what =
        "Guard the shared map with a lock, e.g. a Mutex, so two workers cannot write it at once";
    let found = [finding("src/lib.rs", what)];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(
        issue.title,
        "Guard the shared map with a lock, e.g. a Mutex, so two workers cannot …"
    );
}

#[test]
fn a_finding_filed_before_under_its_80_character_title_is_not_filed_again() {
    let what = "Distinguish publication failures from directory-sync failures: On Unix, if the parent sync fails";
    let found = [finding("src/lib.rs", what)];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    let old_title: String = what.chars().take(80).collect();
    rig.forge
        .open_issue(61, &old_title, &format!("In `src/lib.rs`: {what}"));

    assert_eq!(after_merge(&runner), filed(&[], &[61], 0));
    assert_eq!(rig.forge.created(), []);
}

#[test]
fn the_same_finding_deferred_twice_files_once() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found).repeat(2));

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    assert_eq!(rig.forge.created().len(), 1);
}

// What the worker writes to dispute `found` with `reason`
fn disputed_lines(found: &[Finding], reason: &str) -> String {
    let line = |f: &Finding| format!("{}  {reason}\n", lines(std::slice::from_ref(f)));
    found.iter().map(line).collect()
}

#[test]
fn a_disputed_finding_is_not_filed_after_the_merge() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, "");
    std::fs::write(
        rig.build_7().join("disputed-findings.md"),
        disputed_lines(&found, "src/lib.rs:3 holds the lock"),
    )
    .unwrap();
    // Deferred as well, which counts as disputed.
    defer(&rig, &lines(&found));

    assert!(matches!(
        after_merge(&runner),
        Some(StepReport::Finished { merged: true, .. })
    ));
    assert_eq!(rig.forge.created(), []);
}

#[test]
fn a_dispute_with_no_reason_leaves_the_deferred_finding_to_be_filed() {
    let found = [racy()];
    let (rig, runner) = auto_ready_to_merge(&found, &lines(&found));
    std::fs::write(rig.build_7().join("disputed-findings.md"), lines(&found)).unwrap();

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
        Scripted::Text(CLEAN),
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

    let Some(StepReport::GateFailed { issue, reason }) = after_merge(&runner) else {
        panic!("the failure was not reported");
    };
    assert_eq!(issue, ticket(7));
    assert!(reason.contains("issues are down"), "{reason}");
    assert!(
        rig.ask(&runner, "status", None)["work_item"].is_object(),
        "the merged work item stays until its findings are filed"
    );

    rig.forge.set_issues_down(false);
    rig.next_look();
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

    assert!(matches!(
        after_merge(&runner),
        Some(StepReport::GateFailed { .. })
    ));
    for _ in 0..19 {
        assert_eq!(after_merge(&runner), None, "tried again inside its wait");
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
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.labels, [TRIAGE], "a yes to try again triages nothing");
    assert!(finished(after_merge(&runner)));
}

// Kelpie's own issue is its text, but a project's hook may refuse the finding
// it carries, so the maintainer is asked, naming the hook and its reason.
#[test]
fn a_finding_a_publish_hook_refuses_is_not_filed_and_the_maintainer_is_asked_naming_the_hook() {
    let rig = Rig::new("shep");
    rig.forbid_word(&["publish"], "racy");
    let found = [racy()];
    let (rig, runner) = ready_in(rig, &found, &lines(&found));

    let Some(StepReport::GateFailed { reason, .. }) = after_merge(&runner) else {
        panic!("the refused issue did not hold the work item");
    };
    assert!(
        reason.contains("the `publish` hook `forbid-word.sh` refused"),
        "{reason}"
    );
    rig.clock.advance(6 * 60 * 60);
    let Some(StepReport::Ruling { question, .. }) = after_merge(&runner) else {
        panic!("the maintainer was not asked after the window");
    };
    assert!(
        question.contains("the `publish` hook `forbid-word.sh` refused"),
        "{question}"
    );
    assert!(
        question.contains("it names a word the project keeps private"),
        "{question}"
    );
    assert_eq!(rig.forge.created(), [], "nothing is filed");
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

// Under `ask`: the worker parked on merge ruling 71, then a yes, then the merge
pub(in crate::runner) fn ask_after_the_merge(
    rig: Rig,
    found: &[Finding],
) -> (Rig, Mutex<Runner>, TicketId, String) {
    let (rig, runner, head) = reviewed_in(rig, found, false);
    defer(&rig, &lines(found));
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. }) if id == ticket(71)
    ));
    rig.ask(&runner, "rule", Some("71 yes"));
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
    rig.next_look();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Dispatched { issue, .. }) if issue == ticket(8)
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["parked"], json!(["7"]));
}

// Whether #7 finishes, once the board has opened #900, the issue just filed
// and ready: an item answered into its end holds no slot
fn ends_beside_900(runner: &Mutex<Runner>) -> bool {
    match after_merge(runner) {
        Some(StepReport::Dispatched { issue, .. }) if issue == ticket(900) => {
            finished(after_merge(runner))
        }
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

    rig.forge.set_repo_labels(&["bug", READY]);
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.labels, [READY], "the yes was its triage");
    assert!(!issue.body.contains("triage"), "{}", issue.body);
    let unmade = ["bug", READY];
    assert_eq!(rig.forge.repo_labels_now(), unmade, "no triage label made");
    assert!(ends_beside_900(&runner));
    let status = rig.ask(&runner, "status", None);
    let open = status["work_items"].as_array().unwrap();
    assert!(open.iter().all(|item| item["issue"] != "7"), "{open:?}");
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

// The project manager's yes to ruling `id`, asking for `needs-triage` if `unread`
fn pm_says_yes(runner: &Mutex<Runner>, id: TicketId, unread: bool) {
    let by = Settler::Pm {
        answer: "yes".into(),
        why: "the diff shows the race".into(),
        unread,
    };
    runner.lock().unwrap().rule_by(id, Answer::Yes, by).unwrap();
}

#[test]
fn under_ask_a_yes_from_the_project_manager_files_what_the_maintainers_yes_does() {
    let (rig, runner, id, _) = ask_after_the_merge(Rig::new("shep"), &[racy()]);
    rig.forge.set_repo_labels(&["bug", READY]);
    pm_says_yes(&runner, id, false);

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.labels, [READY], "the yes was its triage");
    assert!(!issue.body.contains("triage"), "{}", issue.body);
    assert_eq!(rig.forge.repo_labels_now(), ["bug", READY]);
}

#[test]
fn under_ask_a_yes_from_the_project_manager_for_needs_triage_files_them_unread() {
    let (rig, runner, id, _) = ask_after_the_merge(Rig::new("shep"), &[racy()]);
    pm_says_yes(&runner, id, true);

    assert_eq!(after_merge(&runner), filed(&[900], &[], 0));
    let [issue] = rig.forge.created().try_into().unwrap();
    assert_eq!(issue.labels, [TRIAGE]);
    assert!(
        issue.body.contains("Nobody has read this"),
        "{}",
        issue.body
    );
    assert!(rig.forge.repo_labels_now().contains(&TRIAGE.to_owned()));
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
    auto_merging_on(Rig::new("shep"), filing, "work\n")
}

// ... on `rig`, whose worker writes `work` to `work.txt`
pub(in crate::runner) fn auto_merging_on(
    rig: Rig,
    filing: &str,
    work: &'static str,
) -> (Rig, Mutex<Runner>) {
    rig.merge_auto();
    rig.issues(filing);
    let (rig, runner, head) = reviewed_pushing(rig, &[racy()], false, work);
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
        Some(StepReport::Ruling { id, .. }) if id == ticket(71)
    ));
    rig.ask(&runner, "rule", Some("71 yes"));
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::MarkedReady { .. })
    ));
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), filed(&[900], &[], 0));
    assert_eq!(rig.forge.merges().len(), 1);
    assert_eq!(rig.forge.created().len(), 1);
}
