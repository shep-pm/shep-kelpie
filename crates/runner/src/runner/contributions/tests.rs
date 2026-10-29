use std::sync::Mutex;

use serde_json::{Value, json};

use super::*;
use crate::ports::{Clock, PullRequestState};
use crate::runner::{BOARD_POLL, StepReport, step};
use crate::test::ticket;
use crate::test::{Rig, Scripted};

// The maintainer is not kelpie's account, which is `the-maintainer`
const MAINTAINER: &str = "Boss";

// A runner for a project whose App covers the repo, with `pick` set
pub(in crate::runner) fn picking(pick: &str) -> (Rig, Mutex<Runner>) {
    let rig = Rig::new("shep");
    rig.with_app();
    rig.maintainer(MAINTAINER);
    rig.edit_settings(|s| {
        format!(
            "{s}\n[app.dogs.kelpie.contributions]\npick = \"{pick}\"\nreviewers = {reviewers:?}\n",
            reviewers = crate::test::RIG_REVIEWERS,
        )
    });
    let runner = rig.open().unwrap();
    (rig, runner)
}

// Pull request `number` on a branch of its own, opened by `author`, ready
// for review. Returns its head.
pub(in crate::runner) fn opened(rig: &Rig, number: u64, author: &str) -> String {
    let branch = format!("topic/{number}");
    let head = rig.push_by_hand(&branch, &format!("{number}.txt"));
    rig.forge.open_pull_request(number, &branch, &[]);
    rig.forge.set_author(number, author);
    rig.forge.ready_pull_request(number);
    head
}

// What `status` lists as followed: each one's number, author and head
pub(in crate::runner) fn followed(rig: &Rig, runner: &Mutex<Runner>) -> Vec<(u64, String, String)> {
    let status = rig.ask(runner, "status", None);
    let listed = status["contributions"].as_array().cloned();
    listed
        .unwrap_or_default()
        .iter()
        .map(|c| {
            let text = |key: &str| c[key].as_str().unwrap().to_owned();
            (
                c["pull_request"].as_u64().unwrap(),
                text("author"),
                text("head"),
            )
        })
        .collect()
}

// The App's writes to a pull request or issue, such as a comment. Setting up
// the repo's labels is not one.
pub(in crate::runner) fn posts(rig: &Rig) -> Vec<String> {
    let writes = rig.github.writes();
    let on = |path: &&str| path.contains("/issues/") || path.contains("/pulls/");
    writes
        .iter()
        .map(|(_, call)| call.path.as_str())
        .filter(on)
        .map(str::to_owned)
        .collect()
}

fn one(number: u64, author: &str, head: &str) -> Vec<(u64, String, String)> {
    vec![(number, author.to_owned(), head.to_owned())]
}

// A step with nothing for the board to do, after the next poll is due, but for
// the alert of a ruling about the pull request
pub(in crate::runner) fn poll(rig: &Rig, runner: &Mutex<Runner>) {
    rig.next_poll(runner);
    let report = step(runner).unwrap();
    assert!(
        report.is_none() || matches!(report, Some(StepReport::Alerted { id }) if id == ticket(30)),
        "{report:?}"
    );
}

// The same, for a poll that may also start and run the work item a release
// adopted: every report it takes besides the ruling's names the pull request
// that was handed over
pub(in crate::runner) fn poll_or_adopt(rig: &Rig, runner: &Mutex<Runner>) {
    rig.next_poll(runner);
    let report = step(runner).unwrap();
    let taken_over = (serde_json::to_value(&report).unwrap().get("pull_request"))
        .and_then(Value::as_u64)
        == Some(30);
    assert!(
        report.is_none()
            || matches!(report, Some(StepReport::Alerted { id }) if id == ticket(30))
            || taken_over,
        "{report:?}"
    );
}

#[test]
fn a_pull_request_from_another_person_is_a_contribution_under_pick_all() {
    let (rig, runner) = picking("all");
    let head = opened(&rig, 30, "newcomer");

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(followed(&rig, &runner), one(30, "newcomer", &head));
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["contributions"][0]["seen"], rig.clock.now().0);
    assert_eq!(posts(&rig), Vec::<String>::new(), "nothing is posted");
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn only_a_labelled_one_is_a_contribution_under_pick_label() {
    let (rig, runner) = picking("label");
    let head = opened(&rig, 30, "newcomer");
    opened(&rig, 31, "another");
    rig.forge.label_pull_request(31, "bug");

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(followed(&rig, &runner), []);

    rig.forge.label_pull_request(30, "ready-for-agent");
    poll(&rig, &runner);
    assert_eq!(followed(&rig, &runner), one(30, "newcomer", &head));
}

