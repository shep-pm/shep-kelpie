use std::sync::Mutex;

use serde_json::json;

use super::PARKED_READ;
use crate::ports::{Cost, PullRequestState, Usage};
use crate::runner::follow_up::tests::{ask_after_the_merge, racy};
use crate::runner::report::StepReport;
use crate::runner::{Runner, step};
use crate::test::ticket;
use crate::test::{CALL_PATIENCE, Hold, Rig, Scripted, git, write_in};

pub(in crate::runner::closed) const ASKS: &str = "<kelpie-question>\nThis is already fixed on main. Shall I close it?\n\
                    </kelpie-question>\n";

pub(in crate::runner::closed) const SENT_BACK: &str = "Your last turn ended with no pull request for this work item \
                         and no question. If a tool failed, try it again or find another \
                         way, and open the draft pull request once the work is done. If \
                         only the maintainer can unblock you, end your reply with a \
                         <kelpie-question> block.";

pub(in crate::runner::closed) fn reply() -> Scripted {
    Scripted::Reply(Usage::default(), Cost(1))
}

// A running project whose worker on issue 7 asked ruling 7 and was alerted
pub(in crate::runner::closed) fn asked(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(ASKS)]);
    step(&runner).unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(7) })
    );
    (rig, runner)
}

fn closed_with_no_change(report: Option<StepReport>) -> bool {
    matches!(
        report,
        Some(StepReport::Finished {
            issue,
            pull_request: None,
            merged: false,
            closed: true,
            ..
        })
     if issue == ticket(7))
}

pub(in crate::runner::closed) fn alert_texts(rig: &Rig) -> Vec<String> {
    rig.alerts
        .posts()
        .into_iter()
        .map(|(_, a)| a.text)
        .collect()
}

#[test]
fn a_worker_that_closes_its_issue_with_no_commit_finishes_with_no_nudge_or_ruling() {
    let (rig, runner) = asked("acme");
    rig.ask(&runner, "rule", Some("7 answer close the issue"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| step(&runner));
        let began = hold.entered(CALL_PATIENCE);
        rig.forge.close_issue(7);
        hold.release();
        assert!(began, "the answered turn never began");
        turn.join().unwrap().unwrap();
    });

    assert!(closed_with_no_change(step(&runner).unwrap()));
    assert_eq!(rig.claude.calls().len(), 2, "the worker was not sent back");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        (&status["work_item"], &status["rulings"]),
        (&json!(null), &json!([]))
    );
    let history = &status["history"][0];
    assert_eq!(
        (&history["issue"], &history["merged"], &history["closed"]),
        (&json!("7"), &json!(false), &json!(true))
    );
    let said = "Issue #7 was closed with no change, so its work item ends with no pull \
                request. Nothing to answer.";
    assert_eq!(alert_texts(&rig).last().map(String::as_str), Some(said));
    assert!(!rig.worktree_7().exists());
    assert_eq!(git(&rig.repo(), &["branch", "--list", "kelpie/7"]), "");
    let board = std::fs::read_to_string(rig.paths().board).unwrap();
    let event = "#7: issue closed with no change, work item done";
    assert!(board.contains(event), "{board}");
}

#[test]
fn a_closed_issue_whose_branch_holds_a_commit_is_still_sent_back_then_parked() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.close_issue(7);
    rig.claude
        .script([Scripted::Commit("work.txt", "work\n"), reply()]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let [_, again] = rig.claude.calls().try_into().unwrap();
    assert_eq!(again.prompt, SENT_BACK);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::Failed {
            issue,
            id,
            ..
        })
     if issue == ticket(7) && id == ticket(7)));

    // Parked, it stays: the commit is work the maintainer decides on.
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(7) })
    );
    assert_eq!(step(&runner).unwrap(), None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": "7" })
    );
    assert_eq!(status["rulings"][0]["id"], "7");
}

#[test]
fn an_issue_the_forge_cannot_read_at_a_turns_end_is_tried_again_next_step() {
    let rig = Rig::new("rotom");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([reply(), reply()]);
    step(&runner).unwrap();
    rig.forge.remove_issue(7);
    let Some(StepReport::GateFailed { issue, reason }) = step(&runner).unwrap() else {
        panic!("the unread issue did not fail the step");
    };
    assert_eq!(issue, ticket(7));
    assert_eq!(reason, "cannot read issue #7: gh failed: no issue #7");
    assert_eq!(rig.claude.calls().len(), 1, "nothing was sent back yet");
}

