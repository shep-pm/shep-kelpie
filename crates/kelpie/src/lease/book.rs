//! The dog's lease book
//!
//! One holder per kind at a time. Waiters queue in arrival order, except
//! that the maintainer goes ahead of every queued runner. Nobody is ever
//! preempted: a holder keeps its lease until it gives it back or its
//! runner is gone. A kind with a [`Window`] is granted only while the
//! window is open, and [`LeaseBook::tick`] grants it once it opens.

use std::collections::{BTreeMap, VecDeque};

use serde::Serialize;

use super::window::{Window, WindowStatus};
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
    /// Its review window, for a kind that has one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowStatus>,
}

#[derive(Debug, Default)]
struct Lease {
    held: Option<(Holder, Timestamp)>,
    queue: VecDeque<Holder>,
    window: Option<Window>,
}

impl Lease {
    fn open(&self, now: Timestamp) -> bool {
        self.window.as_ref().is_none_or(|w| w.opens(now).is_none())
    }
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

    /// Gives `kind` a review window, so it is granted only while that is open
    pub fn add_window(&mut self, kind: LeaseKind) {
        self.leases.entry(kind).or_default().window = Some(Window::default());
    }

    /// Takes the quota a review footer states for `kind`'s window
    ///
    /// This and the other window changes return every grant a
    /// [`LeaseBook::tick`] makes afterwards, whatever its kind.
    pub fn quota(&mut self, kind: &LeaseKind, per_hour: u32) -> Vec<Grant> {
        self.with_window(kind, |window, _| window.quota(per_hour))
    }

    /// Counts a summon of `kind`'s window accepted at `at`
    pub fn summoned(&mut self, kind: &LeaseKind, at: Timestamp) -> Vec<Grant> {
        self.with_window(kind, |window, _| window.summoned(at))
    }

    /// Takes a refusal that quotes `kind`'s window opening at `opens`
    pub fn refused(&mut self, kind: &LeaseKind, opens: Timestamp) -> Vec<Grant> {
        self.with_window(kind, |window, now| window.refused(now, opens))
    }

    /// Grants every free lease whose window has opened to its next waiter
    pub fn tick(&mut self) -> Vec<Grant> {
        let now = self.clock.now();
        let mut grants = Vec::new();
        for (kind, lease) in &mut self.leases {
            if let Some(window) = &mut lease.window {
                window.prune(now);
            }
            if lease.held.is_none() {
                grants.extend(grant_next(kind, lease, now));
            }
        }
        grants
    }

    // A window change can open it, so it grants whatever that allows.
    fn with_window(
        &mut self,
        kind: &LeaseKind,
        change: impl FnOnce(&mut Window, Timestamp),
    ) -> Vec<Grant> {
        let now = self.clock.now();
        let lease = self.leases.get_mut(kind);
        let Some(window) = lease.and_then(|l| l.window.as_mut()) else {
            return Vec::new();
        };
        change(window, now);
        self.tick()
    }

