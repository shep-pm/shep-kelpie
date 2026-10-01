//! A counted lease: several holders at once, up to its capacity
//!
//! The `cargo-test` lease is one. Each holder is one command run under it,
//! from a worker or a shell, known by the connection it asked on. Waiters
//! queue in arrival order and nobody is preempted. A holder or waiter whose
//! connection closes, because it ended or died, gives its place back.
//! Holders are never saved: a dog restart closes every connection.

use std::collections::VecDeque;
use std::num::NonZeroU32;

use serde::Serialize;

use crate::ports::Timestamp;

/// The `cargo-test` lease's name: a share of the machine for running tests
pub const CARGO_TEST: &str = "cargo-test";

/// How many commands hold the `cargo-test` lease at once when the
/// settings do not say
pub const CARGO_TEST_CAPACITY: NonZeroU32 = NonZeroU32::new(3).expect("3 is not zero");

/// Which connection asked, numbered by the dog as it accepts them
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ticket(pub u64);

/// One command holding or waiting for a counted lease
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Taker {
    /// Its connection, which status leaves out
    #[serde(skip)]
    pub ticket: Ticket,
    /// The process that asked, as it said
    pub pid: u32,
    /// What it runs, as it said
    pub what: String,
}

/// What asking for a counted lease came to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taken {
    /// Granted now
    Granted,
    /// Waiting, with this many waiters ahead
    Queued {
        /// Waiters served first
        ahead: usize,
    },
}

/// A holder and since when
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Held {
    /// Who holds it
    #[serde(flatten)]
    pub taker: Taker,
    /// Since when
    pub since: Timestamp,
}

/// A counted lease's line in `status`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CountedStatus {
    /// What it is for
    pub kind: &'static str,
    /// How many may hold it at once
    pub capacity: NonZeroU32,
    /// Who holds it, longest first
    pub holders: Vec<Held>,
    /// Who waits, next first
    pub queue: Vec<Taker>,
}

/// A lease several commands hold at once
#[derive(Debug)]
pub struct CountedLease {
    kind: &'static str,
    capacity: NonZeroU32,
    holders: Vec<Held>,
    queue: VecDeque<Taker>,
}

impl CountedLease {
    /// The `cargo-test` lease, held by up to `capacity` at once
    pub fn cargo_test(capacity: NonZeroU32) -> Self {
        Self {
            kind: CARGO_TEST,
            capacity,
            holders: Vec::new(),
            queue: VecDeque::new(),
        }
    }

    /// Asks for the lease on behalf of `taker`, at `now`
    pub fn ask(&mut self, taker: Taker, now: Timestamp) -> Taken {
        if self.queue.is_empty() && self.has_room() {
            self.holders.push(Held { taker, since: now });
            return Taken::Granted;
        }
        self.queue.push_back(taker);
        Taken::Queued {
            ahead: self.queue.len() - 1,
        }
    }

    /// Gives the lease back, or leaves its queue, for the connection
    /// `ticket` at `now`, and returns the waiters granted the room it made
    pub fn leave(&mut self, ticket: Ticket, now: Timestamp) -> Vec<Ticket> {
        self.queue.retain(|t| t.ticket != ticket);
        self.holders.retain(|h| h.taker.ticket != ticket);
        let mut granted = Vec::new();
        while self.has_room() {
            let Some(taker) = self.queue.pop_front() else {
                break;
            };
            granted.push(taker.ticket);
            self.holders.push(Held { taker, since: now });
        }
        granted
    }

    /// Who holds and waits for it
    pub fn status(&self) -> CountedStatus {
        CountedStatus {
            kind: self.kind,
            capacity: self.capacity,
            holders: self.holders.clone(),
            queue: self.queue.iter().cloned().collect(),
        }
    }

    fn has_room(&self) -> bool {
        // A capacity past usize is room for everyone.
        usize::try_from(self.capacity.get()).map_or(true, |cap| self.holders.len() < cap)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ports::Clock;
    use crate::test::FakeClock;

    const NOW: u64 = 1_790_000_000;

    // The dog's clock, which the desk reads for every ask and leave.
    struct Lease {
        lease: CountedLease,
        clock: FakeClock,
    }

    impl Lease {
        fn ask(&mut self, n: u64) -> Taken {
            self.lease.ask(taker(n), self.clock.now())
        }

        fn leave(&mut self, n: u64) -> Vec<Ticket> {
            self.lease.leave(Ticket(n), self.clock.now())
        }
    }

    fn lease(capacity: u32) -> Lease {
        let capacity = NonZeroU32::new(capacity).unwrap();
        Lease {
            lease: CountedLease::cargo_test(capacity),
            clock: FakeClock::at(NOW),
        }
    }

    fn taker(n: u64) -> Taker {
        Taker {
            ticket: Ticket(n),
            pid: 1000 + u32::try_from(n).unwrap(),
            what: format!("cargo test {n}"),
        }
    }

    fn status(lease: &Lease) -> serde_json::Value {
        serde_json::to_value(lease.lease.status()).unwrap()
    }

    #[test]
    fn a_fourth_holder_waits_while_three_hold_it() {
        let mut lease = lease(3);
        for n in 1..=3 {
            assert_eq!(lease.ask(n), Taken::Granted, "holder {n}");
        }
        assert_eq!(lease.ask(4), Taken::Queued { ahead: 0 });
        assert_eq!(lease.ask(5), Taken::Queued { ahead: 1 });
        assert_eq!(
            status(&lease),
            json!({
                "kind": "cargo-test",
                "capacity": 3,
                "holders": [
                    { "pid": 1001, "what": "cargo test 1", "since": NOW },
                    { "pid": 1002, "what": "cargo test 2", "since": NOW },
                    { "pid": 1003, "what": "cargo test 3", "since": NOW },
                ],
                "queue": [
                    { "pid": 1004, "what": "cargo test 4" },
                    { "pid": 1005, "what": "cargo test 5" },
                ],
            })
        );
    }

    #[test]
    fn a_holder_that_leaves_lets_the_next_waiter_in_from_then() {
        let mut lease = lease(2);
        lease.ask(1);
        lease.ask(2);
        lease.ask(3);
        lease.ask(4);
        lease.clock.advance(40);
        assert_eq!(lease.leave(2), [Ticket(3)]);
        let line = status(&lease);
        assert_eq!(line["holders"][1]["pid"], 1003);
        assert_eq!(line["holders"][1]["since"], NOW + 40);
        assert_eq!(
            line["queue"],
            json!([{ "pid": 1004, "what": "cargo test 4" }])
        );
    }

    #[test]
    fn a_waiter_that_leaves_loses_its_place_and_grants_nothing() {
        let mut lease = lease(1);
        lease.ask(1);
        lease.ask(2);
        lease.ask(3);
        assert_eq!(lease.leave(2), []);
        assert_eq!(lease.leave(1), [Ticket(3)]);
        assert_eq!(lease.leave(9), [], "a stranger changes nothing");
    }

    #[test]
    fn a_lease_nobody_asked_for_is_empty() {
        let lease = lease(3);
        assert_eq!(
            status(&lease),
            json!({ "kind": "cargo-test", "capacity": 3, "holders": [], "queue": [] })
        );
    }
}
