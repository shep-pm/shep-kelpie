//! The board dispatching, through the runner's stand-ins

use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::board::Skip;
use crate::forge_ledger::{self, Kind};
use crate::ports::{AgentError, Cost, PullRequestState, Role, Usage};
use crate::runner::{BOARD_POLL, Pass, StepReport, advance, step};
use crate::settings::{AgentName, Effort};
use crate::state::{ProjectState, StateStore};
use crate::test::{CALL_PATIENCE, FakeForge, Hold, Rig, Scripted, ticket};

mod choice;

fn running(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    (rig, runner)
}

// Both wait on #32 while the board reads them. #8 is the older issue but
// became ready after #9, so it joins after it, where the rule alone
// would take #8 first.
#[test]
fn the_order_the_board_kept_survives_a_runner_restart() {
    let (rig, runner) = running("shep");
    rig.forge.list_ready(9, false);
    rig.forge.block(9, 32);
    assert_eq!(step(&runner).unwrap(), None);
    rig.forge.list_ready(8, false);
    rig.forge.block(8, 32);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "status", None)["order"], json!(["9", "8"]));
    drop(runner);

    let runner = rig.open().unwrap();
    rig.forge.close_issue(32);
    rig.next_look();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Dispatched { issue, .. }) if issue == ticket(9)
    ));
}

#[test]
fn a_priority_label_changed_between_reads_moves_the_issue_even_across_a_restart() {
    let (rig, runner) = running("shep");
    for issue in [8, 9] {
        rig.forge.list_ready(issue, false);
        rig.forge.block(issue, 32);
    }
    assert_eq!(step(&runner).unwrap(), None);
    drop(runner);

    // Bumped while the runner was down.
    rig.forge.label(9, "priority: P0");
    let runner = rig.open().unwrap();
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "status", None)["order"], json!(["9", "8"]));
}

#[test]
fn a_read_of_the_board_puts_a_new_p0_after_the_last_p0_and_status_shows_the_first_five() {
    let (rig, runner) = running("shep");
    for issue in [1, 2, 3, 4, 5] {
        rig.forge.list_ready(issue, false);
        rig.forge.block(issue, 32);
    }
    rig.forge.label(1, "priority: P0");
    rig.forge.label(2, "priority: P0");
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        rig.ask(&runner, "status", None)["order"],
        json!(["1", "2", "3", "4", "5"])
    );

    rig.forge.list_ready(6, false);
    rig.forge.block(6, 32);
    rig.forge.label(6, "priority: P0");
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        rig.ask(&runner, "status", None)["order"],
        json!(["1", "2", "6", "3", "4"])
    );
}

#[test]
fn a_dropped_item_labelled_ready_again_is_picked_again() {
    let (rig, runner) = running("shep");
    rig.forge.list_ready(7, false);
    step(&runner).unwrap();
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    rig.ask(&runner, "drop", None);

    // Still labelled, so the board leaves it be and says it was dropped.
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        rig.ask(&runner, "status", None)["skipped"],
        json!([{ "reason": "dropped", "issue": "7" }])
    );

    rig.forge.unlist_ready(7);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.forge.list_ready(7, false);
    rig.next_look();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Dispatched { issue, .. }) if issue == ticket(7)
    ));
}

// Stops #7's item with a no on a turn-timeout ruling, the issue still listed ready.
fn stopped_by_a_no(rig: &Rig, runner: &Mutex<Runner>) {
    rig.forge.list_ready(7, false);
    step(runner).unwrap();
    rig.claude.script([Scripted::Fail(AgentError::TimedOut(
        crate::settings::Harness::ClaudeCode,
    ))]);
    step(runner).unwrap();
    rig.ask(runner, "rule", Some("7 no not worth waiting for"));
    assert!(matches!(
       step(runner).unwrap(),
       Some(StepReport::Finished { issue, merged: false, .. }) if issue == ticket(7)
    ));
}

#[test]
fn an_item_stopped_by_a_no_stays_skipped_as_dropped_until_its_issue_is_edited() {
    let (rig, runner) = running("shep");
    stopped_by_a_no(&rig, &runner);

    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        rig.ask(&runner, "status", None)["skipped"],
        json!([{ "reason": "dropped", "issue": "7" }])
    );

    rig.forge.set_ready_body(
        7,
        &format!("{}\nNarrowed to one file.\n", FakeForge::passing_body(7)),
    );
    rig.next_look();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Dispatched { issue, .. }) if issue == ticket(7)
    ));
}

