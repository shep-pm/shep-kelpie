//! How often a pass reads the forge, through the runner's stand-ins

use std::time::Duration;

use crate::ports::{Clock, Timestamp};
use crate::runner::{BOARD_POLL, Pass, StepReport, advance, step};
use crate::test::Rig;

// The loop's pass after a wake, whatever woke it
fn wake(runner: &std::sync::Mutex<crate::runner::Runner>) -> Option<StepReport> {
    match advance(runner).unwrap() {
        Pass::Report(report) => Some(report),
        Pass::Idle | Pass::Started => None,
    }
}

#[test]
fn many_wakes_in_a_minute_read_the_board_once() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    let before = rig.forge.board_reads();
    for _ in 0..30 {
        wake(&runner);
    }
    assert_eq!(rig.forge.board_reads(), before + 1);

    rig.clock.advance(BOARD_POLL.as_secs() - 1);
    wake(&runner);
    assert_eq!(
        rig.forge.board_reads(),
        before + 1,
        "read inside the minute"
    );
    rig.clock.advance(1);
    for _ in 0..30 {
        wake(&runner);
    }
    assert_eq!(rig.forge.board_reads(), before + 2);
}

#[test]
fn a_work_item_ending_lets_the_board_be_read_at_once() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    // Draining, so #7 starts no call while it holds the one slot.
    rig.ask(&runner, "drain", None);
    rig.ask(&runner, "add", Some("7"));
    wake(&runner);
    let before = rig.forge.board_reads();
    rig.forge.list_ready(8, false);
    rig.ask(&runner, "drop", Some("7"));

    let report = wake(&runner);

    assert_eq!(rig.forge.board_reads(), before + 1);
    assert!(
        matches!(report, Some(StepReport::Dispatched { issue: 8, .. })),
        "{report:?}"
    );
}

#[test]
fn a_failing_gate_is_retried_on_a_growing_wait_that_a_success_ends() {
    let (rig, runner, _) = Rig::with_pull_request("acme");
    rig.forge.set_down(true);
    let failed = |report| matches!(report, Some(StepReport::GateFailed { issue: 7, .. }));
    assert!(failed(step(&runner).unwrap()));

    for wait in [15, 30, 60] {
        let asked = rig.forge.asked();
        assert_eq!(step(&runner).unwrap(), None);
        rig.clock.advance(wait - 1);
        assert_eq!(step(&runner).unwrap(), None);
        assert_eq!(
            rig.forge.asked(),
            asked,
            "the forge was asked inside a wait"
        );
        rig.clock.advance(1);
        assert!(failed(step(&runner).unwrap()), "no retry after {wait}s");
        assert!(rig.forge.asked() > asked);
    }

    rig.forge.set_down(false);
    rig.clock.advance(120);
    assert_eq!(step(&runner).unwrap(), None, "CI is read, and pending");
    rig.forge.set_down(true);
    assert!(failed(step(&runner).unwrap()));
    rig.clock.advance(15);
    assert!(
        failed(step(&runner).unwrap()),
        "the wait did not start over"
    );
}

#[test]
fn a_used_up_rate_limit_holds_every_forge_call_until_it_resets_and_says_so_once() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    runner.lock().unwrap().take_notes();
    let reset = Timestamp(rig.clock.now().0 + 1800);
    rig.forge.set_used_up(Some(reset));

    assert!(matches!(
        wake(&runner),
        Some(StepReport::BoardFailed { .. })
    ));
    let asked = rig.forge.asked();
    let notes = runner.lock().unwrap().take_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    let until = jiff::Timestamp::from_second(i64::try_from(reset.0).unwrap()).unwrap();
    assert!(
        notes[0].ends_with(&format!("makes no forge call until {until}")),
        "{}",
        notes[0]
    );
    assert_eq!(
        rig.ask(&runner, "status", None)["forge_held_until"],
        reset.0
    );

    let refused = rig.ask(&runner, "add", Some("8"));
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("rate limit is used up until"),
        "{refused}"
    );
    for _ in 0..29 {
        rig.clock.advance(60);
        wake(&runner);
    }
    rig.clock.advance(59);
    wake(&runner);
    assert_eq!(rig.forge.asked(), asked, "a held call reached the forge");
    assert_eq!(runner.lock().unwrap().take_notes(), Vec::<String>::new());

    rig.forge.set_down(false);
    rig.clock.advance(1);
    assert_eq!(wake(&runner), None);
    assert!(rig.forge.asked() > asked);
    assert_eq!(
        rig.ask(&runner, "status", None).get("forge_held_until"),
        None
    );
}

#[test]
fn the_hold_is_told_in_turn_with_the_notes_after_it() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    runner.lock().unwrap().take_notes();
    rig.forge
        .set_used_up(Some(Timestamp(rig.clock.now().0 + 1800)));
    wake(&runner);
    // A stray file in the answers folder is a note of its own, made with no forge call.
    let answers = rig.paths().answers;
    std::fs::create_dir_all(&answers).unwrap();
    std::fs::write(answers.join("stray"), "").unwrap();
    wake(&runner);

    let notes = runner.lock().unwrap().take_notes();

    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(
        notes[0].starts_with("the forge's rate limit is used up"),
        "{notes:?}"
    );
    assert!(notes[1].contains("stray"), "{notes:?}");
}

#[test]
fn a_rate_limit_whose_reset_the_forge_cannot_say_holds_for_ten_minutes() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.forge.set_used_up(None);

    wake(&runner);

    let held = rig.ask(&runner, "status", None)["forge_held_until"].clone();
    assert_eq!(held, rig.clock.now().0 + 600);
}

// After an idle pass, nothing the runner waits on is due already, so the
// loop sleeps rather than spins.

#[test]
fn a_board_shut_by_a_full_slot_sets_no_wait_past_its_last_read() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    wake(&runner);
    rig.ask(&runner, "drain", None);
    rig.ask(&runner, "add", Some("7"));
    rig.clock.advance(3 * BOARD_POLL.as_secs());

    assert_eq!(wake(&runner), None);

    assert_eq!(runner.lock().unwrap().next_look(), None);
}

#[test]
fn a_hold_longer_than_a_minute_waits_for_its_end_not_the_board_read() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.forge
        .set_used_up(Some(Timestamp(rig.clock.now().0 + 1800)));
    wake(&runner);
    rig.clock.advance(BOARD_POLL.as_secs() + 1);

    assert_eq!(wake(&runner), None);

    let wait = runner.lock().unwrap().next_look();
    assert_eq!(wait, Some(Duration::from_secs(1800 - 61)));
}

#[test]
fn the_wait_of_a_work_item_that_ended_sets_no_wait() {
    let (rig, runner, _) = Rig::with_pull_request("acme");
    rig.forge.set_down(true);
    assert!(matches!(
        step(&runner).unwrap(),
        Some(StepReport::GateFailed { issue: 7, .. })
    ));
    rig.forge.set_down(false);
    rig.ask(&runner, "drop", Some("7"));
    assert_eq!(step(&runner).unwrap(), None);
    rig.clock.advance(20);

    let wait = runner.lock().unwrap().next_look();

    assert_eq!(wait, Some(Duration::from_secs(BOARD_POLL.as_secs() - 20)));
}
