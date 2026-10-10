use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;

use super::PARKED_READ;
use crate::ports::{Cost, Usage};
use crate::runner::report::StepReport;
use crate::runner::{Runner, step};
use crate::test::{Hold, Rig, Scripted, git, write_in};

// Real threads on real time, so a held call's wait has this ceiling.
const PATIENCE: Duration = Duration::from_secs(30);

const ASKS: &str = "<kelpie-question>\nThis is already fixed on main. Shall I close it?\n\
                    </kelpie-question>\n";

const SENT_BACK: &str = "Your last turn ended with no pull request for this work item \
                         and no question. If a tool failed, try it again or find another \
                         way, and open the draft pull request once the work is done. If \
                         only the maintainer can unblock you, end your reply with a \
                         <kelpie-question> block.";

fn reply() -> Scripted {
    Scripted::Reply(Usage::default(), Cost(1))
}

// A running project whose worker on issue 7 asked ruling 1 and was alerted
fn asked(project: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new(project);
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([Scripted::Say(ASKS)]);
    step(&runner).unwrap();
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    (rig, runner)
}

fn closed_with_no_change(report: Option<StepReport>) -> bool {
    matches!(
        report,
        Some(StepReport::Finished {
            issue: 7,
            pull_request: None,
            merged: false,
            closed: true,
            ..
        })
    )
}

fn alert_texts(rig: &Rig) -> Vec<String> {
    rig.alerts
        .posts()
        .into_iter()
        .map(|(_, a)| a.text)
        .collect()
}

#[test]
fn a_worker_that_closes_its_issue_with_no_commit_finishes_with_no_nudge_or_ruling() {
    let (rig, runner) = asked("acme");
    rig.ask(&runner, "rule", Some("1 answer close the issue"));
    let hold = Hold::default();
    rig.claude.script([Scripted::Hold(hold.clone())]);
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| step(&runner));
        let began = hold.entered(PATIENCE);
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
        (&json!(7), &json!(false), &json!(true))
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
            issue: 7,
            id: 1,
            ..
        })
    ));

    // Parked, it stays: the commit is work the maintainer decides on.
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    assert_eq!(step(&runner).unwrap(), None);
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        status["work_item"]["phase"],
        json!({ "state": "ruling", "id": 1 })
    );
    assert_eq!(status["rulings"][0]["id"], 1);
}

#[test]
fn an_issue_the_forge_cannot_read_at_a_turns_end_is_tried_again_next_step() {
    let rig = Rig::new("rotom");
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([reply(), reply()]);
    step(&runner).unwrap();
    rig.forge.remove_issue(7);
    let Some(StepReport::GateFailed { issue: 7, reason }) = step(&runner).unwrap() else {
        panic!("the unread issue did not fail the step");
    };
    assert_eq!(reason, "cannot read issue #7: gh failed: no issue #7");
    assert_eq!(rig.claude.calls().len(), 1, "nothing was sent back yet");
}

#[test]
fn a_parked_item_whose_issue_is_closed_ends_at_its_next_read_and_its_ruling_goes() {
    let (rig, runner) = asked("shep");
    rig.forge.close_issue(7);
    // Its issue was read on the alert's pass, so the next read waits.
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], 1);
    rig.clock.advance(PARKED_READ);
    assert!(closed_with_no_change(step(&runner).unwrap()));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(
        (&status["work_item"], &status["rulings"]),
        (&json!(null), &json!([]))
    );
    assert_eq!(status["history"][0]["closed"], true);
    let said = "Issue #7 was closed with no change, so its work item ends with no pull \
                request. Ruling 1 is withdrawn. Nothing to answer.";
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
        json!({ "state": "ruling", "id": 1 })
    );
    assert_eq!(status["rulings"][0]["id"], 1);
}

#[test]
fn a_parked_item_stays_while_the_open_pull_requests_cannot_be_listed() {
    let (rig, runner) = asked("rotom");
    rig.forge.close_issue(7);
    rig.forge.set_board_down(true);
    rig.clock.advance(PARKED_READ);
    step(&runner).unwrap();
    assert_eq!(rig.ask(&runner, "status", None)["rulings"][0]["id"], 1);
    let notes = runner.lock().unwrap().take_notes();
    let note = "cannot list open pull requests";
    assert!(notes.iter().any(|n| n.starts_with(note)), "{notes:?}");
    rig.forge.set_board_down(false);
    rig.clock.advance(PARKED_READ);
    assert!(closed_with_no_change(step(&runner).unwrap()));
}

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
        json!({ "state": "ruling", "id": 1 })
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
        json!({ "state": "ruling", "id": 1 })
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
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));

    rig.ask(&runner, "rule", Some("1 answer no, keep going"));
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
