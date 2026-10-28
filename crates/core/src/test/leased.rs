//! The gateway's leases, answered with nothing, one holder or one load, and counted

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::forwarder::Upstream;
use crate::ports::{LocalLeases, ModelLoad, PaddockHolder};

/// A gateway whose lease reads answer that nothing is held, or one holder or
/// load once a test names one, counted so a test can say who asked
#[derive(Debug, Default)]
pub struct LeaseReads {
    reads: AtomicUsize,
    holder: Mutex<Option<PaddockHolder>>,
    load: Mutex<Option<ModelLoad>>,
}

impl LeaseReads {
    /// How many lease reads reached the gateway
    pub fn count(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    /// Answer every lease read that `client` holds its model for `what`
    pub fn held_by(&self, client: &str, what: &str) {
        *self.holder.lock().unwrap() = Some(PaddockHolder {
            lease: "L4".to_owned(),
            client: client.to_owned(),
            what: what.to_owned(),
            since: "2026-10-13T09:30:00Z".to_owned(),
        });
    }

    /// Answer every load read with `load`, or as unreadable with `None`
    pub fn loaded(&self, load: Option<ModelLoad>) {
        *self.load.lock().unwrap() = load;
    }
}

impl LocalLeases for LeaseReads {
    fn paddock_holder(&self, _gateway: &Upstream, _model: &str) -> Option<PaddockHolder> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.holder.lock().unwrap().clone()
    }

    fn paddock_load(&self, _gateway: &Upstream, _model: &str) -> Option<ModelLoad> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        *self.load.lock().unwrap()
    }
}
