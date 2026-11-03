//! The disputes a worker makes of a finding, through the runner's work-item loop

use std::sync::Mutex;

use serde_json::json;

use crate::github::ApiError;
use crate::ports::{AgentError, Checks, Finding, Severity};
use crate::runner::{Runner, StepReport, step};
use crate::settings::Harness;
use crate::test::{Rig, Scripted, ScriptedRound, ticket};

// Round 1 on pull request 71 found one MEDIUM finding, and its fix turn is next.
fn found() -> Finding {
    Finding::new(
        Severity::Medium,
        "src/lib.rs",
        3,
        "the closure destructures a three-tuple",
        "it reads the zip as three values",
    )
}

const FOUND_LINE: &str =
    "MEDIUM|src/lib.rs:3|the closure destructures a three-tuple|it reads the zip as three values\n";

fn findings_sent() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found()])]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the findings go out: the fix turn is next
    (rig, runner)
}

fn dispute(rig: &Rig, text: &str) {
    std::fs::write(rig.build_7().join("disputed-findings.md"), text).unwrap();
}

#[test]
fn a_fix_turn_that_disputes_every_finding_with_a_reason_goes_on_to_the_next_reviewer() {
    let (rig, runner) = findings_sent();
    dispute(
        &rig,
        &format!(
            "{FOUND_LINE}  src/runner/review.rs:312 destructures the two-tuple and builds the three-tuple for the next map\n"
        ),
    );
    rig.claude
        .script([Scripted::Say("The finding reads the closure wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FindingsDisputed {
            issue: ticket(7),
            pull_request: 71,
            round: 1,
            disputed: 1,
            deferred: 0,
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"], json!([]));
    assert_eq!(
        status["work_item"]["phase"],
        json!({
            "state": "review",
            "round": 2,
            "stage": { "stage": "round" },
            "ran": ["qwen"],
        })
    );
}

const REASON: &str = "  src/runner/review.rs:312 destructures the two-tuple and builds the three-tuple for the next map\n";

#[test]
fn a_dispute_with_no_reason_line_parks_on_fix_not_pushed() {
    let (rig, runner) = findings_sent();
    dispute(&rig, FOUND_LINE);
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "fix-not-pushed");
}

#[test]
fn a_turn_that_defers_some_findings_and_disputes_the_rest_goes_on() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    let other = Finding::new(
        Severity::High,
        "src/lib.rs",
        9,
        "another thing",
        "it is wrong",
    );
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found(), other])]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the findings go out: the fix turn is next
    let deferred = "HIGH|src/lib.rs:9|another thing|it is wrong\n";
    std::fs::write(rig.build_7().join("deferred-findings.md"), deferred).unwrap();
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude
        .script([Scripted::Say("One is wrong, one is out of scope.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::FindingsDisputed {
            issue: ticket(7),
            pull_request: 71,
            round: 1,
            disputed: 1,
            deferred: 1,
        })
    );
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

#[test]
fn a_turn_that_disputes_one_finding_and_leaves_another_parks_and_the_question_names_it() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    let other = Finding::new(
        Severity::High,
        "src/lib.rs",
        9,
        "another thing",
        "it is wrong",
    );
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found(), other])]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the findings go out: the fix turn is next
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("One is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed

    let Some(StepReport::Ruling { question, .. }) = step(&runner).unwrap() else {
        panic!("a finding left unanswered parks the item");
    };

    assert!(
        question.contains("disputed `src/lib.rs:3`"),
        "the dispute: {question}"
    );
    assert!(
        question.contains("left `src/lib.rs:9`"),
        "the finding left: {question}"
    );
}

#[test]
fn a_disputed_finding_whose_attached_test_still_fails_parks() {
    let rig = Rig::new("shep");
    rig.test_command("cargo test --test {name}");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // the worker's first turn
    step(&runner).unwrap(); // round 1, qwen: clean by default
    let failed = |output: &str| crate::ports::TestOutcome::Failed {
        output: output.to_owned(),
    };
    rig.tests.script([failed("before"), failed("after")]);
    let line = "MEDIUM|src/lib.rs:3|the flag is read first|a caller sees nothing|tests/flag.rs";
    rig.claude.script([Scripted::Attach(
        "tests/flag.rs",
        "#[test]\nfn flag_is_read() { assert!(false); }\n",
        line,
    )]);
    step(&runner).unwrap(); // round 2, claude
    step(&runner).unwrap(); // the findings sent
    rig.claude.script([Scripted::Push("fixed.txt", "fixed\n")]);
    step(&runner).unwrap(); // the worker's fix turn
    step(&runner).unwrap(); // the test fails still: the worker is sent back to it

    // The second fix turn pushes nothing and says the finding is wrong.
    dispute(&rig, &format!("{line}\n{REASON}"));
    rig.claude.script([Scripted::Say("The test is wrong.")]);
    step(&runner).unwrap();
    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "fix-not-pushed");
}