#[test]
fn a_parked_item_whose_issue_is_closed_ends_at_its_next_read_and_its_ruling_goes() {
    let (rig, runner) = asked("shep");
    rig.forge.close_issue(7);
    // Its issue was read on the alert's pass, so the next read waits.
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], "7");
    rig.clock.advance(PARKED_READ);
    assert!(closed_with_no_change(step(&runner).unwrap()));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        (&status["work_item"], &status["rulings"]),
        (&json!(null), &json!([]))
    );
    assert_eq!(status["history"][0]["closed"], true);
    let said = "Issue #7 was closed with no change, so its work item ends with no pull \
                request. Ruling 7 is withdrawn. Nothing to answer.";
    assert_eq!(alert_texts(&rig).last().map(String::as_str), Some(said));
    assert_eq!(rig.claude.calls().len(), 1);
    assert!(!rig.worktree_7().exists());
}

#[test]
fn a_parked_item_whose_closed_issue_an_open_pull_request_closes_stays() {
    let (rig, runner) = asked("koji");
    rig.forge.close_issue(7);
    rig.forge.open_pull_request(80, "kelpie/7-again", &[7]);
    rig.clock.advance(PARKED_READ);
    assert_eq!(step(&runner).unwrap(), None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": "7" })
    );
    assert_eq!(status["rulings"][0]["id"], "7");
}

#[test]
fn a_parked_item_stays_while_the_open_pull_requests_cannot_be_listed() {
    let (rig, runner) = asked("rotom");
    rig.forge.close_issue(7);
    rig.forge.set_board_down(true);
    rig.clock.advance(PARKED_READ);
    step(&runner).unwrap();
    assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], "7");
    let notes = runner.lock().unwrap().take_notes();
    let note = "cannot list open pull requests";
    assert!(notes.iter().any(|n| n.starts_with(note)), "{notes:?}");
    rig.forge.set_board_down(false);
    rig.clock.advance(PARKED_READ);
    assert!(closed_with_no_change(step(&runner).unwrap()));
}

#[cfg(unix)]
#[test]
fn a_closed_issue_whose_end_could_not_be_saved_is_read_again_on_the_next_pass() {
    use std::os::unix::fs::PermissionsExt;
    let (rig, runner) = asked("acme");
    rig.forge.close_issue(7);
    rig.clock.advance(PARKED_READ);
    let folder = rig.paths().state.parent().unwrap().to_owned();
    let mode = |m| std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(m));
    mode(0o555).unwrap();
    let failed = step(&runner);
    mode(0o755).unwrap();
    assert!(failed.is_err(), "{failed:?}");
    assert!(closed_with_no_change(step(&runner).unwrap()));
}

#[test]
fn a_restarted_runner_reads_a_parked_items_issue_on_its_first_pass() {
    let (rig, runner) = asked("rotom");
    rig.forge.close_issue(7);
    drop(runner);
    let runner = rig.open().unwrap();
    assert!(closed_with_no_change(step(&runner).unwrap()));
}

#[test]
fn a_parked_item_stays_while_its_issue_cannot_be_read() {
    let (rig, runner) = asked("acme");
    rig.forge.remove_issue(7);
    rig.clock.advance(PARKED_READ);
    assert_eq!(step(&runner).unwrap(), None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": "7" })
    );
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes.iter().any(|n| n.starts_with("cannot read issue #7")),
        "{notes:?}"
    );
}

#[test]
fn a_parked_item_with_files_in_its_worktree_stays_when_its_issue_is_closed() {
    let (rig, runner) = asked("koji");
    write_in(&rig.worktree_7(), "notes.txt", "half done\n");
    rig.forge.close_issue(7);
    rig.clock.advance(PARKED_READ);
    assert_eq!(step(&runner).unwrap(), None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": "7" })
    );
    assert!(rig.worktree_7().join("notes.txt").exists());
}

#[test]
fn a_queued_reply_to_a_parked_item_whose_issue_closed_resumes_no_worker() {
    let (rig, runner) = asked("acme");
    rig.forge.close_issue(7);
    rig.clock.advance(PARKED_READ);
    rig.reply("acme 1 answer go on then");
    for _ in 0..4 {
        step(&runner).unwrap();
    }
    assert_eq!(rig.claude.calls().len(), 1, "the worker was resumed");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"], json!(null));
    assert_eq!(status["history"][0]["closed"], true);
}