#[test]
fn nothing_is_a_contribution_under_pick_off_and_a_labelled_fork_is_refused_as_before() {
    let rig = Rig::new("shep");
    rig.with_app();
    let runner = rig.open().unwrap();
    let head = rig.push_by_hand("fix/timeline", "work.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    rig.forge.set_author(80, "newcomer");
    rig.forge.ready_pull_request(80);
    rig.forge.set_from_fork(80);
    rig.forge.label_pull_request(80, "ready-for-agent");
    assert!(!head.is_empty());

    assert_eq!(
        step(&runner).unwrap(),
        Some(StepReport::AdoptRefused {
            pull_request: 80,
            reason: "pull request #80 comes from a fork".into(),
            comment_failed: None,
        })
    );
    assert_eq!(followed(&rig, &runner), []);
}

#[test]
fn the_maintainers_kelpies_and_a_bots_pull_requests_are_not_contributions() {
    let (rig, runner) = picking("all");
    // The maintainer's login is written in another case than the forge's
    opened(&rig, 40, "boss");
    opened(&rig, 41, "The-Maintainer");
    opened(&rig, 42, "dependabot");
    rig.forge.set_author_bot(42);
    let head = opened(&rig, 43, "newcomer");

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(followed(&rig, &runner), one(43, "newcomer", &head));
}

#[test]
fn a_draft_becomes_a_contribution_when_it_is_marked_ready() {
    let (rig, runner) = picking("all");
    let head = opened(&rig, 30, "newcomer");
    rig.forge.set_draft(30, true);

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(followed(&rig, &runner), []);

    rig.forge.ready_pull_request(30);
    poll(&rig, &runner);
    assert_eq!(followed(&rig, &runner), one(30, "newcomer", &head));

    rig.forge.set_draft(30, true);
    poll(&rig, &runner);
    assert_eq!(
        followed(&rig, &runner),
        [],
        "a draft again leaves the record"
    );
}

#[test]
fn a_fork_branch_named_kelpie_slash_5_is_still_a_contribution() {
    let (rig, runner) = picking("all");
    let head = rig.push_by_hand("kelpie/5", "five.txt");
    rig.forge.open_pull_request(30, "kelpie/5", &[]);
    rig.forge.set_author(30, "newcomer");
    rig.forge.set_from_fork(30);
    rig.forge.ready_pull_request(30);

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(followed(&rig, &runner), one(30, "newcomer", &head));
}

#[test]
fn a_labelled_contribution_is_not_refused_by_adoption_under_pick_label() {
    let (rig, runner) = picking("label");
    rig.push_by_hand("fix/timeline", "work.txt");
    rig.forge.open_pull_request(80, "fix/timeline", &[5]);
    rig.forge.set_author(80, "newcomer");
    rig.forge.ready_pull_request(80);
    rig.forge.set_from_fork(80);
    rig.forge.label_pull_request(80, "ready-for-agent");

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(rig.forge.pull_request_labels(80), ["ready-for-agent"]);
    assert_eq!(posts(&rig), Vec::<String>::new(), "no refusal comment");
    let status = rig.ask(&runner, "status", None);
    assert_eq!(status["adopted"], json!([]));
    assert_eq!(status["contributions"][0]["pull_request"], 80);
}

#[test]
fn a_labelled_draft_from_another_person_is_not_refused_by_adoption() {
    for pick in ["label", "all"] {
        let (rig, runner) = picking(pick);
        let head = opened(&rig, 30, "newcomer");
        rig.forge.set_draft(30, true);
        rig.forge.set_from_fork(30);
        rig.forge.label_pull_request(30, "ready-for-agent");

        assert_eq!(step(&runner).unwrap(), None);

        assert_eq!(rig.forge.pull_request_labels(30), ["ready-for-agent"]);
        assert_eq!(posts(&rig), Vec::<String>::new(), "no refusal comment");
        assert_eq!(followed(&rig, &runner), []);

        rig.forge.ready_pull_request(30);
        poll(&rig, &runner);
        assert_eq!(followed(&rig, &runner), one(30, "newcomer", &head));
    }
}

#[test]
fn a_new_head_updates_the_record() {
    let (rig, runner) = picking("all");
    let first = opened(&rig, 30, "newcomer");
    assert_eq!(step(&runner).unwrap(), None);
    let seen = rig.ask(&runner, "status", None)["contributions"][0]["seen"].clone();

    let second = rig.push_by_hand("topic/30", "more.txt");
    assert_ne!(first, second);
    poll(&rig, &runner);

    assert_eq!(followed(&rig, &runner), one(30, "newcomer", &second));
    let status = rig.ask(&runner, "status", None);
    assert_ne!(status["contributions"][0]["seen"], seen);
    assert_eq!(
        status["contributions"][0]["seen"],
        rig.clock.now().0,
        "seen when the new head was"
    );
}

#[test]
fn a_closed_merged_or_unlabelled_contribution_leaves_the_record_with_no_post() {
    let (rig, runner) = picking("label");
    for number in [30, 31, 32, 33] {
        opened(&rig, number, "newcomer");
        rig.forge.label_pull_request(number, "ready-for-agent");
    }
    assert_eq!(step(&runner).unwrap(), None);
    let numbers = |rig: &Rig| -> Vec<u64> { followed(rig, &runner).iter().map(|c| c.0).collect() };
    assert_eq!(numbers(&rig), [30, 31, 32, 33]);

    rig.forge.set_state(30, PullRequestState::Merged);
    rig.forge.set_state(31, PullRequestState::Closed);
    rig.forge.unlabel_pull_request(32, "ready-for-agent");
    poll(&rig, &runner);

    assert_eq!(numbers(&rig), [33]);
    assert_eq!(posts(&rig), Vec::<String>::new(), "nothing is posted");
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn a_contribution_is_followed_while_every_slot_is_held() {
    let (rig, runner) = picking("all");
    rig.ask(&runner, "add", Some("7"));
    assert!(
        !runner.lock().unwrap().board_open(),
        "the one slot is taken"
    );
    let head = opened(&rig, 30, "newcomer");
    rig.claude.script([Scripted::Say("Working on it.\n")]);

    assert!(step(&runner).unwrap().is_some(), "the item's turn ran");

    assert_eq!(followed(&rig, &runner), one(30, "newcomer", &head));
}

#[test]
fn contributions_are_followed_once_a_poll_from_the_listing_the_board_reads() {
    let (rig, runner) = picking("all");
    opened(&rig, 30, "newcomer");
    let listed = || {
        let reads = rig.forge.pull_request_reads();
        reads.listed.load(std::sync::atomic::Ordering::SeqCst)
    };

    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(listed(), 1, "following and the board share the poll's read");

    rig.clock.advance(BOARD_POLL.as_secs() - 1);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(listed(), 1, "a poll has not passed");

    rig.clock.advance(1);
    assert_eq!(step(&runner).unwrap(), None);
    assert_eq!(listed(), 2);
}

#[test]
fn a_runner_that_picks_nothing_follows_nothing_and_reads_nothing() {
    let rig = Rig::new("shep");
    opened(&rig, 30, "newcomer");
    let runner = rig.open().unwrap();

    assert_eq!(step(&runner).unwrap(), None);

    assert_eq!(followed(&rig, &runner), []);
    let status: Value = rig.ask(&runner, "status", None);
    assert_eq!(status.get("contributions"), None);
}

#[test]
fn status_lists_each_contribution_with_its_number_author_and_head() {
    let (rig, runner) = picking("all");
    let first = opened(&rig, 30, "newcomer");
    let second = opened(&rig, 31, "another");

    assert_eq!(step(&runner).unwrap(), None);

    let status = rig.ask(&runner, "status", None);
    let seen = rig.clock.now().0;
    assert_eq!(
        status["contributions"],
        json!([
            { "pull_request": 30, "author": "newcomer", "head": first, "seen": seen },
            { "pull_request": 31, "author": "another", "head": second, "seen": seen },
        ])
    );
}
