//! Closing the issue of a merged pull request through the runner's stand-ins

use std::sync::Mutex;

use serde_json::json;

use super::tests::{finished, ready_then_merge};
use crate::ports::PullRequestState;
use crate::runner::close_merged::FORGE_CLOSE_WAIT;
use crate::runner::{Runner, StepReport, step};
use crate::test::Rig;
use crate::test::ticket;

// The work item whose pull request merged while its issue stayed open: #71
// merged, the forge named #7 for itself to close, and never closed it
fn merged_leaving_its_issue_open(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner, _) = Rig::parked(project);
    rig.forge.list_ready(7, false);
    rig.ask(&runner, "rule", Some("71 yes"));
    rig.forge.set_state(71, PullRequestState::Merged);
    assert!(finished(step(&runner).unwrap()));
    assert!(rig.forge.closings().is_empty(), "the forge has the close");
    assert!(rig.forge.comments().is_empty(), "and no comment");
    (rig, runner)
}

#[test]
fn a_merged_issue_still_open_after_the_wait_is_closed_with_one_comment() {
    let (rig, runner) = merged_leaving_its_issue_open("koji");

    rig.clock.advance(FORGE_CLOSE_WAIT);
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.forge.closings(), [7]);
    let comments = rig.forge.comments();
    assert_eq!(comments.len(), 1, "{comments:?}");
    assert_eq!(comments[0].0, 7);
    assert!(comments[0].1.contains("#71"), "{comments:?}");
}

#[test]
fn a_merged_issue_still_open_inside_the_wait_is_left_to_the_forge() {
    let (rig, runner) = merged_leaving_its_issue_open("golbat");

    rig.clock.advance(FORGE_CLOSE_WAIT - 1);
    assert_eq!(step(&runner).unwrap(), None);

    assert!(rig.forge.closings().is_empty());
    assert!(rig.forge.comments().is_empty());
}

#[test]
fn a_finished_issue_that_did_not_merge_stays_open() {
    let (rig, runner, _) = Rig::parked("chelone");
    rig.forge.list_ready(7, false);
    rig.ask(&runner, "drop", None);

    rig.clock.advance(FORGE_CLOSE_WAIT);
    assert_eq!(step(&runner).unwrap(), None);

    assert!(rig.forge.closings().is_empty());
    assert!(rig.forge.comments().is_empty());
}

#[test]
fn a_reopened_issue_that_merged_once_is_not_closed_again() {
    let (rig, runner) = merged_leaving_its_issue_open("pelagia");
    // The forge closes #7, so the board lets it go, and the maintainer reopens
    // it to work on it again. #40 holds it back, so the board opens no work
    // item and only the sweep could close it.
    rig.forge.close_issue(7);
    assert_eq!(step(&runner).unwrap(), None);
    rig.forge.reopen_issue(7);
    rig.forge.block(7, 40);

    rig.clock.advance(FORGE_CLOSE_WAIT);
    assert_eq!(step(&runner).unwrap(), None);

    assert!(rig.forge.closings().is_empty());
    assert!(rig.forge.comments().is_empty());
}

#[test]
fn a_merged_issue_off_the_ready_list_is_not_swept() {
    let (rig, runner, _) = Rig::parked("pelagia");
    rig.forge.set_state(71, PullRequestState::Merged);
    assert!(finished(step(&runner).unwrap()));
    assert!(rig.forge.closings().is_empty());

    rig.clock.advance(FORGE_CLOSE_WAIT);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(
        rig.forge.closings().is_empty(),
        "the board reads no issue it does not list"
    );
}

#[test]
fn a_sweep_the_forge_refuses_comments_once() {
    let (rig, runner) = merged_leaving_its_issue_open("rotom");
    rig.forge.set_closes_down(true);

    rig.clock.advance(FORGE_CLOSE_WAIT);
    for look in 0..4 {
        assert_eq!(step(&runner).unwrap(), None, "{look}");
        rig.next_look();
    }

    assert_eq!(rig.forge.closes_refused(), 3, "the sweep gives up");
    assert!(rig.forge.closings().is_empty());
    assert_eq!(rig.forge.comments().len(), 1, "{:?}", rig.forge.comments());
    let notes = runner.lock().unwrap().take_notes();
    let refused: Vec<&String> = notes
        .iter()
        .filter(|n| n.contains("cannot close issue #7"))
        .collect();
    assert_eq!(refused.len(), 1, "{notes:?}");
    assert!(refused[0].contains("refused 3 times"), "{notes:?}");
}

