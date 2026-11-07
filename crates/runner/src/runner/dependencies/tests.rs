use std::path::Path;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::super::contributions::security_tests::commit_on;
use crate::ports::{Checks, Clock, E2eOutcome};
use crate::runner::{CHECKS_SETTLE, Pass, Runner, advance, step};
use crate::state::{Bump, ProjectState, Red, RedStep, StateStore};
use crate::test::{CALL_PATIENCE, Hold, Rig};

// A runner for a project with `[app.dogs.kelpie.dependencies]` set to `pick = "on"`
// and `table`'s lines
pub(in crate::runner) fn bumping(table: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    let table = format!("[app.dogs.kelpie.dependencies]\npick = \"on\"\n{table}\n");
    rig.edit_settings(|s| format!("{s}\n{table}"));
    let runner = rig.open().unwrap();
    (rig, runner)
}

pub(super) fn write(folder: &Path, file: &str, text: &str) {
    let at = folder.join(file);
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(at, text).unwrap();
}

// Pull request `number` by `author` on branch `topic/<number>`, one commit
// off `main` that changes `file`. Returns its head.
fn opened_by(rig: &Rig, number: u64, author: &str, file: &str) -> String {
    let head = commit_on(rig, number, "origin/main", |h| write(h, file, "bumped\n"));
    rig.forge
        .open_pull_request(number, &format!("topic/{number}"), &[]);
    rig.forge.set_author(number, author);
    rig.forge.ready_pull_request(number);
    head
}

pub(in crate::runner) fn bump(rig: &Rig, number: u64, file: &str) -> String {
    opened_by(rig, number, "dependabot", file)
}

pub(in crate::runner) fn poll(rig: &Rig, runner: &Mutex<Runner>) {
    rig.next_poll(runner);
    assert_eq!(step(runner).unwrap(), None);
}

// A first poll that records the bump, and one after its checks have settled
pub(in crate::runner) fn settle(rig: &Rig, runner: &Mutex<Runner>) {
    assert_eq!(step(runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(runner).unwrap(), None);
}

pub(in crate::runner) fn notes(runner: &Mutex<Runner>) -> Vec<String> {
    runner.lock().unwrap().take_notes()
}

pub(in crate::runner) fn bumps(rig: &Rig, runner: &Mutex<Runner>) -> Vec<Value> {
    let status = rig.ask(runner, "status", None);
    status["bumps"].as_array().cloned().unwrap_or_default()
}

pub(in crate::runner) fn words(notes: &[String], needle: &str) -> usize {
    notes.iter().filter(|n| n.contains(needle)).count()
}

#[test]
fn a_green_bump_is_merged_with_no_review_ruling_or_call() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Passed);

    assert_eq!(step(&runner).unwrap(), None);
    assert!(rig.forge.merges().is_empty(), "its checks have not settled");
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.forge.merges(), [(40, head)]);
    assert_eq!(rig.claude.all_seen().len(), 0, "no model read it");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["rulings"].as_array().unwrap().len(), 0);
    assert!(bumps(&rig, &runner).is_empty());
    let notes = notes(&runner);
    assert_eq!(words(&notes, "bump #40: merged"), 1, "{notes:?}");
}

#[test]
fn a_bump_waits_while_its_checks_are_pending_and_for_them_to_settle() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Pending(Vec::new()));

    settle(&rig, &runner);

    assert!(rig.forge.merges().is_empty());
    let listed = bumps(&rig, &runner);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["author"], "dependabot");
    assert_eq!(listed[0]["head"], head);
    assert_eq!(listed[0]["standing"], "waiting for its checks to finish");
}

#[test]
fn a_skipped_run_counts_as_a_pass() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Skipped);

    settle(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(40, head)]);
}

