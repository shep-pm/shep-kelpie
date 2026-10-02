use serde_json::json;

use std::time::Duration;

use crate::ports::{
    AgentCall, Checks, MaintainerReview, PullRequestState, ReviewComment, Role, Tools,
};
use crate::runner::{StepReport, step};
use crate::test::{Hold, Rig, Scripted};

const UNMET: &str = r#"{"criteria": [
    {"criterion": "A test where an unmet criterion sends the item back",
     "met": false, "where": "no test does"},
    {"criterion": "Runs on Opus by default", "met": true, "where": "src/settings.rs:151"}],
  "assumptions": []}"#;

fn audits(rig: &Rig) -> Vec<AgentCall> {
    let all = rig.claude.all_calls();
    all.into_iter()
        .filter(|c| c.role == Role::Auditor)
        .collect()
}

// What the check was given: its prompt, and the files the prompt sends it to,
// as they stand now
fn read_all(call: &AgentCall) -> String {
    let mut text = call.prompt.clone();
    for folder in &call.reach.read {
        let mut files: Vec<_> = std::fs::read_dir(folder).unwrap().flatten().collect();
        files.sort_by_key(|f| f.file_name());
        for file in files {
            text.push_str(&std::fs::read_to_string(file.path()).unwrap());
        }
    }
    text
}

fn last_audit_read(rig: &Rig) -> String {
    read_all(&audits(rig).pop().expect("the check ran"))
}

const FAKE_ONLY: &str = r#"{"criteria": [
    {"criterion": "Labels the pull request", "met": true, "where": "src/label.rs:9"}],
  "assumptions": [
    {"assumption": "The `worker:opus` label exists on the repo",
     "checked": false, "where": "only the fake forge, which accepts any label, is tested"},
    {"assumption": "`gh` prints JSON", "checked": true, "where": "tests/gh.rs:12"}]}"#;

fn the_item_is_sent_back(report: Option<StepReport>) -> Vec<String> {
    match report {
        Some(StepReport::AuditSentBack { gaps, .. }) => gaps,
        other => panic!("the check did not send the worker back: {other:?}"),
    }
}

// A gap's fix is reviewed like any other before CI: qwen's round, clean by
// default, then Claude's, which the caller scripts clean.
fn the_fix_is_reviewed(runner: &std::sync::Mutex<crate::runner::Runner>) {
    for round in ["qwen", "claude"] {
        let report = step(runner).unwrap();
        assert!(
            matches!(report, Some(StepReport::ReviewFindingsSent { held: 0, .. })),
            "the fix went to CI before {round}'s review round: {report:?}"
        );
    }
}

#[test]
fn an_assumption_only_a_fake_checks_sends_the_worker_back_naming_it() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.claude.script([
        Scripted::Audit(FAKE_ONLY),
        Scripted::Push("real.txt", "a real check\n"),
    ]);
    rig.forge.set_checks(&head, Checks::Passed);

    let gaps = the_item_is_sent_back(rig.verdict(&runner));
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));

    step(&runner).unwrap();
    let turns = rig.claude.calls();
    let fix = turns.last().expect("the worker was sent back");
    assert!(
        fix.prompt
            .contains("The `worker:opus` label exists on the repo"),
        "{}",
        fix.prompt
    );
    assert!(
        fix.prompt.contains("only the fake forge"),
        "the gap says what checks it: {}",
        fix.prompt
    );
    assert!(
        !fix.prompt.contains("`gh` prints JSON"),
        "an assumption a real check covers is not a gap"
    );
}

#[test]
fn under_auto_a_gap_stops_the_merge_until_the_worker_closes_it() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    drop(runner);
    rig.merge_auto();
    let runner = rig.open().unwrap();
    rig.claude.script([
        Scripted::Audit(UNMET),
        Scripted::Push("test.txt", "a test\n"),
        Scripted::Text("CLEAN"),
    ]);
    rig.forge.set_checks(&head, Checks::Passed);

    the_item_is_sent_back(rig.verdict(&runner));
    assert_eq!(rig.forge.merges(), []);
    assert!(
        rig.forge.readied().is_empty(),
        "it was not even marked ready"
    );

    step(&runner).unwrap(); // the worker closes the gap
    the_fix_is_reviewed(&runner);
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&fixed, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
}

