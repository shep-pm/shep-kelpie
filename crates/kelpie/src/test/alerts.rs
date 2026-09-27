//! The rig's webhook: every post recorded, and failing while a test says so

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::ports::{Alert, AlertError, Alerts};
use crate::webhook::Webhook;

/// A webhook that records every post, and fails them while a test says it is down
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeAlerts {
    posts: Arc<Mutex<Vec<(Webhook, Alert)>>>,
    down: Arc<AtomicBool>,
}

impl FakeAlerts {
    /// Every post tried, failed ones included, oldest first
    pub(crate) fn posts(&self) -> Vec<(Webhook, Alert)> {
        self.posts.lock().unwrap().clone()
    }

    /// Makes posts fail as a webhook that is down would, or work again
    pub(crate) fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }
}

impl Alerts for FakeAlerts {
    fn post(&self, webhook: &Webhook, alert: &Alert) -> Result<(), AlertError> {
        self.posts
            .lock()
            .unwrap()
            .push((webhook.clone(), alert.clone()));
        if self.down.load(Ordering::SeqCst) {
            return Err(AlertError::Refused(503));
        }
        Ok(())
    }
}