#[test]
fn a_pull_request_from_a_bot_that_is_not_listed_is_left_alone() {
    let (rig, runner) = bumping("");
    let head = opened_by(&rig, 41, "renovate", "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Passed);

    settle(&rig, &runner);

    assert!(rig.forge.merges().is_empty());
    assert!(rig.forge.comments().is_empty());
    assert!(bumps(&rig, &runner).is_empty());
}

#[test]
fn a_listed_bot_is_matched_ignoring_case_and_a_bot_suffix() {
    let (rig, runner) = bumping("bots = [\"Renovate[bot]\"]");
    let head = opened_by(&rig, 41, "renovate", "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Passed);

    settle(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(41, head)]);
}

#[test]
fn bumps_are_left_alone_unless_the_project_picks_them() {
    let rig = Rig::new("shep");
    let runner = rig.open().unwrap();
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Passed);

    settle(&rig, &runner);

    assert!(rig.forge.merges().is_empty());
    assert!(rig.forge.comments().is_empty());
    assert!(bumps(&rig, &runner).is_empty());
}

#[test]
fn a_bump_behind_main_is_asked_to_rebase_once_for_each_head() {
    let (rig, runner) = bumping("");
    let stale = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&stale, Checks::Passed);
    rig.land("main.txt", "main moved on\n");

    settle(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(
        rig.forge.comments(),
        [(40, "@dependabot rebase".to_owned())]
    );
    assert!(
        rig.forge.merges().is_empty(),
        "nothing is proved on a stale head"
    );
    let listed = bumps(&rig, &runner);
    assert!(listed[0]["standing"].as_str().unwrap().contains("behind"));
    assert!(
        listed[0]["standing"]
            .as_str()
            .unwrap()
            .contains("requires it to be up to date"),
        "the standing names the rule that asked it"
    );

    let fresh = commit_on(&rig, 40, "origin/main", |h| write(h, "Cargo.lock", "b\n"));
    rig.land("main_again.txt", "and again\n");
    poll(&rig, &runner);

    assert_ne!(fresh, stale);
    assert_eq!(rig.forge.comments().len(), 2, "the new head is asked once");
    let notes = notes(&runner);
    assert_eq!(words(&notes, "bump #40: asked dependabot to rebase"), 2);
}

#[test]
fn a_rebase_is_asked_of_the_bots_name_without_its_bot_suffix() {
    let (rig, runner) = bumping("");
    let head = opened_by(&rig, 40, "dependabot[bot]", "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Passed);
    rig.land("main.txt", "main moved on\n");

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(
        rig.forge.comments(),
        [(40, "@dependabot rebase".to_owned())]
    );
}

#[test]
fn a_rebased_bump_is_proved_afresh_on_its_new_head() {
    let (rig, runner) = bumping("");
    let stale = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&stale, Checks::Passed);
    rig.land("main.txt", "main moved on\n");
    assert_eq!(step(&runner).unwrap(), None);

    let fresh = commit_on(&rig, 40, "origin/main", |h| write(h, "Cargo.lock", "b\n"));
    rig.forge.set_checks(&fresh, Checks::Passed);
    poll(&rig, &runner);
    assert_eq!(
        rig.forge.merges(),
        [],
        "the new head's checks have not settled"
    );
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.forge.merges(), [(40, fresh)]);
}

#[test]
fn a_bump_that_cannot_be_asked_to_rebase_is_asked_at_the_next_poll() {
    let (rig, runner) = bumping("");
    bump(&rig, 40, "Cargo.lock");
    rig.land("main.txt", "main moved on\n");
    rig.forge.set_comments_down(true);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(rig.forge.comments().is_empty());

    rig.forge.set_comments_down(false);
    poll(&rig, &runner);

    assert_eq!(
        rig.forge.comments(),
        [(40, "@dependabot rebase".to_owned())]
    );
}

#[test]
fn a_web_bump_is_merged_after_a_passing_e2e_run_of_its_head() {
    let (rig, runner) = bumping("web = [\"web/\"]");
    let head = bump(&rig, 50, "web/package-lock.json");
    rig.forge.set_checks(&head, Checks::Passed);

    settle(&rig, &runner);

    let ran = rig.e2e.ran();
    assert_eq!(ran.len(), 1);
    assert!(ran[0].checked_out, "the head was checked out for the run");
    assert!(ran[0].run.worktree.ends_with("worktrees/bump-50"));
    assert!(!ran[0].run.worktree.exists(), "and removed after it");
    assert!(rig.forge.merges().is_empty(), "the run has just ended");

    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(50, head)]);
    let notes = notes(&runner);
    assert_eq!(words(&notes, "bump #50: e2e passed"), 1, "{notes:?}");
    assert_eq!(words(&notes, "bump #50: merged"), 1, "{notes:?}");
}

