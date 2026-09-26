//! The dog's lease book
//!
//! One holder per kind at a time. Waiters queue in arrival order, except
//! that the maintainer goes ahead of every queued runner. Nobody is ever
//! preempted: a holder keeps its lease until it gives it back or its
//! runner is gone.

use std::collections::{BTreeMap, VecDeque};

use serde::Serialize;

use super::{Epoch, Holder, LeaseKind};
use crate::ports::{Clock, Timestamp};
use crate::runner::ProjectName;

/// What asking for a lease came to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// Granted now
    Granted,
    /// Already held by the one asking, which changes nothing
    AlreadyHeld,
    /// Waiting, with this many waiters ahead
    Queued {
        /// Waiters served first
        ahead: usize,
    },
}

/// A lease that has just changed hands
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// What it is for
    pub kind: LeaseKind,
    /// Who holds it now
    pub holder: Holder,
}

/// One kind's line in `status`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseStatus {
    /// What it is for
    pub kind: LeaseKind,
    /// Who holds it, if anyone
    pub holder: Option<Holder>,
    /// Since when, if held
    pub since: Option<Timestamp>,
    /// Who waits, next first
    pub queue: Vec<Holder>,
}

#[derive(Debug, Default)]
struct Lease {
    held: Option<(Holder, Timestamp)>,
    queue: VecDeque<Holder>,
}

/// Every book lease, who holds each and who waits
pub struct LeaseBook {
    clock: Box<dyn Clock>,
    leases: BTreeMap<LeaseKind, Lease>,
}

impl std::fmt::Debug for LeaseBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseBook")
            .field("leases", &self.leases)
            .finish_non_exhaustive()
    }
}

impl LeaseBook {
    /// An empty book on `clock`
    pub fn new(clock: Box<dyn Clock>) -> Self {
        Self {
            clock,
            leases: BTreeMap::new(),
        }
    }

    /// Asks for `kind` on behalf of `holder`
    ///
    /// Asking again while waiting keeps the place already in the queue.
    pub fn ask(&mut self, kind: &LeaseKind, holder: Holder) -> Asked {
        let now = self.clock.now();
        let lease = self.leases.entry(kind.clone()).or_default();
        match &lease.held {
            None => {
                lease.held = Some((holder, now));
                return Asked::Granted;
            }
            Some((held, _)) if *held == holder => return Asked::AlreadyHeld,
            Some(_) => {}
        }
        if let Some(ahead) = lease.queue.iter().position(|w| *w == holder) {
            return Asked::Queued { ahead };
        }
        let ahead = match holder {
            Holder::Maintainer => 0,
            Holder::Runner { .. } => lease.queue.len(),
        };
        lease.queue.insert(ahead, holder);
        Asked::Queued { ahead }
    }

    /// Gives `kind` back, or withdraws from its queue, on behalf of `holder`
    ///
    /// Returns the grant to the next waiter, if the lease changed hands.
    pub fn give_back(&mut self, kind: &LeaseKind, holder: &Holder) -> Option<Grant> {
        let now = self.clock.now();
        let lease = self.leases.get_mut(kind)?;
        lease.queue.retain(|w| w != holder);
        if lease.held.as_ref().is_some_and(|(h, _)| h == holder) {
            lease.held = None;
            return grant_next(kind, lease, now);
        }
        None
    }

    /// Reclaims every lease `project`'s runner holds or waits for, except
    /// those of the run `keep`
    ///
    /// Called with the new epoch when a runner restarts, and with `None`
    /// when it is gone. Returns the grants to the next waiters.
    pub fn reclaim(&mut self, project: &ProjectName, keep: Option<Epoch>) -> Vec<Grant> {
        let now = self.clock.now();
        let stale = |h: &Holder| {
            h.is_runner_of(project)
                && !matches!(h, Holder::Runner { epoch, .. } if Some(*epoch) == keep)
        };
        let mut grants = Vec::new();
        for (kind, lease) in &mut self.leases {
            lease.queue.retain(|w| !stale(w));
            if lease.held.as_ref().is_some_and(|(h, _)| stale(h)) {
                lease.held = None;
                grants.extend(grant_next(kind, lease, now));
            }
        }
        grants
    }

