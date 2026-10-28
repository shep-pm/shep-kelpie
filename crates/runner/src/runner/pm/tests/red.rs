//! What the project manager reads of a red run: the end of the failed run's
//! log, written into its folder when a work item is parked on CI still red

use std::sync::Mutex;

use super::{answered, past_the_window, pm_seen, step_until};
use crate::ports::Checks;
use crate::runner::{Runner, StepReport};
use crate::test::{Rig, Scripted, ticket};

const NOTHING: &str = "{\"pick\": null}";

/// A project with a project manager and #7's pull request, whose worker is
/// parked on a `StillRed` ruling at the returned head, with no wake yet
pub(super) fn raised(project: &str, setup: impl FnOnce(&Rig)) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = Rig::with_pull_request_set(project, |rig| {
        rig.fix_attempts(0);
        rig.edit_settings(|s| {
            let listed = "\nimplementers = [\"sonnet-high\"]\n";
            assert!(s.contains(listed), "the example's implementers moved");
            s.replace(listed, "\nimplementers = [\"sonnet-high\"]\npm = \"pm\"\n")
        });
    });
    setup(&rig);
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling { id, .. })
     if id == ticket(71)));
    (rig, runner, head)
}

/// ... and the project manager's wake for it answered
fn parked_red(project: &str, setup: impl FnOnce(&Rig)) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner, head) = raised(project, setup);
    rig.claude.script([Scripted::Say(NOTHING)]);
    past_the_window(&rig);
    answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));
    (rig, runner, head)
}

#[test]
fn a_red_item_wakes_it_with_the_failed_logs_tail_in_its_folder_and_the_board_naming_it() {
    let log = "--- run 9\nFAIL TestFailedBootstrapRecordSwitchesTheAreaBackOff work.txt\n";
    let (rig, _runner, _) = parked_red("koji", |rig| rig.forge.set_ci_log(log));

    let pm = rig.paths().pm;
    assert_eq!(std::fs::read_to_string(pm.join("ci/7.log")).unwrap(), log);
    let board = std::fs::read_to_string(pm.join("board.md")).unwrap();
    let worktree = rig.paths().worktree(ticket(7)).display().to_string();
    assert!(
        board.contains(&format!(
            "- #7: worktree `{worktree}`, diff `diffs/7.diff`, CI log `ci/7.log`\n"
        )),
        "{board}"
    );
    let [wake] = pm_seen(&rig).try_into().unwrap();
    assert!(
        wake.call.prompt.contains("is stuck: CI failed"),
        "{}",
        wake.call.prompt
    );
    assert!(
        wake.call.prompt.contains("`ci/<issue>.log`"),
        "{}",
        wake.call.prompt
    );
}

#[test]
fn a_cancelled_runs_log_is_written_for_the_project_manager() {
    let log = "--- run 9\nnote: job `test-macos` was cancelled in step `Run cargo test --locked` \
               after 20m27s: its `timeout-minutes` ended it, so this is a slow job and not a \
               failing test\ntest-macos\tUNKNOWN STEP\t##[error]The operation was canceled.\n";
    let (rig, _runner, _) = parked_red("magnemite", |rig| rig.forge.set_ci_log_timed_out(log, 1));

    let pm = rig.paths().pm;
    assert_eq!(std::fs::read_to_string(pm.join("ci/7.log")).unwrap(), log);
    let board = std::fs::read_to_string(pm.join("board.md")).unwrap();
    assert!(board.contains(", CI log `ci/7.log`\n"), "{board}");
    assert!(!board.contains("no CI log"), "{board}");
}

#[test]
fn a_log_that_cannot_be_read_is_noted_and_the_wake_still_happens() {
    let (rig, runner, _) = parked_red("rotom", |rig| {
        rig.forge.set_ci_log_error("gh run list failed")
    });

    let pm = rig.paths().pm;
    assert!(!pm.join("ci/7.log").exists());
    let board = std::fs::read_to_string(pm.join("board.md")).unwrap();
    let why = "kelpie could not read it (gh failed: gh run list failed)";
    assert!(board.contains(&format!(", no CI log: {why}\n")), "{board}");
    assert_eq!(pm_seen(&rig).len(), 1, "the wake happened");
    let notes = runner.lock().unwrap().take_notes();
    let note = format!("no CI log of #7 for the project manager: {why}");
    assert!(notes.contains(&note), "{notes:?}");
}

#[test]
fn a_diffs_folder_that_cannot_be_cleared_does_not_cost_the_wake_its_ci_log() {
    let (rig, runner, _) = parked_red("snorlax", |rig| {
        rig.forge.set_ci_log("--- run 9\nboom work.txt\n")
    });
    let pm = rig.paths().pm;
    std::fs::remove_dir_all(pm.join("diffs")).unwrap();
    std::fs::write(pm.join("diffs"), "in the way").unwrap();
    std::fs::remove_file(pm.join("ci/7.log")).unwrap();

    rig.ask(&runner, "tell", Some("how is #7?"));
    rig.claude.script([Scripted::Say(NOTHING)]);
    answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));

    let board = std::fs::read_to_string(pm.join("board.md")).unwrap();
    assert!(board.contains("(no diffs: cannot clear"), "{board}");
    assert!(board.contains("- #7: CI log `ci/7.log`\n"), "{board}");
    assert!(pm.join("ci/7.log").is_file());
}

#[test]
fn a_log_does_not_outlive_its_ruling() {
    let (rig, runner, _) = parked_red("ditto", |rig| {
        rig.forge.set_ci_log("--- run 9\nboom work.txt\n")
    });
    let pm = rig.paths().pm;
    assert!(pm.join("ci/7.log").is_file());

    rig.ask(&runner, "drop", Some("7"));
    rig.ask(&runner, "tell", Some("how is #7?"));
    rig.claude.script([Scripted::Say(NOTHING)]);
    answered(Some(step_until(&runner, |r| {
        matches!(r, StepReport::PmAnswered { .. })
    })));

    assert!(!pm.join("ci/7.log").exists(), "a closed item's log stays");
    let board = std::fs::read_to_string(pm.join("board.md")).unwrap();
    assert!(!board.contains("ci/7.log"), "{board}");
}