#[test]
fn the_next_reviewer_is_shown_each_disputed_finding_and_its_reason() {
    let (rig, runner) = findings_sent();
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken

    rig.claude.script([Scripted::Text(crate::ports::CLEAN)]);
    step(&runner).unwrap(); // round 2, claude
    let prompt = rig.claude.all_calls().pop().unwrap().prompt;

    assert!(
        prompt.contains(FOUND_LINE.trim_end()),
        "the finding as it was raised: {prompt}"
    );
    assert!(
        prompt.contains(REASON.trim()),
        "the worker's reason: {prompt}"
    );
    assert!(
        prompt.contains("only if the reason is wrong"),
        "the ask: {prompt}"
    );
}

#[test]
fn a_finding_disputed_a_second_time_parks_on_fix_not_pushed() {
    let (rig, runner) = findings_sent();
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken
    // The next reviewer reads the reason and raises the finding again.
    rig.claude.script([Scripted::Text(FOUND_LINE)]);
    step(&runner).unwrap(); // round 2, claude
    step(&runner).unwrap(); // the finding goes to the worker again
    dispute(
        &rig,
        &format!("{FOUND_LINE}{REASON}{FOUND_LINE}  I ran it again and it still holds.\n"),
    );
    rig.claude.script([Scripted::Say("Still wrong.")]);
    step(&runner).unwrap(); // the second fix turn ends with nothing pushed

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "fix-not-pushed");
}

#[cfg(unix)]
#[test]
fn a_dispute_file_that_links_to_another_file_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let (build, theirs) = (dir.path().join("build"), dir.path().join("notes.md"));
    std::fs::create_dir(&build).unwrap();
    std::fs::write(&theirs, format!("{FOUND_LINE}{REASON}")).unwrap();
    std::os::unix::fs::symlink(&theirs, super::findings::disputed_path(&build)).unwrap();

    let err = super::findings::disputed(&build).unwrap_err();

    assert!(err.contains("disputed-findings.md"), "{err}");
    assert!(err.contains("not a regular file"), "{err}");
    assert!(!err.contains("closure"), "{err}");
}

#[test]
fn with_no_reviewer_after_it_the_disputes_go_on_the_pull_request_as_one_comment() {
    let rig = Rig::new("shep");
    rig.with_app();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.claude.script([Scripted::Text(FOUND_LINE)]);
    step(&runner).unwrap(); // round 2, claude, the last reviewer, finds it
    step(&runner).unwrap(); // the finding goes to the worker
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken: no reviewer is left

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
    rig.ci_on_now(&runner);
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("a merge ruling is raised on green CI");
    };

    let comments = rig.github.comments();
    let disputes: Vec<_> = comments
        .iter()
        .filter(|(_, text)| text.contains("disputed"))
        .collect();
    let [(thread, text)] = disputes.as_slice() else {
        panic!("one comment for the disputes: {comments:?}");
    };
    assert_eq!(*thread, 71);
    assert!(text.contains("claude"), "who raised it: {text}");
    assert!(
        text.contains("`src/lib.rs:3`: the closure destructures a three-tuple"),
        "the finding: {text}"
    );
    assert!(text.contains(REASON.trim()), "the worker's reason: {text}");
    assert!(
        question.contains("disputed"),
        "the ruling names it: {question}"
    );
    assert!(question.contains("comment"), "{question}");
}

// Pull request 71 with the App covering the repo, round 1 (qwen) about to read it.
fn at_round_1_with_app() -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.with_app();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    (rig, runner)
}

// Green CI on the head, and the merge ruling it raises
fn merge_question(rig: &Rig, runner: &Mutex<Runner>) -> String {
    rig.ci_on_now(runner);
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(runner) else {
        panic!("a merge ruling is raised on green CI");
    };
    question
}

#[test]
fn a_reviewer_that_fails_after_being_shown_the_disputes_leaves_them_for_the_pull_request() {
    let (rig, runner) = at_round_1_with_app();
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found()])]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the findings go out: the fix turn is next
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken: claude reads next
    let down = AgentError::Failed(Harness::ClaudeCode, "overloaded".into());
    rig.claude
        .script((0..3).map(|_| Scripted::Fail(down.clone())));
    for _ in 0..3 {
        step(&runner).unwrap(); // claude's round fails, and is passed over on the third
    }
    // The bots are not listed, so claude was the pass's last reviewer.
    rig.reviewers(&["qwen", "claude"]);
    rig.allow(&["sonnet-high", "qwen", "claude"]);
    let (settings, kelpie) = (rig.settings(), rig.kelpie_settings());
    runner.lock().unwrap().reread(settings, kelpie).unwrap();
    step(&runner).unwrap(); // one of two read, so the pass ends marked unreviewed

    let question = merge_question(&rig, &runner);

    let comments = rig.github.comments();
    assert!(
        comments.iter().any(|(_, text)| text.contains("disputed")),
        "the disputes no reviewer read are posted: {comments:?}"
    );
    assert!(question.contains("disputed"), "{question}");
}

