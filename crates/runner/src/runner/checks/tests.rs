//! The check runs on a pull request's head, through the runner's stand-ins

use std::sync::Mutex;

use crate::ports::{CLEAN, Checks, Standing};
use crate::runner::review_bot::LATE_READ;
use crate::runner::{Runner, step};
use crate::test::{Rig, Scripted, ShownCheck};

fn names(checks: &[ShownCheck]) -> Vec<(&str, &str, Option<&str>)> {
    (checks.iter())
        .map(|c| (c.name.as_str(), c.status.as_str(), c.conclusion.as_deref()))
        .collect()
}

fn on(rig: &Rig, head: &str) -> Vec<ShownCheck> {
    (rig.github.check_runs().into_iter())
        .filter(|c| c.head == head)
        .collect()
}

// The worker's first turn pushes and the pull request is open.
fn reviewing() -> (Rig, Mutex<Runner>, String) {
    let rig = Rig::new("shep");
    rig.with_app();
    let runner = rig.open().unwrap();
    rig.ask(&runner, "add", Some("7"));
    rig.forge.open_pull_request(71, "kelpie/7", &[7]);
    rig.claude
        .script([Scripted::Push("work.txt", "work\n"), Scripted::Text(CLEAN)]);
    step(&runner).unwrap(); // the worker's first turn
    let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
    (rig, runner, head)
}

#[test]
fn a_reviewed_head_shows_review_in_progress_then_success() {
    let (rig, runner, head) = reviewing();

    let during = on(&rig, &head);
    assert_eq!(
        names(&during),
        [
            ("kelpie / review", "in_progress", None),
            ("kelpie", "in_progress", None)
        ]
    );
    assert_eq!(during[0].title, "Round 1");
    assert!(
        during[0]
            .summary
            .ends_with("To ask for another round, run shep kelpie rework 71."),
        "{}",
        during[0].summary
    );

    step(&runner).unwrap(); // round 1, qwen: clean
    step(&runner).unwrap(); // round 2, claude: clean

    assert_eq!(
        names(&on(&rig, &head)),
        [
            ("kelpie / review", "completed", Some("success")),
            ("kelpie", "in_progress", None)
        ]
    );
}

#[test]
fn a_ruling_names_its_question_and_fails_on_a_no() {
    let (rig, runner, head) = Rig::parked_set("shep", Rig::with_app);

    let waiting = on(&rig, &head);
    assert_eq!(
        names(&waiting),
        [
            ("kelpie / review", "completed", Some("success")),
            ("kelpie", "in_progress", None),
            ("kelpie / ruling", "in_progress", None)
        ]
    );
    assert!(waiting[2].title.contains("Merge"), "{}", waiting[1].title);

    rig.claude.script([
        Scripted::Push("work.txt", "revised\n"),
        Scripted::Text(CLEAN),
    ]);
    rig.ask(&runner, "rule", Some("71 no revert it"));
    step(&runner).unwrap();

    assert_eq!(
        names(&on(&rig, &head)),
        [
            ("kelpie / review", "completed", Some("success")),
            ("kelpie", "completed", Some("failure")),
            ("kelpie / ruling", "completed", Some("failure"))
        ]
    );
}

#[test]
fn the_summary_check_is_posted_on_every_gated_head() {
    let (rig, runner, head) = Rig::parked_set("shep", Rig::with_app);
    rig.claude.script([
        Scripted::Push("work.txt", "revised\n"),
        Scripted::Text(CLEAN),
    ]);
    rig.ask(&runner, "rule", Some("71 no revert it"));
    step(&runner).unwrap();
    step(&runner).unwrap();

    let pushed = rig.forge.head_of("kelpie/7").expect("the worker pushed");
    assert_ne!(pushed, head);
    assert_eq!(names(&on(&rig, &pushed)), [("kelpie", "in_progress", None)]);
}

#[test]
fn an_unchanged_check_is_not_posted_again() {
    let (rig, runner, _) = Rig::parked_set("shep", Rig::with_app);
    let posted = rig.github.check_run_writes();
    assert!(posted > 0);

    for _ in 0..3 {
        rig.next_look();
        step(&runner).unwrap();
    }

    assert_eq!(rig.github.check_run_writes(), posted);
}

#[test]
fn a_missing_permission_turns_check_runs_off_for_the_repo() {
    let (rig, runner, _) = Rig::with_pull_request_set("shep", |rig| {
        rig.with_app();
        rig.github.never_given(&["checks"]);
    });
    step(&runner).unwrap_or_default();
    step(&runner).unwrap_or_default();

    assert!(rig.github.check_runs().is_empty());
    let asked = (rig.github.asked().into_iter())
        .filter(|a| format!("{a:?}").contains("checks"))
        .count();
    assert_eq!(asked, 1, "the checks token is asked for once");
    assert!(!rig.github.comments().is_empty(), "comments still post");
}

// The maintainer never answered, so the head shows no failed gate.
#[test]
fn a_comment_that_withdraws_a_ruling_closes_its_checks_without_a_failure() {
    let (rig, runner, head) = Rig::parked_set("shep", Rig::with_app);
    step(&runner).unwrap();
    rig.forge.coderabbit.person_says(
        71,
        "octo-reviewer",
        Standing::Writer,
        "Please rename the flag.",
    );
    rig.clock.advance(LATE_READ);
    rig.claude.script([
        Scripted::Push("rename.txt", "renamed\n"),
        Scripted::Text(CLEAN),
    ]);
    step(&runner).unwrap(); // the comment is read
    step(&runner).unwrap(); // the worker's turn

    assert_eq!(
        names(&on(&rig, &head)),
        [
            ("kelpie / review", "completed", Some("success")),
            ("kelpie", "completed", Some("skipped")),
            ("kelpie / ruling", "completed", Some("skipped"))
        ]
    );
}

// A fix pushed after CI failed moves the item to a new head, and the old
// head's checks are not left running.
#[test]
fn a_head_the_worker_moves_on_from_leaves_no_check_running() {
    let (rig, runner, head) = Rig::with_pull_request_set("shep", Rig::with_app);
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".to_owned()]));
    rig.claude
        .script([Scripted::Push("fix.txt", "fix\n"), Scripted::Text(CLEAN)]);
    for _ in 0..4 {
        rig.next_look();
        step(&runner).unwrap();
    }

    let pushed = rig.forge.head_of("kelpie/7").expect("the worker pushed");
    assert_ne!(pushed, head);
    let old = on(&rig, &head);
    assert!(!old.is_empty());
    assert!(
        old.iter().all(|c| c.status == "completed"),
        "{:?}",
        names(&old)
    );
    assert!(on(&rig, &pushed).iter().any(|c| c.name == "kelpie"));
}
