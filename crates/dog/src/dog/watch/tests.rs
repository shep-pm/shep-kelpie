//! The dog's lease and summon series, read off a desk with a fake clock

use std::num::NonZeroU32;

use crate::dog::desk::tests::{World, stand_in, world};
use crate::lease::counted::{Taker, Ticket};
use crate::lease::wire::{Asker, WindowFact};
use crate::lease::{Epoch, LeaseKind};
use crate::test::EPOCH;

const HELD: &str = "kelpie_leases_held";
const WAITING: &str = "kelpie_lease_waiting";
const GRANTS: &str = "kelpie_lease_grants_total";
const WAITED: &str = "kelpie_lease_wait_seconds_total";
const SUMMONS: &str = "kelpie_summons_total";

fn taker(n: u64) -> Taker {
    Taker {
        ticket: Ticket(n),
        pid: 1000 + u32::try_from(n).unwrap(),
        what: format!("cargo test {n}"),
    }
}

impl World {
    fn kind(&self, name: &str, series: &str) -> Option<f64> {
        self.samples.value(series, &[("kind", name)])
    }

    fn summons(&self, bot: &str, outcome: &str) -> Option<f64> {
        let labels = [("bot", bot), ("outcome", outcome)];
        self.samples.value(SUMMONS, &labels)
    }
}

#[test]
fn a_desk_with_a_book_lease_in_it_reads_it_free() {
    let w = world();

    assert_eq!(w.kind("coderabbit", HELD), Some(0.0));
    assert_eq!(w.kind("coderabbit", WAITING), Some(0.0));
    assert_eq!(w.kind("coderabbit", GRANTS), None, "nothing granted yet");
}

#[test]
fn a_book_lease_reads_held_and_waiting_and_adds_each_wait_to_the_grant_that_ended_it() {
    let mut w = world();
    let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));

    w.raise("koji", koji.want(&stand_in()));
    assert_eq!(w.kind("stand-in", HELD), Some(1.0));
    assert_eq!(w.kind("stand-in", WAITING), Some(0.0));
    assert_eq!(w.kind("stand-in", GRANTS), Some(1.0));
    assert_eq!(
        w.kind("stand-in", WAITED),
        Some(0.0),
        "a free lease waits 0"
    );

    w.raise("reactmap", reactmap.want(&stand_in()));
    assert_eq!(w.kind("stand-in", WAITING), Some(1.0));
    assert_eq!(w.kind("stand-in", GRANTS), Some(1.0), "not granted yet");

    w.clock.advance(40);
    w.raise("koji", koji.give_back(&stand_in()));
    assert_eq!(w.kind("stand-in", HELD), Some(1.0), "reactmap holds it");
    assert_eq!(w.kind("stand-in", WAITING), Some(0.0));
    assert_eq!(w.kind("stand-in", GRANTS), Some(2.0));
    assert_eq!(w.kind("stand-in", WAITED), Some(40.0));
}

#[test]
fn the_same_total_sent_again_adds_no_grant() {
    let mut w = world();
    let mut koji = Asker::new(Epoch(101));
    w.raise("koji", koji.want(&stand_in()));
    for note in koji.notes() {
        w.raise("koji", note);
    }

    assert_eq!(w.kind("stand-in", GRANTS), Some(1.0));
}

#[test]
fn a_waiter_whose_runner_is_gone_leaves_no_wait_to_count_against_the_next() {
    let mut w = world();
    let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
    w.raise("koji", koji.want(&stand_in()));
    w.raise("reactmap", reactmap.want(&stand_in()));
    w.clock.advance(500);

    w.desk.runner_is("reactmap", None);
    assert_eq!(w.kind("stand-in", WAITING), Some(0.0));
    assert_eq!(
        w.kind("stand-in", GRANTS),
        Some(1.0),
        "it was never granted"
    );

    // The same runner comes back and waits again, from now.
    let mut again = Asker::new(Epoch(303));
    w.raise("reactmap", again.want(&stand_in()));
    w.clock.advance(7);
    w.raise("koji", koji.give_back(&stand_in()));
    assert_eq!(w.kind("stand-in", WAITED), Some(7.0));
}

#[test]
fn the_maintainers_take_and_return_move_the_series() {
    let mut w = world();
    let mut koji = Asker::new(Epoch(101));
    w.raise("koji", koji.want(&stand_in()));

    w.ask("take", Some("stand-in"));
    assert_eq!(w.kind("stand-in", WAITING), Some(1.0));
    w.clock.advance(15);
    w.raise("koji", koji.give_back(&stand_in()));

    assert_eq!(w.kind("stand-in", HELD), Some(1.0));
    assert_eq!(w.kind("stand-in", GRANTS), Some(2.0));
    assert_eq!(w.kind("stand-in", WAITED), Some(15.0));
}