#[test]
fn an_item_stopped_by_a_no_is_picked_again_once_its_label_is_put_back() {
    let (rig, runner) = running("shep");
    stopped_by_a_no(&rig, &runner);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);

    rig.forge.unlist_ready(7);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    rig.forge.list_ready(7, false);
    rig.next_look();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Dispatched { issue, .. }) if issue == ticket(7)
    ));
}

#[test]
fn an_edit_after_a_restart_still_picks_a_stopped_item_again() {
    let (rig, runner) = running("shep");
    stopped_by_a_no(&rig, &runner);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    drop(runner);

    let runner = rig.open().unwrap();
    assert_eq!(step(&runner).unwrap(), None);
    rig.forge.set_ready_body(
        7,
        &format!("{}\nNarrowed to one file.\n", FakeForge::passing_body(7)),
    );
    rig.next_look();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Dispatched { issue, .. }) if issue == ticket(7)
    ));
}

#[test]
fn a_dropped_item_labelled_ready_again_while_the_board_is_shut_is_picked_when_it_opens() {
    // #7 is open and the runner finishes it, so the board is not read.
    let (rig, runner, _) = Rig::with_pull_request_set("acme", |rig| {
        rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    });
    rig.forge.list_ready(8, false);
    rig.ask(&runner, "add", Some("8"));
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    rig.ask(&runner, "drop", Some("8"));
    rig.ask(&runner, "finish", None);

    rig.forge.unlist_ready(8);
    rig.clock.advance(600);
    step(&runner).unwrap();
    rig.forge.list_ready(8, false);
    rig.clock.advance(600);
    step(&runner).unwrap();

    rig.ask(&runner, "start", None);
    let picked = (0..3).find_map(|_| match step(&runner).unwrap() {
        Some(StepReport::Dispatched { issue, .. }) => Some(issue),
        _ => None,
    });
    assert_eq!(picked, Some(ticket(8)));
}

#[test]
fn a_dropped_ticket_read_that_fails_is_read_again_on_the_next_pass() {
    // #7 is open and the runner finishes it, so the board is not read.
    let (rig, runner, _) = Rig::with_pull_request_set("acme", |rig| {
        rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    });
    rig.forge.list_ready(8, false);
    rig.ask(&runner, "add", Some("8"));
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    rig.ask(&runner, "drop", Some("8"));
    rig.ask(&runner, "finish", None);
    rig.forge.unlist_ready(8);

    rig.forge.set_down(true);
    rig.clock.advance(600);
    step(&runner).unwrap();

    // The forge is back, and the ticket is read at once, with no poll waited.
    rig.forge.set_down(false);
    step(&runner).unwrap();
    rig.forge.list_ready(8, false);
    rig.clock.advance(600);
    step(&runner).unwrap();

    rig.ask(&runner, "start", None);
    let picked = (0..3).find_map(|_| match step(&runner).unwrap() {
        Some(StepReport::Dispatched { issue, .. }) => Some(issue),
        _ => None,
    });
    assert_eq!(picked, Some(ticket(8)));
}

#[test]
fn a_merged_item_the_forge_still_lists_ready_stays_skipped() {
    let (rig, runner, _) = Rig::parked("acme");
    // The forge keeps the issue open: the pull request closes nothing and
    // kelpie's own close is refused.
    rig.forge.set_closes(71, &[]);
    rig.forge.set_closes_down(true);
    rig.ask(&runner, "rule", Some("71 yes"));
    step(&runner).unwrap();
    rig.clock.advance(crate::runner::CHECKS_SETTLE);
    step(&runner).unwrap();
    // One refused close in each step, and the third lets the item go.
    for wait in [15, 30] {
        rig.clock.advance(wait);
        step(&runner).unwrap();
    }
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));

    // The forge still lists #7 as ready, and it stays skipped as finished
    // even once a label comes off and goes on again.
    assert_eq!(step(&runner).unwrap(), None);
    rig.next_look();
    rig.forge.unlist_ready(7);
    step(&runner).unwrap();
    rig.next_look();
    rig.forge.list_ready(7, false);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        rig.ask(&runner, "status", None)["skipped"],
        json!([{ "reason": "finished", "issue": "7" }])
    );
}

fn sonnet_high() -> AgentName {
    AgentName::try_from("sonnet-high".to_owned()).unwrap()
}