#[test]
fn a_bump_that_changes_no_web_path_is_merged_with_no_e2e_run() {
    let (rig, runner) = bumping("web = [\"web/\", \"package.json\"]");
    let head = bump(&rig, 50, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Passed);

    settle(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(50, head)]);
    assert!(rig.e2e.ran().is_empty());
}

#[test]
fn a_bump_that_moves_a_web_file_out_of_the_web_paths_is_still_a_web_bump() {
    let (rig, runner) = bumping("web = [\"web/\"]");
    rig.land("web/app.js", "app\n");
    let moved = commit_on(&rig, 50, "origin/main", |h| {
        std::fs::rename(h.join("web/app.js"), h.join("app.js")).unwrap();
    });
    rig.forge.open_pull_request(50, "topic/50", &[]);
    rig.forge.set_author(50, "dependabot");
    rig.forge.ready_pull_request(50);
    rig.forge.set_checks(&moved, Checks::Passed);

    settle(&rig, &runner);

    assert_eq!(rig.e2e.ran().len(), 1, "the move is a change to a web path");
}

#[test]
fn a_green_web_bump_starts_no_e2e_run_while_the_runner_drains() {
    let (rig, runner) = bumping("web = [\"web/\"]");
    let head = bump(&rig, 50, "web/package-lock.json");
    rig.forge.set_checks(&head, Checks::Passed);
    rig.ask(&runner, "drain", None);

    settle(&rig, &runner);
    poll(&rig, &runner);

    assert!(rig.e2e.ran().is_empty());
    assert!(rig.forge.merges().is_empty());
    let listed = bumps(&rig, &runner);
    assert!(listed[0]["standing"].as_str().unwrap().contains("draining"));

    rig.ask(&runner, "undrain", None);
    poll(&rig, &runner);
    assert_eq!(rig.e2e.ran().len(), 1);
}

#[test]
fn an_e2e_run_in_flight_is_a_call_the_drain_waits_for() {
    let (rig, runner) = bumping("web = [\"web/\"]");
    let hold = Hold::default();
    rig.e2e.hold_next(hold.clone());
    let head = bump(&rig, 50, "web/package-lock.json");
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE);

    assert!(matches!(advance(&runner).unwrap(), Pass::Started));
    assert!(hold.entered(CALL_PATIENCE), "the run never began");
    let draining = rig.ask(&runner, "drain", None)["draining"].clone();
    assert_eq!(draining["calls"], json!([{ "role": "e2e" }]));

    hold.release();
    assert!(hold.answered(CALL_PATIENCE), "the run never ended");
    let by = std::time::Instant::now() + CALL_PATIENCE;
    while !rig.ask(&runner, "status", None)["draining"]["calls"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        assert!(std::time::Instant::now() < by, "the run never ended");
        std::thread::sleep(std::time::Duration::from_millis(10));
        step(&runner).unwrap();
    }
}

#[test]
fn a_red_bump_that_stops_qualifying_is_still_closed_and_filed() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    rig.forge.set_pull_request_closes_down(true);
    settle(&rig, &runner);
    assert!(rig.forge.pull_request_closings().is_empty());

    rig.forge.set_author(40, "someone-else");
    rig.forge.set_pull_request_closes_down(false);
    poll(&rig, &runner);

    assert_eq!(rig.forge.pull_request_closings(), [40]);
    assert_eq!(rig.forge.created().len(), 1);
    assert!(bumps(&rig, &runner).is_empty());
}

