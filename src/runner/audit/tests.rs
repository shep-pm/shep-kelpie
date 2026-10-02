use serde_json::json;

use crate::ports::{AgentCall, Checks, MaintainerReview, ReviewComment, Role, Tools};
use crate::runner::{StepReport, step};
use crate::test::{Rig, Scripted};

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
    ]);
    rig.forge.set_checks(&head, Checks::Passed);

    the_item_is_sent_back(rig.verdict(&runner));
    assert_eq!(rig.forge.merges(), []);
    assert!(
        rig.forge.readied().is_empty(),
        "it was not even marked ready"
    );

    step(&runner).unwrap(); // the worker closes the gap
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
    rig.forge.remove_issue(277);
    rig.forge.open_pull_request(277, "kelpie/277", &[]);
    rig.forge.review(
        277,
        MaintainerReview {
            id: "r".into(),
            changes_requested: true,
            body: String::new(),
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
    let prompt = &call.prompt;
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
        Scripted::Audit(UNMET),
        Scripted::Push("two.txt", "2\n"),
        Scripted::Audit(UNMET),
    ]);
    let mut head = head;
    for round in 0..2 {
        rig.forge.set_checks(&head, Checks::Passed);
        the_item_is_sent_back(rig.verdict(&runner));
        step(&runner).unwrap(); // the worker's turn
        head = rig.forge.head_of("kelpie/7").unwrap();
        assert_eq!(rig.claude.calls().len(), 2 + round, "one turn each");
    }
    rig.forge.set_checks(&head, Checks::Passed);
    let Some(StepReport::Ruling { id, question, .. }) = rig.verdict(&runner) else {
        panic!("the third gap is the maintainer's");
    };
    assert!(question.contains("no test does"), "{question}");
    assert!(question.contains("by hand"), "{question}");

    rig.claude.script([Scripted::Push("three.txt", "3\n")]);
    rig.ask(&runner, "rule", Some(&format!("{id} yes")));
    step(&runner).unwrap();
    let turns = rig.claude.calls();
    let again = turns.last().unwrap();
    assert!(again.prompt.contains("no test does"), "{}", again.prompt);
}

#[test]
fn an_unmet_criterion_sends_the_worker_back_with_the_gap_named() {
    let (rig, runner, head) = Rig::with_pull_request("shep");
    rig.claude.script([
        Scripted::Audit(UNMET),
        Scripted::Push("test.txt", "a test\n"),
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

    // The fix goes through CI, and the check looks at the head it left.
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