    /// Asks for `kind` on behalf of `holder`
    ///
    /// Asking again while waiting keeps the place already in the queue.
    pub fn ask(&mut self, kind: &LeaseKind, holder: Holder) -> Asked {
        let now = self.clock.now();
        let lease = self.leases.entry(kind.clone()).or_default();
        match &lease.held {
            None if lease.queue.is_empty() && lease.open(now) => {
                granted(lease, holder, now);
                return Asked::Granted;
            }
            None => {}
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
        let now = self.clock.now();
        self.leases
            .iter()
            .map(|(kind, lease)| LeaseStatus {
                kind: kind.clone(),
                holder: lease.held.as_ref().map(|(h, _)| h.clone()),
                since: lease.held.as_ref().map(|(_, since)| *since),
                queue: lease.queue.iter().cloned().collect(),
                window: lease.window.as_ref().map(|w| w.status(now)),
            })
            .collect()
    }
}

fn grant_next(kind: &LeaseKind, lease: &mut Lease, now: Timestamp) -> Option<Grant> {
    if !lease.open(now) {
        return None;
    }
    let holder = lease.queue.pop_front()?;
    granted(lease, holder.clone(), now);
    Some(Grant {
        kind: kind.clone(),
        holder,
    })
}

// The dog cannot see what the maintainer summons, so a grant to them
// counts as a summon at once. A refusal they meet corrects it.
fn granted(lease: &mut Lease, holder: Holder, now: Timestamp) {
    if let (Holder::Maintainer, Some(window)) = (&holder, &mut lease.window) {
        window.summoned(now);
    }
    lease.held = Some((holder, now));
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

    fn windowed() -> (LeaseBook, FakeClock, LeaseKind) {
        let (mut book, clock) = book();
        let kind = LeaseKind::try_from("reviews").unwrap();
        book.add_window(kind.clone());
        (book, clock, kind)
    }

    #[test]
    fn a_windowed_lease_given_back_after_a_summon_waits_out_the_hour() {
        let (mut book, clock, kind) = windowed();
        assert_eq!(book.ask(&kind, runner("koji", 1)), Asked::Granted);
        assert_eq!(book.summoned(&kind, Timestamp(EPOCH)), []);
        assert_eq!(
            book.ask(&kind, runner("reactmap", 1)),
            Asked::Queued { ahead: 0 }
        );
        clock.advance(300);
        assert_eq!(book.give_back(&kind, &runner("koji", 1)), None);
        assert_eq!(status(&book)[0]["window"]["opens"], json!(EPOCH + 3600));

        clock.advance(3299);
        assert_eq!(book.tick(), []);
        clock.advance(1);
        assert_eq!(
            book.tick(),
            [Grant {
                kind: kind.clone(),
                holder: runner("reactmap", 1)
            }]
        );
        assert_eq!(status(&book)[0]["since"], EPOCH + 3600);
    }

    #[test]
    fn a_closed_window_queues_even_a_free_lease() {
        let (mut book, clock, kind) = windowed();
        book.refused(&kind, Timestamp(EPOCH + 600));
        assert_eq!(
            book.ask(&kind, runner("koji", 1)),
            Asked::Queued { ahead: 0 }
        );
        assert_eq!(status(&book)[0]["holder"], json!(null));
        clock.advance(600);
        assert_eq!(book.tick().len(), 1);
        assert_eq!(status(&book)[0]["holder"], json!({ "runner": "koji" }));
    }

    #[test]
    fn a_refusal_while_held_reschedules_the_next_grant_from_its_quote() {
        let (mut book, clock, kind) = windowed();
        book.ask(&kind, runner("koji", 1));
        book.ask(&kind, runner("reactmap", 1));
        clock.advance(60);
        book.refused(&kind, Timestamp(EPOCH + 60 + 10 * 60));
        assert_eq!(book.give_back(&kind, &runner("koji", 1)), None);
        clock.advance(599);
        assert_eq!(book.tick(), []);
        clock.advance(1);
        assert_eq!(book.tick()[0].holder, runner("reactmap", 1));
    }

    #[test]
    fn a_higher_quota_read_from_a_footer_grants_at_once() {
        let (mut book, clock, kind) = windowed();
        book.ask(&kind, runner("koji", 1));
        book.summoned(&kind, Timestamp(EPOCH));
        book.give_back(&kind, &runner("koji", 1));
        book.ask(&kind, runner("reactmap", 1));
        clock.advance(60);
        assert_eq!(
            book.quota(&kind, 10),
            [Grant {
                kind: kind.clone(),
                holder: runner("reactmap", 1)
            }]
        );
    }

    #[test]
    fn a_dead_runners_window_lease_goes_to_the_next_waiter_only_once_it_opens() {
        let (mut book, clock, kind) = windowed();
        book.ask(&kind, runner("koji", 1));
        book.summoned(&kind, Timestamp(EPOCH));
        book.ask(&kind, runner("reactmap", 1));
        assert_eq!(book.reclaim(&project("koji"), None), []);
        assert_eq!(status(&book)[0]["holder"], json!(null));
        clock.advance(3600);
        assert_eq!(book.tick()[0].holder, runner("reactmap", 1));
    }

    #[test]
    fn a_grant_to_the_maintainer_counts_as_a_summon() {
        let (mut book, clock, kind) = windowed();
        assert_eq!(book.ask(&kind, Holder::Maintainer), Asked::Granted);
        book.ask(&kind, runner("koji", 1));
        clock.advance(120);
        assert_eq!(book.give_back(&kind, &Holder::Maintainer), None);
        assert_eq!(
            status(&book)[0]["window"],
            json!({ "quota": 1, "summons": [EPOCH], "opens": EPOCH + 3600 })
        );
    }

    #[test]
    fn a_window_fact_for_a_kind_without_a_window_changes_nothing() {
        let (mut book, _) = book();
        book.ask(&stand_in(), runner("koji", 1));
        assert_eq!(book.refused(&stand_in(), Timestamp(EPOCH + 600)), []);
        assert_eq!(book.ask(&stand_in(), runner("koji", 1)), Asked::AlreadyHeld);
        assert!(status(&book)[0].get("window").is_none());
    }

    #[test]
    fn a_book_nobody_asked_is_empty() {
        let (book, _) = book();
        assert_eq!(status(&book), json!([]));
    }
}