    /// Who holds `kind`, if anyone
    pub fn holder(&self, kind: &LeaseKind) -> Option<&Holder> {
        self.leases.get(kind)?.held.as_ref().map(|(h, _)| h)
    }

    /// Who holds and waits for each kind anyone has asked for, by kind
    pub fn status(&self) -> Vec<LeaseStatus> {
        self.leases
            .iter()
            .map(|(kind, lease)| LeaseStatus {
                kind: kind.clone(),
                holder: lease.held.as_ref().map(|(h, _)| h.clone()),
                since: lease.held.as_ref().map(|(_, since)| *since),
                queue: lease.queue.iter().cloned().collect(),
            })
            .collect()
    }
}

fn grant_next(kind: &LeaseKind, lease: &mut Lease, now: Timestamp) -> Option<Grant> {
    let holder = lease.queue.pop_front()?;
    lease.held = Some((holder.clone(), now));
    Some(Grant {
        kind: kind.clone(),
        holder,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test::FakeClock;

    const EPOCH: u64 = 1_790_000_000;

    fn book() -> (LeaseBook, FakeClock) {
        let clock = FakeClock::at(EPOCH);
        (LeaseBook::new(Box::new(clock.clone())), clock)
    }

    fn stand_in() -> LeaseKind {
        LeaseKind::try_from("stand-in").unwrap()
    }

    fn runner(project: &str, epoch: u64) -> Holder {
        Holder::Runner {
            project: ProjectName::try_from(project).unwrap(),
            epoch: Epoch(epoch),
        }
    }

    fn project(name: &str) -> ProjectName {
        ProjectName::try_from(name).unwrap()
    }

    fn status(book: &LeaseBook) -> serde_json::Value {
        serde_json::to_value(book.status()).unwrap()
    }

    #[test]
    fn a_free_lease_is_granted_and_a_held_one_queues() {
        let (mut book, clock) = book();
        assert_eq!(book.ask(&stand_in(), runner("koji", 1)), Asked::Granted);
        clock.advance(5);
        assert_eq!(
            book.ask(&stand_in(), runner("reactmap", 1)),
            Asked::Queued { ahead: 0 }
        );
        assert_eq!(
            book.ask(&stand_in(), runner("golbat", 1)),
            Asked::Queued { ahead: 1 }
        );
        assert_eq!(
            status(&book),
            json!([{
                "kind": "stand-in",
                "holder": { "runner": "koji" },
                "since": EPOCH,
                "queue": [{ "runner": "reactmap" }, { "runner": "golbat" }],
            }])
        );
    }

    #[test]
    fn asking_again_changes_nothing() {
        let (mut book, _) = book();
        book.ask(&stand_in(), runner("koji", 1));
        book.ask(&stand_in(), runner("reactmap", 1));
        book.ask(&stand_in(), runner("golbat", 1));
        assert_eq!(book.ask(&stand_in(), runner("koji", 1)), Asked::AlreadyHeld);
        assert_eq!(
            book.ask(&stand_in(), runner("golbat", 1)),
            Asked::Queued { ahead: 1 }
        );
        assert_eq!(status(&book)[0]["queue"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_lease_given_back_goes_to_the_next_waiter_from_then() {
        let (mut book, clock) = book();
        book.ask(&stand_in(), runner("koji", 1));
        book.ask(&stand_in(), runner("reactmap", 1));
        clock.advance(90);
        assert_eq!(
            book.give_back(&stand_in(), &runner("koji", 1)),
            Some(Grant {
                kind: stand_in(),
                holder: runner("reactmap", 1)
            })
        );
        let line = &status(&book)[0];
        assert_eq!(
            (&line["holder"], &line["since"]),
            (&json!({ "runner": "reactmap" }), &json!(EPOCH + 90))
        );
        assert_eq!(book.give_back(&stand_in(), &runner("reactmap", 1)), None);
        assert_eq!(status(&book)[0]["holder"], json!(null));
    }

    #[test]
    fn the_maintainer_goes_ahead_of_queued_runners_without_preempting() {
        let (mut book, _) = book();
        book.ask(&stand_in(), runner("koji", 1));
        book.ask(&stand_in(), runner("reactmap", 1));
        assert_eq!(
            book.ask(&stand_in(), Holder::Maintainer),
            Asked::Queued { ahead: 0 }
        );
        assert_eq!(status(&book)[0]["holder"], json!({ "runner": "koji" }));

        let next = book.give_back(&stand_in(), &runner("koji", 1)).unwrap();
        assert_eq!(next.holder, Holder::Maintainer);
        let next = book.give_back(&stand_in(), &Holder::Maintainer).unwrap();
        assert_eq!(next.holder, runner("reactmap", 1));
    }

    #[test]
    fn the_maintainer_is_granted_a_free_lease_at_once() {
        let (mut book, _) = book();
        assert_eq!(book.ask(&stand_in(), Holder::Maintainer), Asked::Granted);
        assert_eq!(status(&book)[0]["holder"], json!("maintainer"));
    }

    #[test]
    fn a_waiter_that_gives_back_leaves_the_queue() {
        let (mut book, _) = book();
        book.ask(&stand_in(), runner("koji", 1));
        book.ask(&stand_in(), Holder::Maintainer);
        assert_eq!(book.give_back(&stand_in(), &Holder::Maintainer), None);
        assert_eq!(status(&book)[0]["queue"], json!([]));
    }

    #[test]
    fn a_restarted_runner_loses_its_lease_to_the_next_waiter() {
        let (mut book, clock) = book();
        book.ask(&stand_in(), runner("koji", 1));
        book.ask(&stand_in(), runner("reactmap", 1));
        clock.advance(30);
        assert_eq!(
            book.reclaim(&project("koji"), Some(Epoch(2))),
            [Grant {
                kind: stand_in(),
                holder: runner("reactmap", 1)
            }]
        );
        assert_eq!(status(&book)[0]["since"], EPOCH + 30);
    }

    #[test]
    fn a_restart_keeps_what_the_new_run_asked_for() {
        let (mut book, _) = book();
        book.ask(&stand_in(), runner("reactmap", 1));
        book.ask(&stand_in(), runner("koji", 1));
        book.ask(&stand_in(), runner("koji", 2));
        assert_eq!(book.reclaim(&project("koji"), Some(Epoch(2))), []);
        assert_eq!(
            book.give_back(&stand_in(), &runner("reactmap", 1)),
            Some(Grant {
                kind: stand_in(),
                holder: runner("koji", 2)
            })
        );
    }

    #[test]
    fn a_runner_that_is_gone_loses_every_lease_and_place() {
        let (mut book, _) = book();
        let other = LeaseKind::try_from("other").unwrap();
        book.ask(&stand_in(), runner("koji", 1));
        book.ask(&stand_in(), runner("reactmap", 1));
        book.ask(&other, runner("reactmap", 1));
        book.ask(&other, runner("koji", 1));
        let grants = book.reclaim(&project("reactmap"), None);
        assert_eq!(
            grants,
            [Grant {
                kind: other.clone(),
                holder: runner("koji", 1)
            }]
        );
        assert_eq!(
            book.give_back(&stand_in(), &runner("koji", 1)),
            None,
            "reactmap's place in the queue went with it"
        );
    }

    #[test]
    fn reclaiming_a_runner_leaves_the_maintainer_and_other_runners_alone() {
        let (mut book, _) = book();
        book.ask(&stand_in(), Holder::Maintainer);
        book.ask(&stand_in(), runner("reactmap", 1));
        assert_eq!(book.reclaim(&project("koji"), None), []);
        assert_eq!(status(&book)[0]["queue"], json!([{ "runner": "reactmap" }]));
    }

    #[test]
    fn a_book_nobody_asked_is_empty() {
        let (book, _) = book();
        assert_eq!(status(&book), json!([]));
    }
}
