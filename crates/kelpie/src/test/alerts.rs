//! The rig's webhook: every post recorded, failing while a test says so,
//! and a topic of replies a test writes to

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::FakeClock;
use crate::ports::{Alert, AlertError, Alerts, Clock, Reply, Since, Timestamp};
use crate::webhook::Webhook;

/// A webhook that records every post, and fails them while a test says it is down
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeAlerts {
    posts: Arc<Mutex<Vec<(Webhook, Alert)>>>,
    down: Arc<AtomicBool>,
    topic: Arc<Mutex<Vec<Reply>>>,
    reads: Arc<Mutex<Vec<Since>>>,
    // When the webhook takes a post: the rig's clock
    clock: Option<FakeClock>,
}

impl FakeAlerts {
    /// A webhook that takes posts at `clock`'s time
    pub(crate) fn on(clock: FakeClock) -> Self {
        Self {
            clock: Some(clock),
            ..Self::default()
        }
    }

    /// Every post tried, failed ones included, oldest first
    pub(crate) fn posts(&self) -> Vec<(Webhook, Alert)> {
        self.posts.lock().unwrap().clone()
    }

    /// Makes posts and reads fail as a webhook that is down would, or work again
    pub(crate) fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    /// Writes `text` to the topic as the maintainer's phone would, taken by
    /// the webhook at `time`
    pub(crate) fn reply(&self, text: &str, time: Timestamp) {
        let mut topic = self.topic.lock().unwrap();
        let id = format!("m{}", topic.len() + 1);
        topic.push(Reply {
            id,
            time,
            text: Some(text.to_owned()),
        });
    }

    /// Where each read of the topic started, oldest first
    pub(crate) fn reads(&self) -> Vec<Since> {
        self.reads.lock().unwrap().clone()
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
        // What kelpie posts is on the topic too, where a read skips it.
        let mut topic = self.topic.lock().unwrap();
        let id = format!("m{}", topic.len() + 1);
        topic.push(Reply {
            id,
            time: self.clock.as_ref().map_or(Timestamp(0), Clock::now),
            text: None,
        });
        Ok(())
    }

    // Like ntfy, an id the topic no longer holds reads it all.
    fn replies(&self, _: &Webhook, since: &Since) -> Result<Vec<Reply>, AlertError> {
        self.reads.lock().unwrap().push(since.clone());
        if self.down.load(Ordering::SeqCst) {
            return Err(AlertError::Refused(503));
        }
        let topic = self.topic.lock().unwrap();
        let from = match since {
            Since::Time(at) => topic
                .iter()
                .position(|r| r.time >= *at)
                .unwrap_or(topic.len()),
            Since::After(id) => topic
                .iter()
                .position(|r| r.id == *id)
                .map_or(0, |at| at + 1),
        };
        Ok(topic[from..].to_vec())
    }
}
