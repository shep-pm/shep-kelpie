use std::sync::Mutex;

use serde_json::json;

use super::super::*;
use super::ruling_report;
use crate::ports::{Finding, Severity};
use crate::state::Ruling;
use crate::test::{Rig, Scripted, ticket};
use crate::work_item::Dispute;

// The worker's first turn merges `kelpie/9`, the branch of open pull request 72, into its own, as
// the worker for #1178 merged `kelpie/1174`. `setup` changes the rig's settings.
fn borrowing(project: &str, setup: impl FnOnce(&Rig)) -> (Rig, Mutex<Runner>, String) {
    let borrow = |rig: &Rig| {
        rig.push_by_hand("kelpie/9", "borrowed.txt");
        rig.forge.open_pull_request(72, "kelpie/9", &[9]);
        setup(rig);
    };
    let work = Scripted::PushMerging("kelpie/9", "work.txt", "work\n");
    Rig::with_pull_request_worked(project, borrow, work)
}

// Pull request 72's work item, as kelpie kept it: parked on a ruling, with one finding its worker
// was sent to fix
fn keeping_72(runner: &Mutex<Runner>) {
    let mut runner = runner.lock().unwrap();
    let mut other = runner.state.work_items[0].clone();
    other.issue = ticket(9);
    other.branch = "kelpie/9".to_owned();
    other.pull_request = Some(72);
    other.phase = Phase::Ruling { id: ticket(72) };
    other.held = vec![Finding::new(
        Severity::High,
        "src/lib.rs",
        12,
        "loses a write",
        "two callers race",
    )];
    runner.state.work_items.push(other);
    let parked = Ruling {
        issue: Some(ticket(9)),
        pull_request: Some(72),
        alerted: true,
        ..Ruling::new(
            ticket(72),
            "Is it closed?",
            RulingKind::Stuck(Stuck::Closed),
        )
    };
    runner.state.rulings.push(parked);
}

#[test]
fn a_head_carrying_another_open_pull_requests_commits_gets_a_merge_ruling_naming_it() {
    let (rig, runner, head) = borrowing("shep", |_| {});
    keeping_72(&runner);
    rig.forge.set_checks(&head, Checks::Passed);

    let (id, question) = ruling_report(rig.verdict(&runner));

    assert_eq!(id, ticket(71));
    assert_eq!(
        question,
        format!(
            "Merge pull request #71 at {} into main? `shep kelpie rule 71 yes` merges it, \
             `shep kelpie rule 71 no <note>` sends the worker your note for a fix that goes to CI \
             and back to you, and `shep kelpie rule 71 rework <note>` sends it your note for a \
             change the whole review reads again.\n\
             This head carries commits of other open pull requests, so a yes merges those \
             into main too. #72 (a draft): kelpie sent its worker to fix HIGH src/lib.rs:12. \
             The review on the pull request may hold more.",
            &head[..7]
        )
    );
    let status = rig.ask(&runner, "status", None);
    let kept = status["rulings"].as_array().unwrap();
    let kept = kept.iter().find(|r| r["id"] == "71");
    assert_eq!(kept.unwrap()["question"], json!(question));
}

#[test]
fn a_borrowed_pull_request_no_work_item_holds_says_kelpie_has_no_record_of_it() {
    let (rig, runner, head) = borrowing("shep", |_| {});
    rig.forge.set_checks(&head, Checks::Passed);

    let (_, question) = ruling_report(rig.verdict(&runner));

    assert!(
        question.ends_with(
            "#72 (a draft): kelpie holds no record of it. Read its review on the pull request."
        ),
        "{question}"
    );
}

#[test]
fn a_borrowing_head_gets_the_merge_ruling_under_auto_too() {
    let (rig, runner, head) = borrowing("shep", Rig::merge_auto);
    rig.forge.set_checks(&head, Checks::Passed);

    let (id, question) = ruling_report(rig.verdict(&runner));

    assert_eq!(id, ticket(71));
    assert!(question.contains("#72 (a draft)"), "{question}");
}

#[test]
fn a_borrowing_head_s_ruling_keeps_the_count_of_the_disputes_just_posted() {
    let (rig, runner, head) = borrowing("shep", |_| {});
    rig.forge.set_checks(&head, Checks::Passed);
    {
        let mut runner = runner.lock().unwrap();
        let finding = Finding::new(Severity::Low, "src/lib.rs", 3, "a nit", "it reads wrong");
        runner.state.work_items[0].disputes = vec![Dispute {
            finding,
            reason: "src/lib.rs:3 is as the spec says".to_owned(),
            raised_by: "reader".to_owned(),
            round: 1,
            shown: false,
        }];
    }

    let (_, question) = ruling_report(rig.verdict(&runner));

    assert!(
        question.contains("The worker disputed 1 finding that no reviewer read again"),
        "{question}"
    );
    assert!(question.contains("#72 (a draft)"), "{question}");
}

#[test]
fn a_head_carrying_no_other_pull_requests_commits_gets_the_ruling_it_got_before() {
    // Pull request 72 is open, but the worker never merged its branch.
    let (rig, runner, head) = Rig::with_pull_request_set("shep", |rig| {
        rig.push_by_hand("kelpie/9", "borrowed.txt");
        rig.forge.open_pull_request(72, "kelpie/9", &[9]);
    });
    rig.forge.set_checks(&head, Checks::Passed);

    let (_, question) = ruling_report(rig.verdict(&runner));

    assert_eq!(
        question,
        format!(
            "Merge pull request #71 at {} into main? `shep kelpie rule 71 yes` merges it, \
             `shep kelpie rule 71 no <note>` sends the worker your note for a fix that goes to CI \
             and back to you, and `shep kelpie rule 71 rework <note>` sends it your note for a \
             change the whole review reads again.",
            &head[..7]
        )
    );
}

#[test]
fn a_head_carrying_no_other_pull_requests_commits_is_not_stopped_under_auto() {
    let (rig, runner, head) = Rig::with_pull_request_set("shep", |rig| {
        rig.merge_auto();
        rig.push_by_hand("kelpie/9", "borrowed.txt");
        rig.forge.open_pull_request(72, "kelpie/9", &[9]);
    });
    rig.forge.set_checks(&head, Checks::Passed);

    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::MarkedReady { .. })
    ));
}

#[test]
fn a_borrowed_read_that_fails_parks_the_gate() {
    use crate::runner::git_reads::Found;
    use crate::runner::trigger::lock;
    use crate::worktree::WorktreeError;

    let (rig, runner, head) = borrowing("shep", |_| {});
    let open = rig.forge.list_open().unwrap();
    let mut runner = lock(&runner);
    runner.focus = Some(ticket(7));
    runner.reads.git.keep(Found::Borrowed {
        head: head.clone(),
        borrowed: Err(WorktreeError::Spawn("git is gone".to_owned())),
    });

    let Err(begin) = runner.borrowed(&open, &head) else {
        panic!("the failed read was passed over");
    };

    let Begin::Report(StepReport::GateFailed { issue, reason }) = *begin else {
        panic!("the gate was not parked");
    };
    assert_eq!(issue, ticket(7));
    assert!(
        reason.starts_with("cannot tell which pull requests this head carries"),
        "{reason}"
    );
    assert!(reason.contains("git is gone"), "{reason}");
}
