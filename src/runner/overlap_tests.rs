//! Ready issues against the branches of parked work items, through the
//! runner's stand-ins

use std::sync::Mutex;

use crate::board::Skip;
use crate::ports::Checks;
use crate::runner::slots_tests::{dispatched, issue_of, running};
use crate::runner::{Runner, StepReport, step};
use crate::test::{Rig, Scripted, git};

// One slot, and #7 through its turn, which adds `src/seven.rs`, and its
// review, parked on merge ruling 1 and alerted
fn seven_parked_on_src_seven(project: &str) -> (Rig, Mutex<Runner>) {
    let (rig, runner) = running(project);
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude.script([
        Scripted::Push("src/seven.rs", "seven\n"),
        Scripted::Text("CLEAN"),
    ]);
    for _ in 0..3 {
        assert_eq!(
            issue_of(step(&runner).unwrap()),
            7,
            "a turn, then its review"
        );
    }
    let head = rig.forge.head_of("kelpie/7").unwrap();
    rig.forge.set_checks(&head, Checks::Passed);
    assert!(matches!(
        rig.verdict(&runner),
        Some(StepReport::Ruling {
            issue: 7,
            id: 1,
            ..
        })
    ));
    assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
    (rig, runner)
}

fn overlap_9() -> Skip {
    Skip::Overlap {
        issue: 9,
        with: 7,
        files: vec!["src/seven.rs".into()],
    }
}

fn skipped(rig: &Rig, runner: &Mutex<Runner>) -> serde_json::Value {
    rig.ask(runner, "status", None)["skipped"].clone()
}

#[test]
fn a_ready_issue_naming_a_file_a_parked_branch_changes_waits() {
    let (rig, runner) = seven_parked_on_src_seven("acme");
    rig.forge.list_ready(9, false);
    rig.forge.set_ready_body(9, "Change `src/seven.rs`.");
    rig.forge.list_ready(10, false);
    rig.forge.set_ready_body(10, "Change `src/ten.rs`.");

    // Neither issue's paths are read until the board's next write.
    assert_eq!(step(&runner).unwrap(), None);
    let (issue, skipped_now) = dispatched(step(&runner).unwrap());
    assert_eq!(issue, 10);
    assert_eq!(skipped_now, [overlap_9()]);
    assert_eq!(skipped(&rig, &runner)[0]["reason"], "overlap");
}

#[test]
fn a_body_edited_after_its_paths_were_read_is_read_again_before_dispatch() {
    let (rig, runner) = seven_parked_on_src_seven("acme");
    rig.forge.list_ready(9, false);
    rig.forge.set_ready_body(9, "Change `src/nine.rs`.");
    assert_eq!(step(&runner).unwrap(), None, "#9's paths are read");
    let unread = Skip::PathsUnread {
        issue: 9,
        unknown: None,
    };
    assert_eq!(
        skipped(&rig, &runner),
        serde_json::to_value([&unread]).unwrap()
    );
    assert_eq!(skipped(&rig, &runner)[0]["reason"], "paths-unread");

    // Edited to name #7's file before the board opens it.
    rig.forge.set_ready_body(9, "Change `src/seven.rs`.");
    assert_eq!(step(&runner).unwrap(), None, "the new body is read first");
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        skipped(&rig, &runner),
        serde_json::to_value([overlap_9()]).unwrap()
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["working"],
        serde_json::json!([])
    );
}

#[test]
fn a_branch_git_cannot_read_keeps_the_files_last_read() {
    let (rig, runner) = seven_parked_on_src_seven("acme");
    let head = git(&rig.repo(), &["rev-parse", "kelpie/7"]);
    git(&rig.repo(), &["update-ref", "-d", "refs/heads/kelpie/7"]);
    rig.forge.list_ready(9, false);
    rig.forge.set_ready_body(9, "Change `src/seven.rs`.");
    assert_eq!(step(&runner).unwrap(), None, "#9's paths are read");
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(
        skipped(&rig, &runner),
        serde_json::to_value([overlap_9()]).unwrap()
    );
    git(
        &rig.repo(),
        &["update-ref", "refs/heads/kelpie/7", head.trim()],
    );
}

#[test]
fn a_parked_branch_whose_files_are_unknown_holds_every_ready_issue() {
    let (rig, runner) = seven_parked_on_src_seven("acme");
    let head = git(&rig.repo(), &["rev-parse", "kelpie/7"]);
    git(&rig.repo(), &["update-ref", "-d", "refs/heads/kelpie/7"]);
    // A runner started now has never read #7's branch.
    drop(runner);
    let runner = rig.open().unwrap();
    rig.forge.list_ready(10, false);
    rig.forge.set_ready_body(10, "Change `src/ten.rs`.");
    for _ in 0..2 {
        assert_eq!(step(&runner).unwrap(), None, "#10 waits on #7's files");
    }
    let waits = Skip::PathsUnread {
        issue: 10,
        unknown: Some(7),
    };
    assert_eq!(
        skipped(&rig, &runner),
        serde_json::to_value([waits]).unwrap()
    );
    let board = std::fs::read_to_string(rig.paths().board).unwrap();
    assert!(
        board.contains("waits for #7's branch, parked on a ruling, to be read"),
        "{board}"
    );

    git(
        &rig.repo(),
        &["update-ref", "refs/heads/kelpie/7", head.trim()],
    );
    rig.next_look();
    assert_eq!(step(&runner).unwrap(), None, "the board reads #7's files");
    assert_eq!(dispatched(step(&runner).unwrap()), (10, vec![]));
}