#[test]
fn a_web_bump_is_not_run_before_it_is_on_the_latest_main() {
    let (rig, runner) = bumping("web = [\"web/\"]");
    let head = bump(&rig, 50, "web/package-lock.json");
    rig.forge.set_checks(&head, Checks::Passed);
    rig.land("main.txt", "main moved on\n");

    settle(&rig, &runner);

    assert!(rig.e2e.ran().is_empty());
    assert!(rig.forge.merges().is_empty());
}

#[test]
fn a_web_bump_with_no_e2e_host_is_never_merged_and_status_says_why() {
    let (rig, runner) = bumping("web = [\"web/\"]");
    rig.e2e.script([E2eOutcome::NoHost]);
    let head = bump(&rig, 50, "web/package-lock.json");
    rig.forge.set_checks(&head, Checks::Passed);

    settle(&rig, &runner);
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert!(rig.forge.merges().is_empty());
    assert_eq!(rig.e2e.ran().len(), 1, "the dog is asked once for a head");
    let listed = bumps(&rig, &runner);
    let standing = listed[0]["standing"].as_str().unwrap();
    assert!(standing.contains("no e2e host"), "{standing}");
    assert!(
        rig.forge.pull_request_closings().is_empty(),
        "it is not red"
    );
}

#[test]
fn an_e2e_run_that_could_not_happen_is_asked_for_again() {
    let (rig, runner) = bumping("web = [\"web/\"]");
    let down = E2eOutcome::NotRun {
        reason: "the dog was not reached".to_owned(),
    };
    rig.e2e.script([down, E2eOutcome::Passed]);
    let head = bump(&rig, 50, "web/package-lock.json");
    rig.forge.set_checks(&head, Checks::Passed);

    settle(&rig, &runner);
    let listed = bumps(&rig, &runner);
    assert!(
        listed[0]["standing"]
            .as_str()
            .unwrap()
            .contains("did not happen")
    );
    poll(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(rig.e2e.ran().len(), 2);
    assert_eq!(rig.forge.merges(), [(50, head)]);
}

#[test]
fn a_failed_e2e_run_closes_the_bump_with_a_comment_and_an_issue() {
    let (rig, runner) = bumping("web = [\"web/\"]");
    rig.e2e.script([E2eOutcome::Failed {
        output: "1 failed\n".to_owned(),
    }]);
    let head = bump(&rig, 50, "web/package-lock.json");
    rig.forge.set_checks(&head, Checks::Passed);

    settle(&rig, &runner);
    poll(&rig, &runner);

    assert!(rig.forge.merges().is_empty());
    let comments = rig.forge.comments();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].0, 50);
    assert!(comments[0].1.contains("its e2e run failed"), "{comments:?}");
    assert_eq!(rig.forge.pull_request_closings(), [50]);
    let made = rig.forge.created();
    assert_eq!(made.len(), 1);
    assert_eq!(made[0].labels, ["needs-triage", "dependencies"]);
    assert!(made[0].body.contains("#50"), "{}", made[0].body);
    assert!(
        rig.forge
            .repo_labels_now()
            .contains(&"dependencies".to_owned())
    );
    assert!(
        bumps(&rig, &runner).is_empty(),
        "the issue ended the record"
    );
    let notes = notes(&runner);
    assert_eq!(words(&notes, "bump #50: e2e failed"), 1, "{notes:?}");
    assert_eq!(words(&notes, "bump #50: closed"), 1, "{notes:?}");
    assert_eq!(words(&notes, "bump #50: filed issue"), 1, "{notes:?}");
}

#[test]
fn a_red_bump_on_a_local_store_is_closed_and_filed_as_a_local_ticket() {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.dependencies]\npick = \"on\"\n"));
    rig.tickets_local();
    let runner = rig.open().unwrap();
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));

    settle(&rig, &runner);

    assert_eq!(rig.forge.pull_request_closings(), [40]);
    let comments = rig.forge.comments();
    assert_eq!(comments.len(), 1);
    assert!(comments[0].1.contains("A ticket follows"), "{comments:?}");
    assert!(rig.forge.created().is_empty());
    assert_eq!(rig.forge.issue_writes(), Vec::<String>::new());
    assert!(bumps(&rig, &runner).is_empty());
    let listed = rig.ticket_verb(&["list"]);
    let want = "k1  open  dependencies: #40, topic/40, broke [needs-triage, dependencies]";
    assert!(listed.contains(&want.to_owned()), "{listed:?}");
    let notes = notes(&runner);
    assert_eq!(words(&notes, "bump #40: filed ticket k1"), 1, "{notes:?}");
}