#[test]
fn the_older_of_two_ready_issues_is_dispatched_and_the_other_waits() {
    let (rig, runner) = running("shep");
    rig.forge.list_ready(12, false);
    rig.forge.list_ready(9, false);

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched {
            issue: ticket(9),
            agent: sonnet_high(),
            skipped: vec![],
        })
    );
    rig.claude.script([Scripted::Say(
        "<kelpie-question>\nWhich flag?\n</kelpie-question>\n",
    )]);
    step(&runner).unwrap();
    let [call] = rig.claude.calls().try_into().unwrap();
    assert!(call.prompt.starts_with("/mattpocock:implement "));
    assert!(call.prompt.contains("\nYour work item is issue #9: "));

    // #9 is parked on its question, which frees its slot for #12.
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(9) })
    );
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched {
            issue: ticket(12),
            agent: sonnet_high(),
            skipped: vec![],
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["issue"], "9");
    assert_eq!(
        (&status["working"], &status["parked"]),
        (&json!(["12"]), &json!(["9"]))
    );
    assert_eq!(rig.claude.calls().len(), 1);
}

#[test]
fn an_issue_with_an_open_pull_request_or_an_assignee_is_skipped() {
    let (rig, runner) = running("koji");
    rig.forge.list_ready(3, false);
    rig.forge.list_ready(4, true);
    rig.forge.list_ready(5, false);
    rig.forge.open_pull_request(30, "fix/three", &[3]);

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched {
            issue: ticket(5),
            agent: sonnet_high(),
            skipped: vec![
                Skip::PullRequest {
                    issue: ticket(3),
                    pull_request: 30
                },
                Skip::Assigned { issue: ticket(4) },
            ],
        })
    );
}

#[test]
fn a_ready_issue_an_earlier_worker_labelled_in_progress_is_skipped_and_listed() {
    let (rig, runner) = running("kanto");
    rig.forge.list_ready(3, false);
    rig.forge.label(3, "in-progress");

    assert_eq!(step(&runner).unwrap(), None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"], json!(null));
    assert_eq!(
        status["skipped"],
        json!([{ "reason": "in-progress", "issue": "3" }])
    );
}

#[test]
fn a_reopened_runner_keeps_its_own_in_progress_item_and_opens_no_second() {
    let (rig, runner) = running("johto");
    rig.forge.list_ready(3, false);
    step(&runner).unwrap();
    assert_eq!(rig.ask(&runner, "status", None)["working"], json!(["3"]));

    drop(runner);
    let runner = rig.open().unwrap();
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["working"], json!(["3"]));
    assert_eq!(status["skipped"], json!([]));
    assert_eq!(rig.claude.calls().len(), 1);
}

#[test]
fn a_reopened_runner_keeps_its_own_parked_in_progress_item_out_of_skipped() {
    let (rig, runner) = running("hoenn");
    rig.forge.list_ready(3, false);
    step(&runner).unwrap();
    rig.claude.script([Scripted::Say(
        "<kelpie-question>\nWhich flag?\n</kelpie-question>\n",
    )]);
    step(&runner).unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(3) })
    );

    drop(runner);
    let runner = rig.open().unwrap();
    assert_eq!(step(&runner).unwrap(), None);

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["parked"], json!(["3"]));
    assert_eq!(status["skipped"], json!([]));
}

#[test]
fn a_board_with_only_taken_issues_dispatches_nothing() {
    let (rig, runner) = running("rotom");
    rig.forge.list_ready(3, true);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
}

// A project listing both of kelpie's own implementers.
fn listing_opus(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    rig.agent_board.ranks(&["sonnet-high", "opus-high"]);
    let runner = rig.open().unwrap();
    (rig, runner)
}

#[test]
fn a_pinned_agent_label_runs_that_items_worker_on_the_agent_it_names() {
    let (rig, runner) = listing_opus("golbat");
    rig.forge.list_ready(6, false);
    rig.forge.label(6, "agent:opus-high!");
    step(&runner).unwrap();
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();

    let [call] = rig.claude.calls().try_into().unwrap();
    assert_eq!(
        (call.model.as_str(), call.effort),
        ("claude-opus-5-5", Effort::High)
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["agent"],
        json!("opus-high")
    );
}