#[test]
fn a_turn_that_ends_with_files_left_in_its_worktree_is_sent_back_though_its_issue_closed() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.close_issue(7);
    rig.claude.script([
        Scripted::Plant("notes.txt", "half done\n"),
        reply(),
        reply(),
    ]);
    for _ in 0..3 {
        step(&runner).unwrap();
    }
    let [_, _, nudged] = rig.claude.calls().try_into().unwrap();
    assert_eq!(nudged.prompt, SENT_BACK);
    assert!(rig.worktree_7().join("notes.txt").exists());
}

#[test]
fn a_pull_request_on_another_branch_closing_the_issue_keeps_the_work_item() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.close_issue(7);
    rig.claude.script([reply(), reply()]);
    step(&runner).unwrap();
    rig.forge.open_pull_request(80, "kelpie/7-again", &[7]);
    step(&runner).unwrap();
    let [_, nudged] = rig.claude.calls().try_into().unwrap();
    assert_eq!(nudged.prompt, SENT_BACK);
}

#[test]
fn a_closed_issue_whose_work_git_cannot_read_is_noted_and_sent_back() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.close_issue(7);
    rig.claude.script([reply(), reply()]);
    step(&runner).unwrap();
    // A fetch that fails: `origin` is gone for the turn's end.
    let origin = rig.home.path().join("origin.git");
    let away = rig.home.path().join("origin-away.git");
    std::fs::rename(&origin, &away).unwrap();
    step(&runner).unwrap();
    std::fs::rename(&away, &origin).unwrap();
    let [_, nudged] = rig.claude.calls().try_into().unwrap();
    assert_eq!(nudged.prompt, SENT_BACK);
    let notes = runner.lock().unwrap().take_notes();
    let note = "issue #7 is closed, but its work cannot be read";
    assert!(notes.iter().any(|n| n.starts_with(note)), "{notes:?}");
}

#[test]
fn an_answered_question_gives_the_next_turn_that_stops_short_its_nudge_again() {
    let rig = Rig::new("koji");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([reply(), Scripted::Say(ASKS)]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(7) })
    );

    rig.ask(&runner, "rule", Some("7 answer no, keep going"));
    rig.claude.script([reply(), reply()]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    let [_, _, answered, nudged] = rig.claude.calls().try_into().unwrap();
    assert!(
        answered.prompt.contains("no, keep going"),
        "{}",
        answered.prompt
    );
    assert_eq!(nudged.prompt, SENT_BACK);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"], json!([]));
}

fn finished_merged(report: &StepReport) -> bool {
    matches!(
        report,
        StepReport::Finished {
            issue,
            pull_request: Some(71),
            merged: true,
            closed: false,
            ..
        }
     if *issue == ticket(7))
}

// The reports of the next few steps. An ended item is cleaned up on a pass
// of its own, and a free slot lets the board open the next issue first.
fn steps(runner: &Mutex<Runner>) -> Vec<StepReport> {
    (0..6).filter_map(|_| step(runner).unwrap()).collect()
}

fn dispatched_9(reports: &[StepReport]) -> bool {
    let opened =
        |r: &StepReport| matches!(r, StepReport::Dispatched { issue, .. } if *issue == ticket(9));
    reports.iter().any(opened)
}

// What the maintainer's own merge leaves: the pull request merged and its
// issue closed
fn merged_by_hand(rig: &Rig) {
    rig.forge.set_state(71, PullRequestState::Merged);
    rig.forge.close_issue(7);
}

// [`merged_by_hand`], with ready issue 9 waiting on issue 7
fn merged_by_hand_before_9(rig: &Rig) {
    rig.forge.list_ready(9, false);
    rig.forge.block(9, 7);
    merged_by_hand(rig);
}

// Whether the status shows issue 7's work item and rulings gone, though
// issue 9 was opened in its place
fn seven_is_gone(status: &serde_json::Value) -> bool {
    let rulings = status["rulings"].as_array().unwrap();
    status["work_item"]["issue"] == "9" && rulings.iter().all(|r| r["issue"] != "7")
}

// A running project whose worker on issue 7 pushed pull request 71 and then
// asked ruling 71
pub(in crate::runner::closed) fn asked_with_a_pull_request(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::PushSay("work.txt", "work\n", ASKS)]);
    step(&runner).unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["pull_request"], 71);
    (rig, runner)
}

