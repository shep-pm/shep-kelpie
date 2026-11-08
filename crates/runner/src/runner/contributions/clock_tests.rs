//! The quiet clock of a contribution, through the runner's stand-ins

use std::sync::Mutex;

use serde_json::{Value, json};

use super::clock::{Clock, Due};
use super::pass_tests::{asked, listing, read_through, write};
use super::security_tests::{commit_on, contribute, contribution};
use super::tests::{picking, poll, posts};
use crate::ports::{CLEAN, Checks, Clock as _, Finding, Severity, Timestamp};
use crate::runner::review_bot::contribution_turns::{FINDING, contributed, until_summoned};
use crate::runner::{CHECKS_SETTLE, Runner, step};
use crate::settings::Contributions;
use crate::state::{Asked, Contribution};
use crate::test::{Rig, Scripted, ScriptedRound};

const DAY: u64 = Rig::DAY;

fn found() -> Finding {
    Finding::new(
        Severity::Medium,
        "one.txt",
        1,
        "leftover placeholder",
        "it ships to users",
    )
}

fn now(rig: &Rig) -> u64 {
    rig.clock.now().0
}

// Pull request 30, read by `mine`, which found something, and the poll that
// follows: kelpie waits on the contributor from that poll
fn found_something() -> (Rig, Mutex<Runner>, String) {
    let (rig, runner) = listing(&["mine"]);
    rig.claude
        .script(std::iter::repeat_n(Scripted::Text(CLEAN), 2));
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found()])]);
    let head = contribute(&rig, 30, |h| write(h, "one.txt", "one\n"));
    read_through(&rig, &runner, &head);
    poll(&rig, &runner);
    (rig, runner, head)
}

fn waiting_since(rig: &Rig, runner: &Mutex<Runner>) -> Value {
    contribution(rig, runner)["waiting_since"].clone()
}

#[test]
fn findings_start_the_clock() {
    let (rig, runner) = listing(&["mine"]);
    rig.claude.script([Scripted::Text(CLEAN)]);
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found()])]);
    let head = contribute(&rig, 30, |h| write(h, "one.txt", "one\n"));
    read_through(&rig, &runner, &head);
    assert_eq!(waiting_since(&rig, &runner), Value::Null);

    poll(&rig, &runner);

    assert_eq!(waiting_since(&rig, &runner), now(&rig));
    assert_eq!(posts(&rig), Vec::<String>::new());
}

#[test]
fn a_bots_review_starts_the_clock() {
    let (rig, runner, head) = contributed(&["cubic", "mine"]);
    until_summoned(&rig, &runner);
    rig.forge
        .coderabbit
        .cubic_review(30, &head, now(&rig) + 30, &[FINDING]);

    read_through(&rig, &runner, &head);

    assert_eq!(waiting_since(&rig, &runner), now(&rig));
    assert_eq!(posts(&rig), Vec::<String>::new());
}

#[test]
fn red_ci_starts_the_clock_and_posts_nothing() {
    let (rig, runner) = picking("all");
    rig.ci_on_now(&runner);
    rig.claude.script([Scripted::Text(CLEAN)]);
    let head = contribute(&rig, 30, |h| write(h, "one.txt", "one\n"));
    rig.forge
        .set_checks(&head, Checks::Failed(vec!["test".into()]));
    assert_eq!(step(&runner).unwrap(), None);
    poll(&rig, &runner);
    poll(&rig, &runner);
    assert_eq!(waiting_since(&rig, &runner), Value::Null);
    rig.clock.advance(CHECKS_SETTLE);

    poll(&rig, &runner);
    let red_at = now(&rig);
    assert_eq!(
        contribution(&rig, &runner)["ci"]["state"],
        json!({"red": ["test"]})
    );
    rig.clock.advance(DAY);
    poll(&rig, &runner);

    assert_eq!(waiting_since(&rig, &runner), red_at);
    assert_eq!(posts(&rig), Vec::<String>::new());
    assert_eq!(rig.forge.comments(), []);
}