#[test]
fn the_merge_ruling_counts_only_the_disputes_no_reviewer_read() {
    let (rig, runner) = at_round_1_with_app();
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found()])]);
    step(&runner).unwrap(); // round 1's qwen call
    step(&runner).unwrap(); // the findings go out: the fix turn is next
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken: claude reads next
    let other = "HIGH|src/lib.rs:9|another thing|it is wrong\n";
    rig.claude.script([Scripted::Text(other)]);
    step(&runner).unwrap(); // round 2, claude, the last reviewer, finds another
    step(&runner).unwrap(); // the finding goes to the worker
    dispute(&rig, &format!("{other}{REASON}"));
    rig.claude.script([Scripted::Say("Wrong too.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken: no reviewer is left

    let question = merge_question(&rig, &runner);

    assert!(
        question.contains("disputed 1 finding that"),
        "only the dispute no reviewer read: {question}"
    );
}

#[test]
fn a_finding_disputed_a_second_time_and_deferred_still_parks_on_fix_not_pushed() {
    let (rig, runner) = findings_sent();
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken
    rig.claude.script([Scripted::Text(FOUND_LINE)]);
    step(&runner).unwrap(); // round 2, claude raises it again
    step(&runner).unwrap(); // the finding goes to the worker again
    dispute(
        &rig,
        &format!("{FOUND_LINE}{REASON}{FOUND_LINE}  I ran it again and it still holds.\n"),
    );
    std::fs::write(rig.build_7().join("deferred-findings.md"), FOUND_LINE).unwrap();
    rig.claude.script([Scripted::Say("Still wrong.")]);
    step(&runner).unwrap(); // the second fix turn ends with nothing pushed

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "fix-not-pushed");
}

// Round 1 found `found()`; the worker's fix turn pushes and disputes it too, as the
// instructions let it; round 2's reader is about to read the head it pushed.
fn disputed_in_a_fix_that_pushed() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = findings_sent();
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Push("fixed.txt", "fixed\n")]);
    step(&runner).unwrap(); // the fix turn pushes
    step(&runner).unwrap(); // the fix turn ends: round 2 is next
    (rig, runner)
}

#[test]
fn a_dispute_made_in_a_fix_that_pushed_is_shown_to_the_next_reviewer() {
    let (rig, runner) = disputed_in_a_fix_that_pushed();

    rig.claude.script([Scripted::Text(crate::ports::CLEAN)]);
    step(&runner).unwrap(); // round 2, claude
    let prompt = rig.claude.all_calls().pop().unwrap().prompt;

    assert!(
        prompt.contains(REASON.trim()),
        "the worker's reason: {prompt}"
    );
}

#[test]
fn a_dispute_a_pushing_fix_made_is_not_an_answer_to_the_next_round() {
    let (rig, runner) = disputed_in_a_fix_that_pushed();
    rig.claude.script([Scripted::Text(FOUND_LINE)]);
    step(&runner).unwrap(); // round 2, claude raises it again
    step(&runner).unwrap(); // the finding goes to the worker again
    rig.claude.script([Scripted::Say("Nothing to add.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed, the files untouched

    let report = step(&runner).unwrap();

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["reason"], "fix-not-pushed");
}

#[test]
fn disputes_that_cannot_be_posted_hold_an_auto_merge() {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([Scripted::Push("work.txt", "work\n")]);
    step(&runner).unwrap(); // opens the pull request, enters round 1 (qwen)
    step(&runner).unwrap(); // round 1, qwen: clean by default
    rig.claude.script([Scripted::Text(FOUND_LINE)]);
    step(&runner).unwrap(); // round 2, claude, the last reviewer, finds it
    step(&runner).unwrap(); // the finding goes to the worker
    dispute(&rig, &format!("{FOUND_LINE}{REASON}"));
    rig.claude.script([Scripted::Say("The finding is wrong.")]);
    step(&runner).unwrap(); // the fix turn ends with nothing pushed
    step(&runner).unwrap(); // the dispute is taken: no reviewer is left
    rig.github.refuse_comments(ApiError::Refused(502));

    rig.ci_on_now(&runner);
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    let report = rig.verdict(&runner);

    assert!(
        matches!(report, Some(StepReport::GateFailed { .. })),
        "{report:?}"
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["phase"]["state"], "ci");
}
