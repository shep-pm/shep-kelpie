//! The rig's lease book: grants what is asked at once, unless a test holds
//! the grants back or hands it a real book, and keeps every ask, return of
//! a held lease and window fact in order

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::FakeClock;
use crate::lease::book::LeaseBook;
use crate::lease::wire::WindowFact;
use crate::lease::{Epoch, Holder, LeaseKind};
use crate::ports::{Leases, Timestamp};
use crate::review_bot::{ReviewWindow, Reviewers};
use crate::runner::ProjectName;

/// What the runner told the dog
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Told {
    /// Asked for a kind, or asked again
    Want(LeaseKind),
    /// Gave a kind back
    Return(LeaseKind),
    /// Saw a window fact
    Window(WindowFact, u64),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FakeLeases {
    told: Arc<Mutex<Vec<Told>>>,
    held: Arc<Mutex<BTreeSet<LeaseKind>>>,
    withheld: Arc<AtomicBool>,
    closed: Arc<Mutex<BTreeSet<LeaseKind>>>,
    opens: Arc<Mutex<BTreeMap<LeaseKind, Timestamp>>>,
    book: Arc<Mutex<Option<(LeaseBook, Holder)>>>,
}

impl FakeLeases {
    /// Everything the runner told the dog, oldest first
    pub(crate) fn told(&self) -> Vec<Told> {
        self.told.lock().unwrap().clone()
    }

    /// Holds grants back, as a dog whose window is closed does, or lets
    /// the next ask through
    pub(crate) fn withhold(&self, withheld: bool) {
        self.withheld.store(withheld, Ordering::SeqCst);
    }

    /// Holds back grants of `kind` alone, as a dog whose window for it is
    /// closed does, or lets its next ask through
    pub(crate) fn close(&self, kind: &LeaseKind, closed: bool) {
        let mut all = self.closed.lock().unwrap();
        if closed {
            all.insert(kind.clone());
        } else {
            all.remove(kind);
        }
    }

    /// Has the dog's book say `kind`'s window opens at `at`, as a book whose
    /// window is spent says, until a test says otherwise
    pub(crate) fn opens_at(&self, kind: &LeaseKind, at: Timestamp) {
        self.opens.lock().unwrap().insert(kind.clone(), at);
    }

    /// Answers from a real lease book on `clock` from now on, as the runner
    /// of `project`, in a dog whose section defines `reviewers`. Each ask
    /// ticks the book, as the dog's own tick would between steps.
    pub(crate) fn use_book(&self, clock: FakeClock, project: &str, reviewers: Reviewers) {
        let mut book = LeaseBook::new(Box::new(clock));
        book.set_reviewers(reviewers);
        let me = Holder::Runner {
            project: ProjectName::try_from(project).unwrap(),
            epoch: Epoch(1),
        };
        *self.book.lock().unwrap() = Some((book, me));
    }

    /// Grants `kind` between steps, as the dog's `grant` trigger does
    pub(crate) fn grant(&self, kind: &LeaseKind) {
        self.held.lock().unwrap().insert(kind.clone());
    }

    /// Whether the runner holds `kind` now
    pub(crate) fn held(&self, kind: &LeaseKind) -> bool {
        if let Some((book, me)) = self.book.lock().unwrap().as_ref() {
            return book.holder(kind) == Some(me);
        }
        self.held.lock().unwrap().contains(kind)
    }
}

impl Leases for FakeLeases {
    fn want(&self, kind: &LeaseKind) {
        self.told.lock().unwrap().push(Told::Want(kind.clone()));
        if let Some((book, me)) = self.book.lock().unwrap().as_mut() {
            book.ask(kind, me.clone());
            book.tick();
            return;
        }
        let closed = self.closed.lock().unwrap().contains(kind);
        if !self.withheld.load(Ordering::SeqCst) && !closed {
            self.held.lock().unwrap().insert(kind.clone());
        }
    }

    fn holds(&self, kind: &LeaseKind) -> bool {
        self.held(kind)
    }

    fn give_back(&self, kind: &LeaseKind) {
        let returned = match self.book.lock().unwrap().as_mut() {
            Some((book, me)) => {
                let held = book.holder(kind) == Some(&*me);
                book.give_back(kind, me);
                held
            }
            None => self.held.lock().unwrap().remove(kind),
        };
        if returned {
            self.told.lock().unwrap().push(Told::Return(kind.clone()));
        }
    }

    fn window(&self, kind: &LeaseKind, fact: WindowFact, value: u64) {
        if let Some((book, _)) = self.book.lock().unwrap().as_mut() {
            let at = Timestamp(value);
            let _ = match fact {
                WindowFact::Summoned => book.summoned(kind, at),
                WindowFact::Opens => book.refused(kind, at),
                WindowFact::Quota(per_hour) => book.quota(kind, per_hour, at),
            };
        }
        let told = Told::Window(fact, value);
        let mut all = self.told.lock().unwrap();
        // The runner raises a fact again each look; the dog reads it once.
        if all.last() != Some(&told) {
            all.push(told);
        }
    }

    fn opens(&self, kind: &LeaseKind, _window: ReviewWindow, now: Timestamp) -> Option<Timestamp> {
        if let Some((book, _)) = self.book.lock().unwrap().as_ref() {
            let line = book.status().into_iter().find(|l| &l.kind == kind)?;
            return line.window?.opens;
        }
        let at = self.opens.lock().unwrap().get(kind).copied()?;
        (at > now).then_some(at)
    }
}