#[test]
fn a_window_that_opens_grants_the_waiter_and_counts_its_wait() {
    let mut w = world();
    let cr = LeaseKind::coderabbit();
    let (mut koji, mut reactmap) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
    w.raise("koji", koji.want(&cr));
    w.raise("koji", koji.window(&cr, WindowFact::Summoned, EPOCH));
    w.raise("koji", koji.give_back(&cr));
    w.raise("reactmap", reactmap.want(&cr));
    assert_eq!(
        w.kind("coderabbit", WAITING),
        Some(1.0),
        "the hour is not up"
    );

    w.clock.advance(3600);
    w.desk.tick();

    assert_eq!(w.kind("coderabbit", HELD), Some(1.0));
    assert_eq!(w.kind("coderabbit", WAITING), Some(0.0));
    assert_eq!(w.kind("coderabbit", GRANTS), Some(2.0));
    assert_eq!(w.kind("coderabbit", WAITED), Some(3600.0));
}

#[test]
fn a_restart_keeps_what_the_book_holds_without_granting_it_again() {
    let mut w = world();
    let mut koji = Asker::new(Epoch(101));
    w.raise("koji", koji.want(&stand_in()));
    let granted = |w: &World| {
        (w.samples.sent().iter())
            .filter(|s| s.name == GRANTS)
            .count()
    };
    let before = granted(&w);

    w.restart();

    assert_eq!(w.kind("stand-in", HELD), Some(1.0));
    assert_eq!(granted(&w), before);
}

#[test]
fn cargo_test_reads_its_holders_and_waiters_and_their_wait() {
    let mut w = world();
    w.desk.set_test_capacity(NonZeroU32::new(2).unwrap());

    w.desk.take_test(taker(1));
    w.desk.take_test(taker(2));
    w.desk.take_test(taker(3));
    assert_eq!(w.kind("cargo-test", HELD), Some(2.0));
    assert_eq!(w.kind("cargo-test", WAITING), Some(1.0));
    assert_eq!(w.kind("cargo-test", GRANTS), Some(2.0));

    w.clock.advance(30);
    w.desk.leave_test(Ticket(1));

    assert_eq!(w.kind("cargo-test", HELD), Some(2.0));
    assert_eq!(w.kind("cargo-test", WAITING), Some(0.0));
    assert_eq!(w.kind("cargo-test", GRANTS), Some(3.0));
    assert_eq!(w.kind("cargo-test", WAITED), Some(30.0));
}

#[test]
fn a_raised_cargo_test_capacity_counts_the_grants_it_makes() {
    let mut w = world();
    w.desk.set_test_capacity(NonZeroU32::new(1).unwrap());
    for n in 1..=3 {
        w.desk.take_test(taker(n));
    }
    assert_eq!(w.kind("cargo-test", GRANTS), Some(1.0));
    w.clock.advance(30);

    let granted = w.desk.set_test_capacity(NonZeroU32::new(3).unwrap());

    assert_eq!(granted, [Ticket(2), Ticket(3)]);
    assert_eq!(w.kind("cargo-test", HELD), Some(3.0));
    assert_eq!(w.kind("cargo-test", WAITING), Some(0.0));
    assert_eq!(w.kind("cargo-test", GRANTS), Some(3.0));
    assert_eq!(w.kind("cargo-test", WAITED), Some(60.0));
}

#[test]
fn a_command_seated_after_a_restart_holds_the_lease_without_a_grant() {
    let mut w = world();

    w.desk.seat_test(taker(1));

    assert_eq!(w.kind("cargo-test", HELD), Some(1.0));
    assert_eq!(w.kind("cargo-test", GRANTS), None);
}

#[test]
fn every_e2e_host_reads_as_the_one_e2e_lease() {
    let mut w = world();

    w.desk.take_e2e("tokyo", taker(1));
    w.desk.take_e2e("tokyo", taker(2));
    w.desk.take_e2e("osaka", taker(3));
    assert_eq!(w.kind("e2e", HELD), Some(2.0));
    assert_eq!(w.kind("e2e", WAITING), Some(1.0));

    w.clock.advance(90);
    w.desk.leave_e2e("tokyo", Ticket(1));

    assert_eq!(w.kind("e2e", HELD), Some(2.0));
    assert_eq!(w.kind("e2e", WAITING), Some(0.0));
    assert_eq!(w.kind("e2e", GRANTS), Some(3.0));
    assert_eq!(w.kind("e2e", WAITED), Some(90.0));
}

