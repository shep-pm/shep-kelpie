//! The rig's relay: recording every send and counting every clear, and refusing sends
//! until a test says it is up. Down by default, so a test that never
//! mentions the relay still exercises the webhook alone, as every ruling
//! did before the relay existed.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::ports::{Relay, RelayError};
use crate::settings::Effort;

/// A relay that records what it is sent, and refuses sends until a test
/// says it is up
#[derive(Debug, Default)]
pub(crate) struct FakeRelay {
    sent: Mutex<Vec<(String, String, Effort)>>,
    told: Mutex<Vec<String>>,
    clears: AtomicU64,
    up: AtomicBool,
    stale: AtomicBool,
}

impl FakeRelay {
    /// Every message sent, oldest first, with the model and effort it was
    /// asked to start with
    pub(crate) fn sent(&self) -> Vec<(String, String, Effort)> {
        self.sent.lock().unwrap().clone()
    }

    /// Every message told to a running relay, oldest first
    pub(crate) fn told(&self) -> Vec<String> {
        self.told.lock().unwrap().clone()
    }

    /// Makes sends succeed (`up: true`, a reachable relay) or fail
    /// (`up: false`, an unreachable one)
    pub(crate) fn set_up(&self, up: bool) {
        self.up.store(up, Ordering::SeqCst);
    }

    /// Makes the next renew find the relay on older files and clear it
    pub(crate) fn set_stale(&self) {
        self.stale.store(true, Ordering::SeqCst);
    }
}

impl Relay for FakeRelay {
    fn renew(&self) -> Result<bool, RelayError> {
        let stale = self.stale.swap(false, Ordering::SeqCst);
        if stale {
            self.clears.fetch_add(1, Ordering::SeqCst);
        }
        Ok(stale)
    }

    fn clear_count(&self) -> Result<u64, RelayError> {
        Ok(self.clears.load(Ordering::SeqCst))
    }

    fn send(&self, message: &str, model: &str, effort: Effort) -> Result<(), RelayError> {
        if !self.up.load(Ordering::SeqCst) {
            return Err(RelayError::Unreachable("the rig's relay is down".into()));
        }
        self.sent
            .lock()
            .unwrap()
            .push((message.to_owned(), model.to_owned(), effort));
        Ok(())
    }

    fn tell(&self, message: &str) -> Result<(), RelayError> {
        if !self.up.load(Ordering::SeqCst) {
            return Err(RelayError::Unreachable("the rig's relay is down".into()));
        }
        self.told.lock().unwrap().push(message.to_owned());
        Ok(())
    }

    fn clear(&self) -> Result<(), RelayError> {
        self.clears.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