#[test]
fn a_pull_request_merged_by_hand_ends_its_item_parked_on_a_merge_ruling() {
    let (rig, runner, _) = Rig::parked("shep");
    merged_by_hand_before_9(&rig);
    let reports = steps(&runner);
    assert!(reports.iter().any(finished_merged), "{reports:?}");
    assert!(dispatched_9(&reports), "{reports:?}");
    let status = rig.ask(&runner, "status", None);
    assert!(
        seven_is_gone(&status),
        "{} {}",
        status["work_item"]["issue"],
        status["rulings"]
    );
    let history = &status["history"][0];
    assert_eq!(
        (&history["issue"], &history["merged"], &history["closed"]),
        (&json!("7"), &json!(true), &json!(null))
    );
    let said = "Pull request #71 was merged, so work item #7 ends. Ruling 71 is withdrawn. \
                Nothing to answer.";
    assert!(
        alert_texts(&rig).iter().any(|t| t == said),
        "{:?}",
        alert_texts(&rig)
    );
    assert!(!rig.worktree_7().exists());
    assert_eq!(rig.forge.merges(), [], "kelpie made no merge");
}

#[test]
fn a_pull_request_merged_by_hand_ends_its_item_parked_on_a_workers_question() {
    let (rig, runner) = asked_with_a_pull_request("rotom");
    merged_by_hand_before_9(&rig);
    let reports = steps(&runner);
    assert!(reports.iter().any(finished_merged), "{reports:?}");
    assert!(dispatched_9(&reports), "{reports:?}");
    let status = rig.ask(&runner, "status", None);
    assert!(
        seven_is_gone(&status),
        "{} {}",
        status["work_item"]["issue"],
        status["rulings"]
    );
    assert_eq!(status["history"][0]["merged"], true);
}

#[test]
fn a_parked_item_whose_pull_request_was_closed_unmerged_asks_about_that_instead() {
    let (rig, runner) = asked_with_a_pull_request("koji");
    rig.forge.set_state(71, PullRequestState::Closed);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(71) })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": "71" })
    );
    let [ruling] = status["rulings"].as_array().unwrap().as_slice() else {
        panic!("the old ruling stayed: {}", status["rulings"]);
    };
    assert_eq!(ruling["id"], "71");
    assert_eq!(ruling["pull_request"], 71);
    assert_eq!(ruling["kind"]["kind"], "stuck");
    assert_eq!(ruling["kind"]["reason"], "closed");

    // It stays parked on that one, and is not asked again.
    rig.clock.advance(PARKED_READ);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], "71");
}

#[test]
fn a_pull_request_reopened_and_merged_after_it_was_found_closed_ends_its_item() {
    let (rig, runner) = asked_with_a_pull_request("shep");
    rig.forge.set_state(71, PullRequestState::Closed);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(71) })
    );
    rig.forge.set_state(71, PullRequestState::Merged);
    rig.forge.close_issue(7);
    rig.clock.advance(PARKED_READ);
    let reports = steps(&runner);
    assert!(reports.iter().any(finished_merged), "{reports:?}");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        (&status["work_item"], &status["rulings"]),
        (&json!(null), &json!([]))
    );
}

#[test]
fn an_item_parked_on_its_follow_up_ruling_after_kelpies_own_merge_is_left_alone() {
    let (rig, runner, id, _) = ask_after_the_merge(Rig::new("shep"), &[racy()]);
    assert_eq!(rig.forge.merges().len(), 1);
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id }));
    rig.forge.set_state(71, PullRequestState::Merged);
    rig.clock.advance(PARKED_READ);
    for _ in 0..2 {
        assert_eq!(step(&runner).unwrap(), None);
    }
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"][0]["id"], "71");
    assert_eq!(status["rulings"][0]["kind"]["kind"], "follow-up");
    assert_eq!(status["work_item"]["phase"]["id"], id.to_string());
}

#[test]
fn a_parked_item_stays_while_its_pull_request_cannot_be_read() {
    let (rig, runner, _) = Rig::parked("acme");
    merged_by_hand(&rig);
    rig.forge.set_down(true);
    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::Alerted { id: ticket(71) })
    );
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": "71" })
    );
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes.iter().any(|n| n.starts_with("cannot read #71")),
        "{notes:?}"
    );
}

#[test]
fn a_parked_read_that_fails_is_read_again_on_the_next_pass() {
    let (rig, runner, _) = Rig::parked("acme");
    merged_by_hand(&rig);
    rig.forge.set_down(true);
    step(&runner).unwrap();

    // The forge is back, and the item is read at once, with no interval waited.
    rig.forge.set_down(false);
    assert!(steps(&runner).iter().any(finished_merged));
}