#[test]
fn a_red_bump_gets_its_comment_its_close_and_its_issue() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into(), "lint".into()]));

    settle(&rig, &runner);

    assert!(rig.forge.merges().is_empty());
    let comments = rig.forge.comments();
    assert_eq!(comments.len(), 1);
    assert!(
        comments[0].1.contains("check `test` failed"),
        "{comments:?}"
    );
    assert!(
        comments[0].1.contains("check `lint` failed"),
        "{comments:?}"
    );
    assert_eq!(rig.forge.pull_request_closings(), [40]);
    let made = rig.forge.created();
    assert_eq!(made.len(), 1);
    assert!(made[0].title.contains("#40"), "{}", made[0].title);
    assert!(
        made[0].body.contains("check `lint` failed"),
        "{}",
        made[0].body
    );
    let notes = notes(&runner);
    assert_eq!(words(&notes, "bump #40: red"), 1, "{notes:?}");
    assert_eq!(words(&notes, "closing it"), 1, "{notes:?}");
}

// A bump a build that had `on_red = "leave"` saved as red and left open, at `head`
fn saved_as_left(rig: &Rig, number: u64, head: &str) {
    let mut left = Bump::new(
        number,
        "dependabot".to_owned(),
        format!("topic/{number}"),
        head.to_owned(),
        rig.clock.now(),
    );
    left.red = Some(Red {
        head: head.to_owned(),
        why: vec!["check `test` failed".to_owned()],
        step: RedStep::Left,
    });
    let mut state = ProjectState::new();
    state.bumps = vec![left];
    let file = rig.paths().state;
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    StateStore::new(file).save(&state).unwrap();
}

#[test]
fn a_bump_saved_as_left_stays_left_open() {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.dependencies]\npick = \"on\"\n"));
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    saved_as_left(&rig, 40, &head);
    let runner = rig.open().unwrap();

    settle(&rig, &runner);
    poll(&rig, &runner);

    assert!(rig.forge.comments().is_empty());
    assert!(rig.forge.pull_request_closings().is_empty());
    assert!(rig.forge.created().is_empty());
    let listed = bumps(&rig, &runner);
    let standing = listed[0]["standing"].as_str().unwrap();
    assert!(standing.contains("check `test` failed"), "{standing}");
    assert!(standing.contains("left open"), "{standing}");
    let notes = notes(&runner);
    assert_eq!(words(&notes, "bump #40: red"), 0, "{notes:?}");
}

#[test]
fn a_new_head_gives_a_bump_left_open_another_chance() {
    let rig = Rig::new("shep");
    rig.edit_settings(|s| format!("{s}\n[app.dogs.kelpie.dependencies]\npick = \"on\"\n"));
    let head = bump(&rig, 40, "Cargo.lock");
    saved_as_left(&rig, 40, &head);
    let runner = rig.open().unwrap();

    let fixed = commit_on(&rig, 40, "origin/main", |h| write(h, "Cargo.lock", "b\n"));
    rig.forge.set_checks(&fixed, Checks::Passed);
    poll(&rig, &runner);
    rig.clock.advance(CHECKS_SETTLE);
    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.forge.merges(), [(40, fixed)]);
}

