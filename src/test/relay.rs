//! The rig's relay: recording every send and clear, and refusing sends
//! until a test says it is up. Down by default, so a test that never
//! mentions the relay still exercises the webhook alone, as every ruling
//! did before the relay existed.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ports::{Cleared, Relay, RelayError, Timestamp};
use crate::settings::Effort;

/// A relay that records what it is sent, and refuses sends until a test
/// says it is up
#[derive(Debug, Default)]
pub(crate) struct FakeRelay {
    sent: Mutex<Vec<(String, String, Effort)>>,
    told: Mutex<Vec<String>>,
    cleared: Mutex<Cleared>,
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

    /// How many times the relay was cleared, by any runner on it
    pub(crate) fn clears(&self) -> u64 {
        self.cleared.lock().unwrap().count
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
    fn renew(&self, now: Timestamp) -> Result<(), RelayError> {
        if self.stale.swap(false, Ordering::SeqCst) {
            self.clear(now)?;
        }
        Ok(())
    }

    fn cleared(&self) -> Result<Cleared, RelayError> {
        Ok(*self.cleared.lock().unwrap())
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

    fn clear(&self, now: Timestamp) -> Result<(), RelayError> {
        let mut cleared = self.cleared.lock().unwrap();
        cleared.count += 1;
        cleared.last = Some(now);
        Ok(())
    }
}