// Whether the status shows no ruling about issue 7
fn seven_has_no_ruling(status: &serde_json::Value) -> bool {
    let rulings = status["rulings"].as_array().unwrap();
    rulings.iter().all(|r| r["issue"] != "7")
}

#[test]
fn a_trigger_is_answered_while_a_parked_items_pull_request_is_read() {
    let (rig, runner, _) = Rig::parked("shep");
    merged_by_hand(&rig);
    let read = rig.forge.hold_next_pull_request_read();

    let (answered, pass) = rig.asked_while_held(&runner, read, "add", Some("8"));

    assert!(answered.is_some(), "the trigger waited for the read");
    pass.unwrap();
    assert!(seven_has_no_ruling(&rig.ask(&runner, "status", None)));
}

#[test]
fn a_trigger_is_answered_while_the_worktree_is_checked_for_work() {
    let (rig, runner) = asked("shep");
    rig.forge.close_issue(7);
    rig.clock.advance(PARKED_READ);
    let read = rig.hold_next_git_fetch();

    let (answered, pass) = rig.asked_while_held(&runner, read, "add", Some("8"));

    assert!(answered.is_some(), "the trigger waited for the fetch");
    pass.unwrap();
    assert!(seven_has_no_ruling(&rig.ask(&runner, "status", None)));
}

#[test]
fn a_parked_item_dropped_during_the_read_ends_the_step_without_a_panic() {
    let (rig, runner, _) = Rig::parked("rotom");
    merged_by_hand(&rig);
    let read = rig.forge.hold_next_pull_request_read();

    let (dropped, pass) = rig.asked_while_held(&runner, read, "drop", Some("7"));

    assert!(dropped.is_some(), "the drop waited for the read");
    pass.unwrap();
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["work_item"], json!(null));
    assert!(seven_has_no_ruling(&status));
    assert_eq!(status["history"].as_array().map(Vec::len), Some(1));
}

#[test]
fn a_state_change_during_the_read_is_kept_by_the_apply() {
    let (rig, runner, _) = Rig::parked("koji");
    merged_by_hand(&rig);
    let read = rig.forge.hold_next_pull_request_read();

    let (added, pass) = rig.asked_while_held(&runner, read, "add", Some("8"));

    assert!(added.is_some(), "the trigger waited for the read");
    pass.unwrap();
    // An ended item is cleaned up on a pass of its own, and the board opens the next after it.
    steps(&runner);
    let status = rig.ask(&runner, "status", None);
    assert!(seven_has_no_ruling(&status));
    assert_eq!(
        status["work_item"]["issue"], "8",
        "the apply lost the work item added during the read: {status}"
    );
}

#[test]
fn a_trigger_that_moves_focus_during_the_read_does_not_aim_the_apply_at_another_item() {
    let (rig, runner) = asked("acme");
    rig.ask(&runner, "add", Some("8"));
    rig.forge.close_issue(7);
    rig.clock.advance(PARKED_READ);
    let read = rig.forge.hold_next_issue_read();

    // `drop` puts the focus on #8 and ends that work item.
    let (dropped, pass) = rig.asked_while_held(&runner, read, "drop", Some("8"));

    assert!(dropped.is_some(), "the drop waited for the read");
    pass.unwrap();
    let status = rig.ask(&runner, "status", None);
    assert!(seven_has_no_ruling(&status), "{status}");
    let history = status["history"].as_array().unwrap();
    let ended = |issue: &str| history.iter().find(|h| h["issue"] == issue);
    assert_eq!(
        ended("7").map(|h| &h["closed"]),
        Some(&json!(true)),
        "{history:?}"
    );
}

#[test]
fn an_item_cut_from_a_base_the_settings_left_is_still_closed_with_no_change() {
    let rig = Rig::new("acme");
    rig.base_is("dev");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(ASKS)]);
    step(&runner).unwrap();
    step(&runner).unwrap();
    rig.edit_settings(|s| s.replace("\nbase = \"dev\"", ""));
    let line = (runner.lock().unwrap())
        .reread(rig.settings(), rig.kelpie_settings())
        .unwrap();
    assert_eq!(
        line.as_deref(),
        Some("settings changed: git.base now in effect")
    );

    rig.forge.close_issue(7);
    rig.clock.advance(PARKED_READ);

    assert!(closed_with_no_change(step(&runner).unwrap()));
}