// A bump whose checks `checks` never finish, with `wait_hours = 2`, which
// waits an hour short of that and is red an hour past it
fn overdue(checks: Checks, words_of: &str) {
    let (rig, runner) = bumping("wait_hours = 2");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, checks);
    settle(&rig, &runner);

    rig.clock.advance(3600);
    assert_eq!(step(&runner).unwrap(), None);
    assert!(rig.forge.comments().is_empty(), "an hour is not yet two");
    assert!(rig.forge.pull_request_closings().is_empty());

    rig.clock.advance(3600);
    assert_eq!(step(&runner).unwrap(), None);
    let comments = rig.forge.comments();
    assert_eq!(comments.len(), 1);
    assert!(comments[0].1.contains(words_of), "{comments:?}");
    assert!(comments[0].1.contains("2 hours"), "{comments:?}");
    assert_eq!(rig.forge.pull_request_closings(), [40]);
    assert_eq!(rig.forge.created().len(), 1);
}

#[test]
fn checks_still_pending_after_wait_hours_are_red() {
    overdue(Checks::Pending(Vec::new()), "still pending");
}

#[test]
fn checks_that_never_start_are_red_after_wait_hours() {
    overdue(Checks::None, "no check reported");
}

#[test]
fn a_close_the_forge_refused_is_tried_again_without_a_second_comment() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    rig.forge.set_pull_request_closes_down(true);

    settle(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(rig.forge.comments().len(), 1);
    assert!(rig.forge.pull_request_closings().is_empty());
    assert!(rig.forge.created().is_empty(), "no issue before the close");
    assert_eq!(bumps(&rig, &runner).len(), 1);

    rig.forge.set_pull_request_closes_down(false);
    poll(&rig, &runner);

    assert_eq!(rig.forge.comments().len(), 1, "the comment is not repeated");
    assert_eq!(rig.forge.pull_request_closings(), [40]);
    assert_eq!(rig.forge.created().len(), 1);
}

#[test]
fn a_closed_bump_is_owed_its_issue_until_it_is_filed() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    rig.forge.set_issues_down(true);

    settle(&rig, &runner);
    poll(&rig, &runner);

    assert_eq!(rig.forge.pull_request_closings(), [40]);
    assert!(rig.forge.created().is_empty());
    assert_eq!(
        bumps(&rig, &runner).len(),
        1,
        "the closed bump is still listed"
    );

    rig.forge.set_issues_down(false);
    poll(&rig, &runner);

    assert_eq!(rig.forge.created().len(), 1);
    assert_eq!(rig.forge.comments().len(), 1);
    assert!(bumps(&rig, &runner).is_empty());
}

#[test]
fn a_bump_closed_by_someone_else_before_kelpie_closed_it_is_still_filed() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    rig.forge.set_pull_request_closes_down(true);
    settle(&rig, &runner);
    assert!(rig.forge.created().is_empty());

    rig.forge
        .set_state(40, crate::ports::PullRequestState::Closed);
    rig.forge.set_pull_request_closes_down(false);
    poll(&rig, &runner);

    assert!(
        rig.forge.pull_request_closings().is_empty(),
        "already closed"
    );
    let made = rig.forge.created();
    assert_eq!(made.len(), 1);
    assert_eq!(made[0].labels, ["needs-triage", "dependencies"]);
    assert!(bumps(&rig, &runner).is_empty());
}

#[test]
fn a_red_bump_merged_by_hand_before_kelpie_closed_it_gets_no_issue() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    rig.forge.set_pull_request_closes_down(true);
    settle(&rig, &runner);

    rig.forge
        .set_state(40, crate::ports::PullRequestState::Merged);
    poll(&rig, &runner);

    assert!(rig.forge.created().is_empty());
    assert!(rig.forge.pull_request_closings().is_empty());
    assert!(bumps(&rig, &runner).is_empty());
    let notes = notes(&runner);
    assert_eq!(
        words(&notes, "bump #40: merged by someone else"),
        1,
        "{notes:?}"
    );
}

#[test]
fn a_merge_the_forge_refused_is_tried_again() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Passed);
    rig.forge.set_merges_down(true);

    settle(&rig, &runner);
    assert!(rig.forge.merges().is_empty());
    let listed = bumps(&rig, &runner);
    assert!(
        listed[0]["standing"]
            .as_str()
            .unwrap()
            .contains("cannot merge")
    );

    rig.forge.set_merges_down(false);
    poll(&rig, &runner);

    assert_eq!(rig.forge.merges(), [(40, head)]);
}

