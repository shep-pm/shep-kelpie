//! The commit a timed-out or failed turn's ruling keeps: the one its work
//! item's worktree stood at when it was raised, which its case compares with
//! the last of either kind's

use std::sync::Mutex;

use super::super::{name_pm, step_until, with_pm};
use crate::ports::AgentError;
use crate::runner::{Runner, StepReport};
use crate::settings::Harness;
use crate::test::{Rig, Scripted, git};
// Where a turn's last change sits, past the ten minutes a turn may be quiet for
const STALE: u64 = Rig::EPOCH - 11 * 60;

/// A project with a project manager, #7 open, and its worker's turn ended by
/// `end`, which parks it on a ruling. The rig, its runner, and the commit its
/// worktree stood at.
fn parked_at(project: &str, end: Scripted) -> (Rig, Mutex<Runner>, String) {
    let (rig, runner) = with_pm(project);
    rig.ask(&runner, "add", Some("7"));
    rig.claude.script([end]);
    step_until(&runner, |r| {
        matches!(r, StepReport::TimedOut { .. } | StepReport::Failed { .. })
    });
    let head = git(&rig.worktree_7(), &["rev-parse", "HEAD"]);
    (rig, runner, head)
}

/// The one ruling the state file holds, as JSON
fn raised(rig: &Rig) -> serde_json::Value {
    let text = std::fs::read_to_string(rig.paths().state).unwrap();
    let state: serde_json::Value = serde_json::from_str(&text).unwrap();
    state["rulings"].as_array().expect("a ruling")[0].clone()
}

#[test]
fn a_timed_out_turns_ruling_keeps_the_worktrees_head() {
    let (rig, _runner, head) = parked_at(
        "stuck-head-timeout",
        Scripted::TimeOutPlanting("stale.rs", "fn stale() {}\n", STALE),
    );

    assert_eq!(
        raised(&rig)["stuck_head"],
        head,
        "the commit it was raised at"
    );
}

#[test]
fn a_failed_turns_ruling_keeps_the_worktrees_head() {
    let (rig, _runner, head) = parked_at(
        "stuck-head-failed",
        Scripted::Fail(AgentError::Failed(Harness::ClaudeCode, "overloaded".into())),
    );

    assert_eq!(
        raised(&rig)["stuck_head"],
        head,
        "the commit it was raised at"
    );
}

#[test]
fn a_commit_the_worker_never_pushed_is_the_head_it_keeps() {
    let (rig, _runner, head) = parked_at(
        "stuck-head-unpushed",
        Scripted::TimeOutCommitting("done.rs", "fn done() {}\n", STALE),
    );
    let main = git(&rig.repo(), &["rev-parse", "origin/main"]);

    assert_eq!(raised(&rig)["stuck_head"], head);
    assert_ne!(head, main, "the worker's own commit is the head");
    assert_eq!(rig.forge.head_of("kelpie/7"), None, "it pushed nothing");
}

#[test]
fn a_head_git_cannot_read_leaves_the_ruling_standing_with_none() {
    let (rig, runner) = with_pm("stuck-head-unreadable");
    rig.ask(&runner, "add", Some("7"));
    std::fs::create_dir_all(rig.worktree_7()).unwrap();
    std::fs::write(rig.worktree_7().join("stray.txt"), "not a worktree\n").unwrap();

    step_until(&runner, |r| matches!(r, StepReport::Failed { .. }));

    let ruling = raised(&rig);
    assert_eq!(ruling["kind"]["reason"], "turn-failed", "{ruling}");
    assert_eq!(ruling.get("stuck_head"), None, "the head was not read");
    let notes = runner.lock().unwrap().take_notes();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("could not read the head ruling")),
        "{notes:?}"
    );
}

#[test]
fn another_kind_of_ruling_keeps_no_head() {
    let (rig, _runner, _) = Rig::parked_set("stuck-head-merge", name_pm);

    let ruling = raised(&rig);

    assert_eq!(ruling["kind"]["kind"], "merge", "{ruling}");
    assert_eq!(ruling.get("stuck_head"), None);
}