#[test]
fn the_check_reads_the_issue_what_it_points_to_the_pull_request_and_the_diff() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge.set_issue_body(
        7,
        "Fix what #9 lists, and the review threads on #277.\n\n\
         ## Acceptance criteria\n\n- [ ] every item of #9 is fixed\n",
    );
    rig.forge.set_issue_body(
        9,
        "1. the first of three things\n2. the second\n3. the third\n",
    );
    rig.forge.open_pull_request(277, "kelpie/277", &[]);
    rig.forge.review(
        277,
        MaintainerReview {
            id: "r".into(),
            changes_requested: true,
            body: "Every item must also work with no network.".into(),
            comments: vec![ReviewComment {
                file: "src/gate.rs".into(),
                line: Some(40),
                body: "this label does not exist on the repo".into(),
            }],
        },
    );
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { .. })
    ));

    let [call] = audits(&rig).try_into().unwrap();
    let prompt = read_all(&call);
    assert!(
        call.prompt.contains("issue.md") && call.prompt.contains("diff.patch"),
        "the prompt sends the check to its files"
    );
    assert!(prompt.contains("every item of #9 is fixed"), "the issue");
    assert!(
        prompt.contains("Fix what #9 lists"),
        "the issue's whole body"
    );
    assert!(
        prompt.contains("the second\n3. the third"),
        "an issue it points to"
    );
    assert!(
        prompt.contains("this label does not exist on the repo"),
        "a pull request it points to, with its review comments"
    );
    assert!(
        prompt.contains("Every item must also work with no network."),
        "and the review's own body"
    );
    assert!(
        prompt.contains("Body of pull request #71."),
        "this pull request's body"
    );
    assert!(prompt.contains("+work"), "the final diff");
    assert!(prompt.contains("fake"), "it asks about fakes");
    assert_eq!(call.model, "claude-opus-5-5");
    assert_eq!(call.tools, Tools::Review, "it reads and runs nothing");
}

#[test]
fn an_unreadable_answer_is_a_failed_gate_and_the_next_step_asks_again() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.claude.script([Scripted::Audit("I think it is fine.")]);
    rig.forge.set_checks(&head, Checks::Passed);

    let Some(StepReport::GateFailed { reason, .. }) = rig.verdict(&runner) else {
        panic!("an answer that is not the check's JSON is no pass");
    };
    assert!(reason.contains("I think it is fine."), "{reason}");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::AuditPassed { .. })
    ));
    assert_eq!(audits(&rig).len(), 2);
}

#[test]
fn a_third_trip_to_the_worker_goes_to_the_maintainer_whose_yes_sends_it_once_more() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.claude.script([
        Scripted::Audit(UNMET),
        Scripted::Push("one.txt", "1\n"),
        Scripted::Text("CLEAN"),
        Scripted::Audit(UNMET),
        Scripted::Push("two.txt", "2\n"),
        Scripted::Text("CLEAN"),
        Scripted::Audit(UNMET),
    ]);
    let mut head = head;
    for round in 0..2 {
        rig.forge.set_checks(&head, Checks::Passed);
        the_item_is_sent_back(rig.verdict(&runner));
        step(&runner).unwrap(); // the worker's turn
        the_fix_is_reviewed(&runner);
        head = rig.forge.head_of("kelpie/7").unwrap();
        let turns = rig.claude.calls();
        let worker = turns.iter().filter(|c| c.role == Role::Worker).count();
        assert_eq!(worker, 2 + round, "one turn each");
    }
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { id, question, .. }) = rig.verdict(&runner) else {
        panic!("the third gap is the maintainer's");
    };
    assert!(question.contains("no test does"), "{question}");
    assert!(question.contains("by hand"), "{question}");

    rig.claude
        .script([Scripted::Push("three.txt", "3\n"), Scripted::Text("CLEAN")]);
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    step(&runner).unwrap();
    let turns = rig.claude.calls();
    let again = turns.last().unwrap();
    assert!(again.prompt.contains("no test does"), "{}", again.prompt);
    // The yes's fix is reviewed too.
    the_fix_is_reviewed(&runner);
}

#[test]
fn a_head_that_passes_gives_a_later_head_its_trips_to_the_worker_again() {
    let (rig, runner, mut head) = Rig::with_pull_request("shep");
    rig.claude.script([
        Scripted::Audit(UNMET),
        Scripted::Push("one.txt", "1\n"),
        Scripted::Text("CLEAN"),
        Scripted::Audit(UNMET),
        Scripted::Push("two.txt", "2\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..2 {
        rig.forge.set_checks(&head, Checks::Passed);
        the_item_is_sent_back(rig.verdict(&runner));
        step(&runner).unwrap(); // the worker's turn
        the_fix_is_reviewed(&runner);
        head = rig.forge.head_of("kelpie/7").unwrap();
    }
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { id, .. }) = rig.verdict(&runner) else {
        panic!("a head with nothing wrong goes to the merge ruling");
    };

    // The maintainer's no brings a new head through the review loop and CI.
    rig.claude.script([
        Scripted::Push("three.txt", "3\n"),
        Scripted::Text("CLEAN"),
        Scripted::Audit(UNMET),
    ]);
    rig.ask(&runner, "rule", Some(&format!("{id} no one more thing")));
    step(&runner).unwrap(); // the worker's turn
    step(&runner).unwrap(); // review round 1, qwen: clean by default
    step(&runner).unwrap(); // review round 2, claude: clean
    head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    the_item_is_sent_back(rig.verdict(&runner));
}

#[test]
fn a_pull_request_merged_while_the_check_ran_is_left_to_the_gate() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "CI waits for its checks to settle"
    );
    rig.clock.advance(crate::runner::CHECKS_SETTLE);
    let hold = Hold::default();
    rig.claude
        .script([Scripted::HoldAudit(hold.clone(), UNMET)]);

    std::thread::scope(|scope| {
        let call = scope.spawn(|| step(&runner));
        assert!(
            hold.entered(Duration::from_secs(10)),
            "the check never began"
        );
        // The maintainer merges it by hand while the check reads.
        rig.forge.set_state(71, PullRequestState::Merged);
        hold.release();
        assert_eq!(
            call.join().unwrap().unwrap(),
            None,
            "a stale answer is dropped"
        );
    });
    assert_eq!(
        rig.claude.calls().len(),
        1,
        "no worker turn on a merged pull request"
    );

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Finished { merged: true, .. })
    ));
}