#[test]
fn an_agent_label_added_after_dispatch_changes_nothing() {
    let (rig, runner) = listing_opus("chelone");
    rig.forge.list_ready(6, false);
    rig.forge.label(6, "agent:sonnet-high!");
    step(&runner).unwrap();
    rig.forge.label(6, "agent:opus-high!");
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    step(&runner).unwrap();
    assert_eq!(rig.claude.calls()[0].model, "claude-sonnet-5-5");
}

// A runner runs from its start: no trigger comes before its first look.
#[test]
fn a_runner_reads_the_board_on_its_first_pass() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.forge.list_ready(2, false);
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Dispatched { issue, .. }) if issue == ticket(2)
    ));
    assert_eq!(rig.ask(&runner, "status", None)["work_item"]["issue"], "2");
}

#[test]
fn a_board_that_cannot_be_read_is_reported_and_dispatches_nothing() {
    let (rig, runner) = running("reactmap");
    rig.forge.list_ready(2, false);
    rig.forge.set_board_down(true);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::BoardFailed {
            reason: "cannot read the board: gh failed: the board is down".into()
        })
    );
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));

    rig.forge.set_board_down(false);
    rig.next_look();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Dispatched { issue, .. }) if issue == ticket(2)
    ));
}

#[test]
fn a_dispatch_that_cannot_be_saved_is_an_error_and_takes_nothing() {
    let (rig, runner) = running("rotom");
    rig.forge.list_ready(4, false);
    let folder = rig.paths().state.parent().unwrap().to_owned();
    std::fs::remove_dir_all(&folder).unwrap();
    // A file where the folder was, which a save cannot make a folder of.
    std::fs::write(&folder, "").unwrap();
    let err = step(&runner).unwrap_err();
    assert!(
        err.to_string().starts_with("cannot write state file"),
        "{err}"
    );
    assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    assert_eq!(rig.claude.calls(), []);
}

#[test]
fn a_ready_issue_the_forge_cannot_show_is_skipped_for_the_next_oldest() {
    let (rig, runner) = running("golbat");
    rig.forge.list_ready(4, false);
    rig.forge.list_ready(6, false);
    rig.forge.remove_issue(4);
    let skip = Skip::Failed {
        issue: ticket(4),
        error: "cannot read the issue: gh failed: no issue #4".into(),
    };
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched {
            issue: ticket(6),
            agent: sonnet_high(),
            skipped: vec![skip],
        })
    );
    assert_eq!(rig.meter.reads(), 1);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["issue"], "6");
    assert_eq!(
        status["skipped"],
        json!([{
            "reason": "failed",
            "issue": "4",
            "error": "cannot read the issue: gh failed: no issue #4"
        }])
    );
}

#[test]
fn a_board_of_issues_the_forge_cannot_show_reports_each_and_takes_nothing() {
    let (rig, runner) = running("golbat");
    rig.forge.list_ready(4, false);
    rig.forge.list_ready(5, false);
    rig.forge.remove_issue(4);
    rig.forge.remove_issue(5);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::BoardFailed {
            reason: "cannot dispatch #4: cannot read the issue: gh failed: no issue #4; \
                     cannot dispatch #5: cannot read the issue: gh failed: no issue #5"
                .into()
        })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"], json!(null));
    assert_eq!(status["skipped"].as_array().unwrap().len(), 2);
    assert_eq!(rig.meter.reads(), 1);
    assert_eq!(rig.claude.calls(), []);
}

#[test]
fn an_issue_with_an_open_blocker_is_skipped_for_the_next_oldest() {
    let (rig, runner) = running("eevee");
    rig.forge.list_ready(8, false);
    rig.forge.list_ready(9, false);
    rig.forge.block(8, 32);
    rig.forge.block(8, 12);
    rig.forge.close_issue(12);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched {
            issue: ticket(9),
            agent: sonnet_high(),
            skipped: vec![Skip::Blocked {
                issue: ticket(8),
                by: vec![ticket(32)],
                unlisted: 0
            }],
        })
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["skipped"],
        json!([{ "reason": "blocked", "issue": "8", "by": ["32"] }])
    );
}