#[test]
fn a_pull_request_merged_without_a_closing_reference_gets_its_issue_closed() {
    let (rig, runner, _) = Rig::parked("koji");
    rig.forge.set_closes(71, &[]);
    rig.forge.list_ready(7, false);
    rig.forge.list_ready(8, false);
    rig.forge.block(8, 7);
    rig.ask(&runner, "rule", Some("71 yes"));

    assert!(finished(ready_then_merge(&rig, &runner)));
    assert_eq!(rig.forge.closings(), [7]);
    let comments = rig.forge.comments();
    assert_eq!(comments.len(), 1, "{comments:?}");
    assert_eq!(comments[0].0, 7);
    assert!(comments[0].1.contains("#71"), "{comments:?}");
    rig.next_look();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Dispatched { issue, .. }) if issue == ticket(8)
    ));
}

#[test]
fn a_pull_request_the_maintainer_merged_by_hand_gets_its_issue_closed_too() {
    let (rig, runner, _) = Rig::parked("koji");
    rig.forge.set_closes(71, &[]);
    rig.forge.list_ready(7, false);
    rig.ask(&runner, "rule", Some("71 yes"));
    rig.forge.set_state(71, PullRequestState::Merged);

    assert!(finished(step(&runner).unwrap()));
    assert_eq!(rig.forge.closings(), [7]);
    assert_eq!(rig.forge.comments().len(), 1);
}

#[test]
fn an_issue_the_pull_request_names_is_left_to_the_forge_though_it_still_reads_open() {
    let (rig, runner, _) = Rig::parked("koji");
    rig.forge.list_ready(7, false);
    rig.ask(&runner, "rule", Some("71 yes"));
    // The forge has merged it and not yet closed #7, which #71 names.
    rig.forge.set_state(71, PullRequestState::Merged);

    assert!(finished(step(&runner).unwrap()));
    assert!(rig.forge.closings().is_empty());
    assert!(rig.forge.comments().is_empty());
}

#[test]
fn an_issue_the_forge_already_closed_gets_no_comment_and_no_second_close() {
    let (rig, runner, _) = Rig::parked("koji");
    rig.ask(&runner, "rule", Some("71 yes"));

    assert!(finished(ready_then_merge(&rig, &runner)));
    assert!(rig.forge.closings().is_empty());
    assert!(rig.forge.comments().is_empty());
}

#[test]
fn a_close_the_forge_refuses_is_tried_once_in_each_step_and_then_given_up() {
    let (rig, runner, _) = Rig::parked("koji");
    rig.forge.set_closes(71, &[]);
    rig.forge.list_ready(7, false);
    rig.forge.set_closes_down(true);
    rig.ask(&runner, "rule", Some("71 yes"));

    let first = ready_then_merge(&rig, &runner);
    assert!(
        matches!(&first, Some(StepReport::GateFailed { issue, reason })
            if *issue == ticket(7)
                && reason.contains("cannot close issue #7")
                && reason.contains("closing is refused")),
        "{first:?}"
    );
    assert_eq!(rig.forge.closes_refused(), 1);
    assert_ne!(rig.ask(&runner, "status", None)["work_item"], json!(null));

    // The step runs again after its backoff, and tries the close once.
    rig.clock.advance(15);
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::GateFailed { issue, .. }) if issue == ticket(7)
    ));
    assert_eq!(rig.forge.closes_refused(), 2);

    // The third refusal gives the issue up, and the work item finishes.
    rig.clock.advance(30);
    assert!(finished(step(&runner).unwrap()));
    assert_eq!(rig.forge.closes_refused(), 3);
    assert!(rig.forge.closings().is_empty());
    assert_eq!(rig.forge.comments().len(), 1, "the comment goes once");
    let notes = runner.lock().unwrap().take_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("cannot close issue #7"), "{notes:?}");
    assert!(notes[0].contains("closing is refused"), "{notes:?}");
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
}

#[test]
fn a_comment_the_forge_refuses_is_noted_and_the_issue_is_closed_without_it() {
    let (rig, runner, _) = Rig::parked("koji");
    rig.forge.set_closes(71, &[]);
    rig.forge.list_ready(7, false);
    rig.forge.set_comments_down(true);
    rig.ask(&runner, "rule", Some("71 yes"));

    assert!(finished(ready_then_merge(&rig, &runner)));
    assert_eq!(rig.forge.closings(), [7]);
    let notes = runner.lock().unwrap().take_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("cannot comment on issue #7"), "{notes:?}");
}

#[test]
fn an_item_that_ended_without_a_merge_leaves_its_issue_untouched() {
    let (rig, runner, _) = Rig::parked("koji");
    rig.forge.set_closes(71, &[]);
    rig.forge.list_ready(7, false);

    rig.ask(&runner, "drop", Some("7"));
    assert!(rig.forge.closings().is_empty());
    assert!(rig.forge.comments().is_empty());
}