#[test]
fn a_head_pushed_while_the_check_ran_drops_its_answer() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "CI waits for its checks to settle"
    );
    rig.clock.advance(crate::runner::CHECKS_SETTLE);
    let hold = Hold::default();
    rig.claude
        .script([Scripted::HoldAudit(hold.clone(), crate::test::AUDIT_PASSES)]);

    std::thread::scope(|scope| {
        let call = scope.spawn(|| step(&runner));
        assert!(
            hold.entered(Duration::from_secs(10)),
            "the check never began"
        );
        rig.push_by_hand("kelpie/7", "late.txt");
        hold.release();
        assert_eq!(
            call.join().unwrap().unwrap(),
            None,
            "a stale answer is dropped"
        );
    });
    let moved = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(moved, head);

    // The gate takes the head from there: a push it did not make is the
    // maintainer's to rule on, and no merge ruling on a head nothing checked.
    rig.forge.set_checks(&moved, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { .. })
    ));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["kind"]["kind"], "foreign-change");
    assert_eq!(
        audits(&rig).len(),
        1,
        "the old head's answer passed nothing"
    );
}

#[test]
fn a_long_issue_is_read_whole_so_no_criterion_goes_unseen() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    let long = format!(
        "## Acceptance criteria\n\n{}- [ ] the very last criterion\n",
        "- [ ] a criterion, one of very many\n".repeat(2_000)
    );
    assert!(long.len() > 60_000);
    rig.forge.set_issue_body(7, &long);
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { .. })
    ));
    let [call] = audits(&rig).try_into().unwrap();
    let seen = read_all(&call);
    assert!(seen.contains("the very last criterion"));
    assert!(!seen.contains("left out"));
    // A harness passes the prompt as one argument, which a long issue and a
    // diff would overflow, so the prompt itself stays short.
    assert!(call.prompt.len() < 10_000, "{}", call.prompt.len());
}

const NO_EVIDENCE: &str = r#"{"criteria": [
    {"criterion": "Runs on Opus by default", "met": true},
    {"criterion": "Has a test", "met": true, "where": "  "}],
  "assumptions": [
    {"assumption": "The label exists", "checked": true, "where": ""}]}"#;

#[test]
fn a_success_that_names_no_evidence_is_a_gap_like_a_failure() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.claude.script([Scripted::Audit(NO_EVIDENCE)]);
    rig.forge.set_checks(&head, Checks::Passed);

    let gaps = the_item_is_sent_back(rig.verdict(&runner));
    assert_eq!(gaps.len(), 3, "{gaps:?}");
    assert!(gaps[0].contains("Runs on Opus by default"), "{gaps:?}");
    assert!(gaps[2].contains("The label exists"), "{gaps:?}");
}

#[test]
fn every_item_the_issue_points_to_is_pulled_in_however_many() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    let refs: Vec<String> = (100..112).map(|n| format!("#{n}")).collect();
    rig.forge
        .set_issue_body(7, &format!("See {}.", refs.join(", ")));
    rig.forge
        .set_issue_body(111, "the twelfth one asks for a thing");
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { .. })
    ));
    assert!(last_audit_read(&rig).contains("the twelfth one asks for a thing"));
}

#[test]
fn an_answer_with_no_criterion_for_an_issue_that_has_some_checked_nothing() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.claude
        .script([Scripted::Audit(r#"{"criteria": [], "assumptions": []}"#)]);
    rig.forge.set_checks(&head, Checks::Passed);

    let Some(StepReport::GateFailed { reason, .. }) = rig.verdict(&runner) else {
        panic!("an empty list of criteria is no pass");
    };
    assert!(reason.contains("no criterion"), "{reason}");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));

    // The next step asks again, and a real answer passes.
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::AuditPassed { .. })
    ));
    assert_eq!(audits(&rig).len(), 2);
}

