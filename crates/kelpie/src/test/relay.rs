//! The rig's relay: recording every send and clear, and refusing sends
//! until a test says it is up. Down by default, so a test that never
//! mentions the relay still exercises the webhook fallback it replaced.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::ports::{Relay, RelayError};

/// A relay that records what it is sent, and refuses sends until a test
/// says it is up
#[derive(Debug, Default)]
pub(crate) struct FakeRelay {
    sent: Mutex<Vec<String>>,
    clears: AtomicUsize,
    up: AtomicBool,
}

impl FakeRelay {
    /// Every message sent, oldest first
    pub(crate) fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }

    /// How many times the relay was cleared
    pub(crate) fn clears(&self) -> usize {
        self.clears.load(Ordering::SeqCst)
    }

    /// Makes sends succeed, as a reachable relay would
    pub(crate) fn set_up(&self, up: bool) {
        self.up.store(up, Ordering::SeqCst);
    }
}

impl Relay for FakeRelay {
    fn send(&self, message: &str) -> Result<(), RelayError> {
        if !self.up.load(Ordering::SeqCst) {
            return Err(RelayError::Unreachable("the rig's relay is down".into()));
        }
        self.sent.lock().unwrap().push(message.to_owned());
        Ok(())
    }

    fn clear(&self) -> Result<(), RelayError> {
        self.clears.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