#[test]
fn a_blocked_issue_waits_through_its_blockers_pull_request_until_the_blocker_closes() {
    let (rig, runner) = running("ditto");
    rig.forge.list_ready(8, false);
    rig.forge.block(8, 32);
    rig.forge.open_pull_request(40, "kelpie/32", &[32]);
    assert_eq!(step(&runner).unwrap(), None);
    rig.forge.set_state(40, PullRequestState::Merged);
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        rig.ask(&runner, "status", None)["skipped"],
        json!([{ "reason": "blocked", "issue": "8", "by": ["32"] }])
    );

    rig.forge.close_issue(32);
    rig.next_look();
    assert!(matches!(
       step(&runner).unwrap(),
       Some(StepReport::Dispatched { issue, .. }) if issue == ticket(8)
    ));
    assert_eq!(rig.ask(&runner, "status", None)["skipped"], json!([]));
}

// Ready #8 whose `Blocked by:` line names #7, which the forge has no link to
fn naming_an_unlinked_blocker() -> (Rig, Mutex<Runner>) {
    let (rig, runner) = running("ditto");
    rig.forge.list_ready(8, false);
    let body = format!("Blocked by: #7\n\n{}", FakeForge::passing_body(8));
    rig.forge.set_ready_body(8, &body);
    (rig, runner)
}

#[test]
fn an_issue_naming_an_open_blocker_with_no_link_is_linked_and_held() {
    let (rig, runner) = naming_an_unlinked_blocker();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.links_added(), [(8, 7)]);
    assert_eq!(
        rig.ask(&runner, "status", None)["skipped"],
        json!([{ "reason": "blocked", "issue": "8", "by": ["7"] }])
    );

    // The next poll finds the link, and adds none.
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.forge.links_added(), [(8, 7)]);
}

#[test]
fn an_issue_naming_a_closed_blocker_is_linked_and_picked() {
    let (rig, runner) = naming_an_unlinked_blocker();
    rig.forge.close_issue(7);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue, .. }) if issue == ticket(8)
    ));
    assert_eq!(rig.forge.links_added(), [(8, 7)]);
}

#[test]
fn a_link_the_forge_refuses_holds_the_issue_and_is_logged_once() {
    let (rig, runner) = naming_an_unlinked_blocker();
    rig.forge.close_issue(7);
    rig.forge.set_links_refused(true);
    for _ in 0..3 {
        assert_eq!(step(&runner).unwrap(), None);
        rig.next_look();
    }
    assert_eq!(rig.forge.links_added(), []);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["skipped"][0]["reason"], json!("unlinked"));
    assert_eq!(status["skipped"][0]["by"], json!(["7"]));
    let notes = runner.lock().unwrap().take_notes();
    let held = notes.iter().filter(|n| n.starts_with("#8 skipped")).count();
    assert_eq!(held, 1, "{notes:?}");

    // Linking works again, so the issue is picked.
    rig.forge.set_links_refused(false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue, .. }) if issue == ticket(8)
    ));
}

#[test]
fn a_pull_request_on_the_blocked_by_line_is_no_link_and_holds_nothing() {
    let (rig, runner) = naming_an_unlinked_blocker();
    rig.forge.open_pull_request(7, "someone-else", &[]);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue, .. }) if issue == ticket(8)
    ));
    assert_eq!(rig.forge.links_added(), []);
    assert_eq!(rig.ask(&runner, "status", None)["skipped"], json!([]));
}

#[test]
fn the_workers_draft_pull_request_is_recorded_when_its_turn_ends() {
    let (rig, runner) = running("shep");
    rig.forge.list_ready(7, false);
    step(&runner).unwrap();
    rig.forge.open_pull_request(70, "someone-else", &[]);
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Reply(Usage::default(), Cost(1))]);
    let Some(StepReport::Ended { pull_request, .. }) = step(&runner).unwrap() else {
        panic!("the turn did not end");
    };
    assert_eq!(pull_request, Some(71));
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["pull_request"],
        71
    );
}

#[test]
fn a_turn_that_opened_no_pull_request_records_none() {
    let (rig, runner) = running("koji");
    rig.forge.list_ready(7, false);
    step(&runner).unwrap();
    rig.claude.script([Scripted::Fail(AgentError::Failed(
        crate::settings::Harness::ClaudeCode,
        "overloaded".into(),
    ))]);
    step(&runner).unwrap();
    assert_eq!(
        rig.ask(&runner, "status", None)["work_item"]["pull_request"],
        json!(null)
    );
}

// The first wait of a failed step, and the least a poll waits after a good read
const RETRY_FIRST: u64 = 15;

