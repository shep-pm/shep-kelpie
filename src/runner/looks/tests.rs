//! How often a pass reads the forge, through the runner's stand-ins

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
fn a_rate_limit_whose_reset_the_forge_cannot_say_holds_for_ten_minutes() {
    let rig = Rig::new("acme");
    let runner = rig.open().unwrap();
    rig.forge.set_used_up(None);

    wake(&runner);

    let held = rig.ask(&runner, "status", None)["forge_held_until"].clone();
    assert_eq!(held, rig.clock.now().0 + 600);
}