#[test]
fn an_issue_edited_after_a_pass_is_checked_again_on_the_same_head() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "CI waits for its checks to settle"
    );
    rig.clock.advance(crate::runner::CHECKS_SETTLE);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::AuditPassed { .. })
    ));
    assert!(!last_audit_read(&rig).contains("a new one"));

    // The issue gains a criterion before the ruling is raised.
    rig.forge
        .set_issue_body(7, "## Acceptance criteria\n\n- [ ] a new one\n");
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::AuditPassed { .. })
    ));
    assert!(last_audit_read(&rig).contains("a new one"));

    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Ruling { .. })
    ));
    assert_eq!(
        audits(&rig).len(),
        2,
        "unchanged since, so not checked again"
    );
}

#[test]
fn an_issue_edited_while_the_check_ran_drops_its_answer() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(
        step(&runner).unwrap(),
        None,
        "CI waits for its checks to settle"
    );
    rig.clock.advance(crate::runner::CHECKS_SETTLE);
    let hold = Hold::default();
    rig.claude
        .script([Scripted::HoldAudit(hold.clone(), crate::test::AUDIT_PASSES)]);

    std::thread::scope(|scope| {
        let call = scope.spawn(|| step(&runner));
        assert!(
            hold.entered(Duration::from_secs(10)),
            "the check never began"
        );
        rig.forge
            .set_issue_body(7, "## Acceptance criteria\n\n- [ ] added meanwhile\n");
        hold.release();
        assert_eq!(
            call.join().unwrap().unwrap(),
            None,
            "a stale answer is dropped"
        );
    });

    // Nothing passed, so the next step checks what the issue says now.
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::AuditPassed { .. })
    ));
    assert!(last_audit_read(&rig).contains("added meanwhile"));
}

#[test]
fn an_unmet_criterion_sends_the_worker_back_with_the_gap_named() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.claude.script([
        Scripted::Audit(UNMET),
        Scripted::Push("test.txt", "a test\n"),
        Scripted::Text("CLEAN"),
    ]);
    rig.forge.set_checks(&head, Checks::Passed);

    let Some(StepReport::AuditSentBack {
        issue,
        pull_request,
        gaps,
        ..
    }) = rig.verdict(&runner)
    else {
        panic!("the check did not send the worker back");
    };
    assert_eq!((issue, pull_request), (7, 71));
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));

    step(&runner).unwrap(); // the worker's turn on the gap
    let turns = rig.claude.calls();
    let fix = turns.last().expect("the worker was sent back");
    assert!(
        fix.prompt
            .contains("A test where an unmet criterion sends the item back"),
        "{}",
        fix.prompt
    );
    assert!(fix.prompt.contains("no test does"), "{}", fix.prompt);
    assert!(
        !fix.prompt.contains("Runs on Opus by default"),
        "a criterion that is met is not a gap"
    );

    // The fix goes through the review loop and CI, and the check looks at
    // the head it left.
    the_fix_is_reviewed(&runner);
    let fixed = rig.forge.head_of("kelpie/7").unwrap();
    assert_ne!(fixed, head);
    rig.forge.set_checks(&fixed, Checks::Passed);
    let Some(StepReport::Ruling { question, .. }) = rig.verdict(&runner) else {
        panic!("a head with nothing wrong goes to the merge ruling");
    };
    assert!(question.contains(&fixed[..7]), "{question}");
    assert_eq!(audits(&rig).len(), 2, "once for each head");
}

#[test]
fn an_issue_number_with_letters_stuck_to_it_is_no_reference() {
    let body = "Fix #9, then #12. See C#4, #123abc, #7_x, #9 again, #8) and own #7 and #71.";
    assert_eq!(super::pointed_at(body, &[7, 71]), [9, 12, 8]);
}

#[test]
fn the_check_is_read_past_a_brace_in_the_prose_before_it() {
    let text = "Every {met} criterion is listed.\n{\"criteria\": [], \"assumptions\": []}";
    let read = super::read_findings(text).unwrap();
    assert!(read.gaps().is_empty());
    assert!(super::read_findings("no {json} here").is_err());
}

// Under a pacing hold, a head the check never passed asks the forge for
// nothing: the hold is known before any of the check's reading. An issue the
// forge cannot find stands in for that reading, which would fail the gate.
#[test]
fn a_held_check_reads_nothing_from_the_forge_first() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.forge.set_checks(&head, Checks::Passed);
    rig.forge.remove_issue(7);
    rig.meter.set(Rig::utilization(0, 55));
    let report = rig.verdict(&runner);
    assert!(
        matches!(report, Some(StepReport::Held { .. })),
        "the check read the forge before the hold: {report:?}"
    );
}
