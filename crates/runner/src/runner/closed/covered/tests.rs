use super::super::PARKED_READ;
use super::super::tests::{SENT_BACK, alert_texts, asked, asked_with_a_pull_request, reply};
use crate::ports::Checks;
use crate::runner::report::StepReport;
use crate::runner::step;
use crate::test::Rig;
use crate::test::ticket;
use serde_json::json;

// Whether `report` ends issue 7's work item with no change, its issue still open
fn stood_down(report: Option<StepReport>) -> bool {
    matches!(
        report,
        Some(StepReport::Finished {
            issue,
            pull_request: None,
            merged: false,
            closed: false,
            ..
        })
     if issue == ticket(7))
}

// A contributor's pull request 80, which closes issue 7
fn contributor_closes_7(rig: &Rig) {
    rig.forge.list_open_pull(80, "Fix seven", &[7]);
    rig.forge.set_author(80, "newcomer");
}

#[test]
fn a_turn_that_ends_with_no_pull_request_stands_down_for_a_contributors_that_closes_the_issue() {
    let rig = Rig::new("koji");
    rig.with_app();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    contributor_closes_7(&rig);
    rig.claude.script([reply()]);

    step(&runner).unwrap();
    assert!(stood_down(step(&runner).unwrap()));

    assert_eq!(rig.claude.calls().len(), 1, "the worker was sent back");
    let comments = rig.github.comments();
    let [(thread, notice)] = comments.as_slice() else {
        panic!("one comment expected: {comments:?}");
    };
    assert_eq!(*thread, 7);
    assert!(notice.contains("#80"), "{notice}");
}

#[test]
fn a_parked_item_stands_down_for_a_contributors_pull_request_that_closes_its_issue() {
    let (rig, runner) = asked("koji");
    contributor_closes_7(&rig);
    rig.clock.advance(PARKED_READ);

    assert!(stood_down(step(&runner).unwrap()));

    let said = "Pull request #80 closes issue #7, so its work item ends with no change. \
                Ruling 7 is withdrawn. Nothing to answer.";
    assert_eq!(alert_texts(&rig).last().map(String::as_str), Some(said));
    assert_eq!(rig.claude.calls().len(), 1);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        (&status["work_item"], &status["rulings"]),
        (&json!(null), &json!([]))
    );
    assert_ne!(
        status["history"][0]["closed"], true,
        "the issue is still open"
    );
}

#[test]
fn a_pull_request_that_only_mentions_the_issue_changes_nothing() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.list_open_pull(80, "Mentions seven", &[]);
    rig.forge.set_author(80, "newcomer");
    rig.claude.script([reply(), reply()]);

    step(&runner).unwrap();
    step(&runner).unwrap();

    let [_, nudged] = rig.claude.calls().try_into().unwrap();
    assert_eq!(nudged.prompt, SENT_BACK);
}

#[test]
fn a_pull_request_from_a_fork_covers_the_issue_whoever_opened_it() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.list_open_pull(80, "Fix seven", &[7]);
    rig.forge.set_from_fork(80);
    rig.claude.script([reply()]);

    step(&runner).unwrap();

    assert!(stood_down(step(&runner).unwrap()));
}

#[test]
fn a_merge_ruling_asks_which_pull_request_goes_ahead_when_a_contributors_closes_the_issue() {
    let (rig, runner, _) = Rig::with_pull_request("koji");
    contributor_closes_7(&rig);
    rig.forge
        .set_checks(&rig.forge.head_of("kelpie/7").unwrap(), Checks::Passed);

    let Some(StepReport::Ruling { id, question, .. }) = rig.verdict(&runner) else {
        panic!("the maintainer was not asked");
    };

    assert_eq!(id, ticket(71));
    assert!(
        question.contains("Pull request #80 also closes this issue"),
        "{question}"
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["pull_request"],
        71
    );
    assert!(rig.github.comments().is_empty());
}

#[test]
fn a_merge_ruling_is_raised_under_auto_when_a_contributors_pull_request_closes_the_issue() {
    let (rig, runner, _) = Rig::with_pull_request_set("koji", Rig::merge_auto);
    contributor_closes_7(&rig);
    rig.forge
        .set_checks(&rig.forge.head_of("kelpie/7").unwrap(), Checks::Passed);

    let report = rig.verdict(&runner);

    assert!(
        matches!(report, Some(StepReport::Ruling { .. })),
        "{report:?}"
    );
}

#[test]
fn a_parked_item_with_its_own_pull_request_is_left_to_its_ruling() {
    let (rig, runner) = asked_with_a_pull_request("koji");
    contributor_closes_7(&rig);
    rig.clock.advance(PARKED_READ);

    step(&runner).unwrap();
    rig.clock.advance(PARKED_READ);
    assert_eq!(step(&runner).unwrap(), None);

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["pull_request"], 71);
    assert_eq!(status["work_item"]["issue"], "7");
}

#[test]
fn a_parked_item_stays_for_a_pull_request_by_kelpies_own_account() {
    let (rig, runner) = asked("koji");
    rig.forge.open_pull_request(80, "kelpie/7-again", &[7]);
    rig.clock.advance(PARKED_READ);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], "7");
}

#[test]
fn a_pull_request_on_the_items_own_branch_covers_nothing() {
    let (rig, runner) = asked("koji");
    rig.forge.open_pull_request(80, "kelpie/7", &[7]);
    rig.clock.advance(PARKED_READ);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], "7");
}