#[test]
fn a_bump_that_left_the_board_is_forgotten() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Pending(Vec::new()));
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(bumps(&rig, &runner).len(), 1);

    rig.forge
        .set_state(40, crate::ports::PullRequestState::Closed);
    poll(&rig, &runner);

    assert!(bumps(&rig, &runner).is_empty());
}

#[test]
fn a_trigger_is_answered_while_a_bumps_pull_request_is_read() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    rig.forge.set_pull_request_closes_down(true);
    settle(&rig, &runner);
    rig.forge
        .set_state(40, crate::ports::PullRequestState::Closed);
    rig.next_look();
    let read = rig.forge.hold_next_pull_request_read();

    let (answered, pass) = rig.asked_while_held(&runner, read, "add", Some("8"));

    assert!(answered.is_some(), "the trigger waited for the read");
    pass.unwrap();
    assert_eq!(rig.forge.created().len(), 1, "the closed bump is filed");
    assert!(bumps(&rig, &runner).is_empty());
}

#[test]
fn a_trigger_is_answered_while_a_green_bump_is_merged() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE);
    let merge = rig.forge.hold_next_merge();

    let (answered, pass) = rig.asked_while_held(&runner, merge, "add", Some("8"));

    assert!(answered.is_some(), "the trigger waited for the merge");
    pass.unwrap();
    assert_eq!(rig.forge.merges(), [(40, head)]);
    assert!(bumps(&rig, &runner).is_empty());
}

#[test]
fn a_trigger_is_answered_while_a_red_bumps_pull_request_is_commented() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE);
    let comment = rig.forge.hold_next_comment();

    let (answered, pass) = rig.asked_while_held(&runner, comment, "add", Some("8"));

    assert!(answered.is_some(), "the trigger waited for the comment");
    pass.unwrap();
    assert_eq!(rig.forge.comments().len(), 1, "the comment went up");
}

#[test]
fn a_bump_closed_during_the_read_is_not_merged() {
    let (rig, runner) = bumping("");
    let head = bump(&rig, 40, "Cargo.lock");
    rig.forge.set_checks(&head, Checks::Passed);
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(CHECKS_SETTLE);
    let read = rig.forge.hold_next_open_pull_requests_read();

    let (_, pass) = rig.during_read(
        read,
        || advance(&runner),
        || {
            rig.forge
                .set_state(40, crate::ports::PullRequestState::Closed)
        },
    );

    pass.unwrap();
    assert!(rig.forge.merges().is_empty());
    poll(&rig, &runner);
    assert!(bumps(&rig, &runner).is_empty());
}

#[test]
fn a_second_green_bump_is_proved_again_after_the_first_is_merged() {
    let (rig, runner) = bumping("");
    let first = bump(&rig, 40, "Cargo.lock");
    let second = bump(&rig, 41, "package-lock.json");
    rig.forge.set_checks(&first, Checks::Passed);
    rig.forge.set_checks(&second, Checks::Passed);

    settle(&rig, &runner);

    assert_eq!(
        rig.forge.merges(),
        [(40, first.clone())],
        "the second was proved on a main the first has moved"
    );
    let listed = bumps(&rig, &runner);
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0]["standing"],
        "waiting for the next poll to read it"
    );

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.forge.merges(), [(40, first), (41, second)]);
}

#[test]
fn a_listing_the_forge_refuses_is_asked_again_a_poll_on_not_at_the_next_pass() {
    use std::sync::atomic::Ordering::SeqCst;

    let (rig, runner) = bumping("");
    rig.forge.set_down(true);
    let listed = || rig.forge.pull_request_reads().listed.load(SeqCst);

    super::read::follow_dependencies(&runner).unwrap();
    let first = listed();
    super::read::follow_dependencies(&runner).unwrap();
    assert_eq!(listed(), first, "asked again at the next pass");

    rig.next_look();
    super::read::follow_dependencies(&runner).unwrap();
    assert_eq!(listed(), first + 1, "not asked a poll on");
}