#[test]
fn a_trigger_is_answered_while_the_board_is_read() {
    let (rig, runner) = running("shep");
    rig.forge.list_ready(8, false);
    let read = rig.forge.hold_next_board_read();

    std::thread::scope(|scope| {
        let pass = scope.spawn(|| advance(&runner));
        assert!(read.waiting(CALL_PATIENCE), "the pass never read the board");
        assert!(
            runner.try_lock().is_ok(),
            "the pass held the lock in the read"
        );
        let added = rig.ask(&runner, "add", Some("7"));
        assert_eq!(added.get("error"), None, "{added}");
        drop(read);
        pass.join().unwrap().unwrap();
    });

    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"]["issue"], "7", "{status}");
    assert_eq!(status["working"], json!(["7"]), "{status}");
}

#[test]
fn a_board_read_held_for_three_seconds_holds_the_lock_for_less_than_one() {
    let (rig, runner) = running("shep");
    let read = rig.forge.hold_next_board_read();

    std::thread::scope(|scope| {
        let pass = scope.spawn(|| advance(&runner));
        assert!(read.waiting(CALL_PATIENCE), "the pass never read the board");
        std::thread::sleep(Duration::from_secs(3));
        drop(read);
        pass.join().unwrap().unwrap();
    });

    let status = rig.ask(&runner, "status", None);
    let holds = status["lock"].as_object().expect("the holds by site");
    let longest = holds.values().filter_map(json_f64).fold(0.0, f64::max);
    assert!(longest > 0.0 && longest < 1.0, "{holds:?}");
}

fn json_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
}

#[test]
fn a_board_read_that_fails_leaves_no_stamp_and_is_retried_on_the_backoff() {
    let (rig, runner) = running("shep");
    rig.forge.set_down(true);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::BoardFailed { .. })
    ));
    let reads = |rig: &Rig| rig.forge.board_reads();
    let first = reads(&rig);

    // A stamped read would wait out the poll.
    rig.clock.advance(RETRY_FIRST - 1);
    step(&runner).unwrap();
    assert_eq!(reads(&rig), first, "read inside the first backoff");
    rig.clock.advance(1);
    step(&runner).unwrap();
    assert_eq!(reads(&rig), first + 1, "not read once the backoff passed");

    rig.clock.advance(2 * RETRY_FIRST - 1);
    step(&runner).unwrap();
    assert_eq!(reads(&rig), first + 1, "read inside the doubled backoff");
    rig.clock.advance(1);
    rig.forge.set_down(false);
    rig.forge.list_ready(8, false);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Dispatched { issue, .. }) if issue == ticket(8)
    ));
    assert!(2 * RETRY_FIRST < BOARD_POLL.as_secs());
}

// #7 takes the first of two slots with a turn in flight, which acts before the board's step
// is reached
#[test]
fn a_board_read_the_pass_did_not_use_is_dropped_and_stamps_nothing() {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.list_ready(8, false);
    let turn = Hold::default();
    rig.claude.script([Scripted::Hold(turn.clone())]);

    let before = rig.forge.board_reads();
    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(turn.entered(CALL_PATIENCE), "#7's turn never began");
    assert_eq!(
        rig.forge.board_reads(),
        before + 1,
        "the pass read the board"
    );

    // Nothing stamped the look, so the next pass reads again at once.
    let report = advance(&runner).unwrap();
    assert_eq!(
        rig.forge.board_reads(),
        before + 2,
        "the board waited its poll"
    );
    assert!(
        matches!(report, Pass::Report(StepReport::Dispatched { issue, .. }) if issue == ticket(8)),
        "{report:?}"
    );
    turn.release();
}

// The ledger names every forge call in the order it was made. The board's are a listing of
// the ready issues, the open pull requests, the issue of #6 whose entry the board trims, and
// the parents with every sub-issue closed with the sub-issues of the one off its list.
#[test]
fn a_board_read_taken_off_the_lock_makes_the_same_forge_calls_as_before() {
    let rig = Rig::new("shep");
    let mut state = ProjectState::new();
    state.finished = vec![ticket(6)];
    let file = rig.paths().state;
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    StateStore::new(file.clone()).save(&state).unwrap();
    let runner = rig.open().unwrap();
    rig.forge.close_issue(6);
    rig.forge.open_issue(5, "A parent", "Split in two.\n");
    rig.forge.link_sub_issue(5, 9);
    rig.forge.close_issue(9);

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::ParentClosed { issue: ticket(5) })
    );

    let lines = forge_ledger::read(&rig.paths().kelpie_home.join(forge_ledger::FILE)).unwrap();
    let board = [
        Kind::ReadyIssues,
        Kind::OpenPullRequests,
        Kind::Issue,
        Kind::FinishedParents,
        Kind::SubIssues,
    ];
    let made: Vec<Kind> = (lines.iter().map(|l| l.kind))
        .filter(|kind| board.contains(kind))
        .collect();
    assert_eq!(made, board, "{lines:?}");
    // #6 was read closed, so its entry went.
    assert_eq!(StateStore::new(file).load().unwrap().unwrap().finished, []);
}