#[test]
fn a_summon_a_runner_reports_counts_once_however_often_it_is_sent() {
    let mut w = world();
    let cr = LeaseKind::coderabbit();
    let mut koji = Asker::new(Epoch(101));
    assert_eq!(w.summons("coderabbit", "accepted"), None);

    w.raise("koji", koji.window(&cr, WindowFact::Summoned, EPOCH));
    for note in koji.notes() {
        w.raise("koji", note);
    }

    assert_eq!(w.summons("coderabbit", "accepted"), Some(1.0));
    assert_eq!(w.summons("coderabbit", "refused"), None);
}

#[test]
fn a_summon_the_window_forgot_counts_no_more_when_the_runner_sends_it_again() {
    let mut w = world();
    let cr = LeaseKind::coderabbit();
    let mut koji = Asker::new(Epoch(101));
    w.raise("koji", koji.window(&cr, WindowFact::Summoned, EPOCH));
    w.raise("koji", koji.window(&cr, WindowFact::Opens, EPOCH + 600));

    w.clock.advance(3 * 3600);
    w.desk.tick();
    for note in koji.notes() {
        w.raise("koji", note);
    }
    w.desk.tick();
    for note in koji.notes() {
        w.raise("koji", note);
    }

    assert_eq!(w.summons("coderabbit", "accepted"), Some(1.0));
    assert_eq!(w.summons("coderabbit", "refused"), Some(1.0));
}

#[test]
fn a_restart_counts_no_summon_or_refusal_again_when_the_runner_sends_them_again() {
    let mut w = world();
    let cr = LeaseKind::coderabbit();
    let mut koji = Asker::new(Epoch(101));
    w.raise("koji", koji.window(&cr, WindowFact::Summoned, EPOCH));
    w.raise("koji", koji.window(&cr, WindowFact::Opens, EPOCH + 600));
    let counted = |w: &World| {
        (w.samples.sent().iter())
            .filter(|s| s.name == SUMMONS)
            .count()
    };
    let before = counted(&w);
    assert_eq!(before, 2);

    w.restart();
    for note in koji.notes() {
        w.raise("koji", note);
    }

    assert_eq!(counted(&w), before);
}

#[test]
fn a_restart_after_the_window_forgot_a_summon_does_not_count_it_when_it_is_sent_again() {
    let mut w = world();
    let cr = LeaseKind::coderabbit();
    let mut koji = Asker::new(Epoch(101));
    w.raise("koji", koji.window(&cr, WindowFact::Summoned, EPOCH));
    w.raise("koji", koji.window(&cr, WindowFact::Opens, EPOCH + 600));
    w.clock.advance(3 * 3600);
    w.desk.tick();
    let before = w.samples.sent().len();

    w.restart();
    for note in koji.notes() {
        w.raise("koji", note);
    }

    let counted = (w.samples.sent().iter().skip(before))
        .filter(|s| s.name == SUMMONS)
        .count();
    assert_eq!(counted, 0);
}

#[test]
fn a_refusal_counts_once_and_a_new_quote_counts_again() {
    let mut w = world();
    let cr = LeaseKind::coderabbit();
    let mut koji = Asker::new(Epoch(101));

    w.raise("koji", koji.window(&cr, WindowFact::Opens, EPOCH + 600));
    for note in koji.notes() {
        w.raise("koji", note);
    }
    assert_eq!(w.summons("coderabbit", "refused"), Some(1.0));

    w.raise("koji", koji.window(&cr, WindowFact::Opens, EPOCH + 900));
    assert_eq!(w.summons("coderabbit", "refused"), Some(2.0));
}

#[test]
fn a_summon_from_a_run_already_replaced_counts_nothing() {
    let mut w = world();
    let cr = LeaseKind::coderabbit();
    let (mut old, mut new) = (Asker::new(Epoch(101)), Asker::new(Epoch(202)));
    w.raise("koji", old.want(&cr));
    w.raise("koji", new.want(&cr));

    w.raise("koji", old.window(&cr, WindowFact::Summoned, EPOCH));

    assert_eq!(w.summons("coderabbit", "accepted"), None);
}

#[test]
fn a_summon_for_a_lease_with_no_window_counts_nothing() {
    let mut w = world();
    let mut koji = Asker::new(Epoch(101));

    w.raise(
        "koji",
        koji.window(&stand_in(), WindowFact::Summoned, EPOCH),
    );

    assert_eq!(w.summons("stand-in", "accepted"), None);
}