#[test]
fn a_comment_a_push_or_a_review_reply_from_the_contributor_resets_it() {
    let (rig, runner, _) = found_something();
    let started = now(&rig);

    rig.clock.advance(DAY);
    rig.writes_on(30, "Newcomer", "Fixed the placeholder");
    poll(&rig, &runner);
    let commented = now(&rig);
    assert!(commented > started);
    assert_eq!(waiting_since(&rig, &runner), commented);

    rig.clock.advance(DAY);
    let at = rig.clock.now();
    rig.forge
        .threads
        .writes_in_review(30, "newcomer", "Done here too", at);
    poll(&rig, &runner);
    let replied = now(&rig);
    assert_eq!(waiting_since(&rig, &runner), replied);

    // A push is a new head: its results start the clock again, not the push.
    rig.clock.advance(DAY);
    commit_on(&rig, 30, "topic/30", |h| write(h, "two.txt", "two\n"));
    poll(&rig, &runner);
    assert_eq!(waiting_since(&rig, &runner), Value::Null);
}

#[test]
fn a_reaction_and_the_maintainers_comment_do_not_reset_it() {
    let (rig, runner, _) = found_something();
    let started = now(&rig);

    // A reaction is no comment, push or reply, so the board reads nothing of
    // it; a poll that finds nothing new is what one looks like.
    rig.clock.advance(DAY);
    poll(&rig, &runner);
    assert_eq!(waiting_since(&rig, &runner), started);

    rig.writes_on(30, "boss", "Please add a test");
    rig.forge
        .threads
        .writes_in_review(30, "Boss", "Here too", rig.clock.now());
    rig.writes_on(30, "a-passer-by", "Same here");
    poll(&rig, &runner);

    assert_eq!(waiting_since(&rig, &runner), started);
}

#[test]
fn a_green_head_with_no_findings_clears_it() {
    let (rig, runner, first) = found_something();
    assert_ne!(waiting_since(&rig, &runner), Value::Null);
    rig.reviewer.script([ScriptedRound::Findings(Vec::new())]);

    let second = commit_on(&rig, 30, "topic/30", |h| write(h, "two.txt", "two\n"));
    assert_ne!(first, second);
    let held = read_through(&rig, &runner, &second);
    poll(&rig, &runner);

    assert_eq!(held["pass"]["findings"], Value::Null);
    assert_eq!(waiting_since(&rig, &runner), Value::Null);
    assert_eq!(asked(&rig), ["mine", "mine"]);
}

#[test]
fn due_is_nudge_after_nudge_days_and_quiet_after_quiet_days_on_a_fake_clock() {
    let mut contribution = Contribution::new(30, "newcomer".into(), "2a8f1e6".into(), Timestamp(1));
    let settings = Contributions::default();
    assert_eq!(Clock::of(&contribution, &settings), None);

    let start = Rig::EPOCH;
    contribution.waiting_since = Some(Timestamp(start));
    let clock = Clock::of(&contribution, &settings).unwrap();
    let due = |after: u64| clock.due(Timestamp(start + after));

    assert_eq!(due(0), None);
    assert_eq!(due(3 * DAY - 1), None);
    assert_eq!(due(3 * DAY), Some(Due::Nudge));
    assert_eq!(due(7 * DAY - 1), Some(Due::Nudge));
    assert_eq!(due(7 * DAY), Some(Due::Quiet));
    assert_eq!(due(30 * DAY), Some(Due::Quiet));
}

// Pull request 30 waiting on its contributor since whole day `since`, its ask
// open since whole day `asked`
fn asked_on(since: u64, asked: u64) -> Contribution {
    let at = |day: u64| Timestamp(Rig::EPOCH + day * DAY);
    let mut held = Contribution::new(30, "newcomer".into(), "2a8f1e6".into(), Timestamp(1));
    held.waiting_since = Some(at(since));
    held.asked = Some(Asked {
        comment: 7,
        at: at(asked),
        asks: 1,
        nos: 0,
        read: Vec::new(),
        closing: false,
        thanked: false,
    });
    held
}

// What `held`'s clock reports at whole day `after`
fn due_at(held: &Contribution, after: u64) -> Option<Due> {
    let clock = Clock::of(held, &Contributions::default()).unwrap();
    clock.due(Timestamp(Rig::EPOCH + after * DAY))
}

