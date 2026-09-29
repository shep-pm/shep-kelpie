//! The rig's lease book: grants what is asked at once, unless a test holds
//! the grants back, and keeps every ask, return of a held lease and window
//! fact in order

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::Leases;

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

    /// Grants `kind` between steps, as the dog's `grant` trigger does
    pub(crate) fn grant(&self, kind: &LeaseKind) {
        self.held.lock().unwrap().insert(kind.clone());
    }

    /// Whether the runner holds `kind` now
    pub(crate) fn held(&self, kind: &LeaseKind) -> bool {
        self.held.lock().unwrap().contains(kind)
    }
}

impl Leases for FakeLeases {
    fn want(&self, kind: &LeaseKind) {
        self.told.lock().unwrap().push(Told::Want(kind.clone()));
        let closed = self.closed.lock().unwrap().contains(kind);
        if !self.withheld.load(Ordering::SeqCst) && !closed {
            self.held.lock().unwrap().insert(kind.clone());
        }
    }

    fn holds(&self, kind: &LeaseKind) -> bool {
        self.held(kind)
    }

    fn give_back(&self, kind: &LeaseKind) {
        if self.held.lock().unwrap().remove(kind) {
            self.told.lock().unwrap().push(Told::Return(kind.clone()));
        }
    }

    fn window(&self, _kind: &LeaseKind, fact: WindowFact, value: u64) {
        let told = Told::Window(fact, value);
        let mut all = self.told.lock().unwrap();
        // The runner raises a fact again each look; the dog reads it once.
        if all.last() != Some(&told) {
            all.push(told);
        }
    }
}