// The runner's loop as `work` runs it: a pass that started a call or reported goes on at once,
// and an idle one sleeps until the next look, here by moving the clock. Runs until `want` more
// issues are opened, and returns them with the seconds slept on the way.
fn looped(rig: &Rig, runner: &Mutex<Runner>, want: usize) -> (Vec<TicketId>, u64) {
    let (wake, woken) = std::sync::mpsc::channel();
    runner.lock().unwrap().wake_with(wake);
    let (mut opened, mut slept) = (Vec::new(), 0);
    for _ in 0..200 {
        match advance(runner).unwrap() {
            Pass::Report(StepReport::Dispatched { issue, .. }) => opened.push(issue),
            Pass::Report(_) | Pass::Started => {}
            Pass::Idle => {
                // A call that ended wakes the loop; otherwise it sleeps to the next look.
                if woken.recv_timeout(Duration::from_millis(50)).is_err() {
                    let wait = runner.lock().unwrap().next_look().unwrap_or(BOARD_POLL);
                    rig.clock.advance(wait.as_secs());
                    slept += wait.as_secs();
                }
            }
        }
        if opened.len() == want {
            break;
        }
    }
    (opened, slept)
}

// An implementer on a local model, whose calls queue for its lease
const LOCAL_CODER: &str = "---\nrole: implementer\nharness: stand-in\nmodel: qwen3-coder\n\
                           effort: low\nusage: none\n---\n";

// 10-25, the shep runner: two implementers, 12 slots, #1 to #5 open on the first listed,
// every first turn waiting on its lease, and #430 next on the board, takeable and unlabelled.
// Their turns wait until the returned holds are released.
fn afternoon() -> (Rig, Mutex<Runner>, Vec<Hold>) {
    let rig = Rig::new("shep");
    rig.write_agent("coder", LOCAL_CODER);
    rig.agent_board.ranks(&["coder", "sonnet-high"]);
    rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 12"));
    let runner = rig.open().unwrap();
    for issue in [1, 2, 3, 4, 5, 430] {
        rig.forge.list_ready(issue, false);
    }
    let queues: Vec<Hold> = (0..6).map(|_| Hold::default()).collect();
    rig.claude.script(
        (queues.iter())
            .map(|queue| Scripted::Queued(queue.clone(), Box::new(Scripted::Say("done")))),
    );
    (rig, runner, queues)
}

// Passes, without moving the clock, until `done`
fn passes_until(runner: &Mutex<Runner>, what: &str, done: impl Fn() -> bool) {
    let by = std::time::Instant::now() + CALL_PATIENCE;
    while !done() {
        assert!(std::time::Instant::now() < by, "never {what}");
        advance(runner).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Whether `count` work items are open, all waiting on a lease
fn all_waiting_on_a_lease(rig: &Rig, runner: &Mutex<Runner>, count: usize) -> bool {
    let status = rig.ask(runner, "status", None);
    let items = status["work_items"].as_array().unwrap();
    items.len() == count && items.iter().all(|item| item["waiting"]["why"] == "lease")
}

// 10-25: the shep runner opened nothing for 37 minutes with #430 takeable and 5 of 12 slots
// taken. This rules out as the gate a first turn waiting on its lease, and an opening making
// the next read due. Each issue opens with no poll waited.
#[test]
fn opening_chains_into_the_next_issue_while_every_first_turn_waits_on_its_lease() {
    let (rig, runner, queues) = afternoon();

    let (opened, slept) = looped(&rig, &runner, 6);

    assert_eq!(opened, [1, 2, 3, 4, 5, 430].map(ticket), "after {slept}s");
    assert_eq!(slept, 0, "a board poll was waited for");
    passes_until(&runner, "waited on its lease", || {
        all_waiting_on_a_lease(&rig, &runner, 6)
    });
    queues.iter().for_each(Hold::release);
}