#[test]
fn an_ask_made_late_at_nudge_still_gets_the_full_notice() {
    let held = asked_on(0, 6);

    assert_eq!(due_at(&held, 9), Some(Due::Nudge), "day 9 is not quiet");
    assert_eq!(due_at(&held, 10), Some(Due::Quiet));
}

#[test]
fn an_ask_made_on_time_keeps_its_deadline() {
    let held = asked_on(0, 3);

    assert_eq!(due_at(&held, 6), Some(Due::Nudge));
    assert_eq!(due_at(&held, 7), Some(Due::Quiet));
}

#[test]
fn an_ask_made_at_quiet_keeps_the_whole_window() {
    let held = asked_on(7, 7);

    assert_eq!(due_at(&held, 13), Some(Due::Nudge));
    assert_eq!(due_at(&held, 14), Some(Due::Quiet));
}

#[test]
fn quiet_is_never_due_before_the_nudge_whatever_the_settings() {
    use std::num::NonZeroU32;
    let mut contribution = Contribution::new(30, "newcomer".into(), "2a8f1e6".into(), Timestamp(1));
    contribution.waiting_since = Some(Timestamp(Rig::EPOCH));
    let settings = Contributions {
        nudge_days: NonZeroU32::new(5).unwrap(),
        quiet_days: NonZeroU32::new(2).unwrap(),
        ..Contributions::default()
    };
    let clock = Clock::of(&contribution, &settings).unwrap();

    assert_eq!(clock.due(Timestamp(Rig::EPOCH + 3 * DAY)), None);
    assert_eq!(clock.due(Timestamp(Rig::EPOCH + 5 * DAY)), Some(Due::Quiet));
}

#[test]
fn a_comment_heard_in_the_poll_that_starts_the_clock_starts_it_at_that_poll() {
    let (rig, runner) = listing(&["mine"]);
    rig.claude.script([Scripted::Text(CLEAN)]);
    rig.reviewer
        .script([ScriptedRound::Findings(vec![found()])]);
    let head = contribute(&rig, 30, |h| write(h, "one.txt", "one\n"));
    read_through(&rig, &runner, &head);

    rig.writes_on(30, "newcomer", "Looking at it");
    poll(&rig, &runner);

    assert_eq!(waiting_since(&rig, &runner), now(&rig));
}

#[test]
fn a_reply_written_before_the_wait_began_does_not_reset_it() {
    let (rig, runner, _) = found_something();
    let started = now(&rig);
    let before = Timestamp(started - 60);

    rig.clock.advance(DAY);
    rig.forge
        .threads
        .writes_in_review(30, "newcomer", "Written before the findings", before);
    poll(&rig, &runner);

    assert_eq!(waiting_since(&rig, &runner), started);
    assert_eq!(contribution(&rig, &runner)["heard"][0]["at"], before.0);
}

#[test]
fn status_shows_the_clock_and_days_to_the_nudge() {
    let (rig, runner, _) = found_something();
    let started = now(&rig);

    rig.clock.advance(DAY);
    let held = contribution(&rig, &runner);
    assert_eq!(held["waiting_since"], started);
    assert_eq!(held["clock"]["waiting_for"], DAY);
    assert_eq!(held["clock"]["days_to_nudge"], 2);
    assert_eq!(held["clock"]["days_to_quiet"], 6);
    assert_eq!(held["clock"].get("due"), None);

    rig.clock.advance(2 * DAY);
    let held = contribution(&rig, &runner);
    assert_eq!(held["clock"]["days_to_nudge"], 0);
    assert_eq!(held["clock"]["due"], "nudge");
}

#[test]
fn a_contribution_kelpie_waits_on_no_one_for_shows_no_clock() {
    let (rig, runner) = picking("all");
    super::tests::opened(&rig, 30, "newcomer");
    assert_eq!(step(&runner).unwrap(), None);

    let held = contribution(&rig, &runner);

    assert_eq!(held.get("waiting_since"), None);
    assert_eq!(held.get("clock"), None);
}
