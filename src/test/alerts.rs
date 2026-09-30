//! The rig's webhook: every post recorded, failing while a test says so,
//! and a topic of replies a test writes to

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::{FakeClock, Rig};
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
            said: vec![text.to_owned()],
            cut: false,
        });
    }

    /// Writes a post to the topic with `text` to answer with, if any, and
    /// every other text it carries, such as a title or a tag
    pub(crate) fn post_raw(&self, text: Option<&str>, others: &[&str], time: Timestamp) {
        self.post_cut(text, others, false, time);
    }

    /// [`Self::post_raw`], for a post that is `cut`, as ntfy turns a long
    /// message into an attachment
    pub(crate) fn post_cut(&self, text: Option<&str>, others: &[&str], cut: bool, time: Timestamp) {
        let mut topic = self.topic.lock().unwrap();
        let id = format!("m{}", topic.len() + 1);
        let said = text.into_iter().chain(others.iter().copied());
        topic.push(Reply {
            id,
            time,
            text: text.map(str::to_owned),
            said: said.map(str::to_owned).collect(),
            cut,
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
            said: vec![alert.title.clone(), alert.text.clone()],
            cut: false,
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
        Ok(match since {
            Since::Time(at) => topic.iter().filter(|r| r.time >= *at).cloned().collect(),
            Since::After(id) => {
                let from = topic
                    .iter()
                    .position(|r| r.id == *id)
                    .map_or(0, |at| at + 1);
                topic[from..].to_vec()
            }
        })
    }
}

impl Rig {
    /// The rig's authenticator secret: RFC 6238's SHA-1 key, in base32
    pub(crate) const TOTP_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    /// Writes [`Self::TOTP_SECRET`] where `shep kelpie totp` would, for its owner alone
    pub(crate) fn write_totp_secret(&self) {
        use std::os::unix::fs::PermissionsExt;
        let folder = self.paths().totp;
        crate::totp::private_dir(&folder).unwrap();
        let secret = folder.join("secret");
        std::fs::write(&secret, format!("{}\n", Self::TOTP_SECRET)).unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    /// The maintainer's authenticator code at `at`
    pub(crate) fn code_at(&self, at: Timestamp) -> String {
        let secret = crate::totp::Secret::load(&self.paths().totp.join("secret"));
        let secret = secret.unwrap().expect("the rig writes a secret");
        format!("{:06}", secret.code(crate::totp::step_of(at)))
    }

    /// Writes `text` ending with the code of the moment to the topic, as the
    /// maintainer's phone would, and returns the code
    pub(crate) fn reply(&self, text: &str) -> String {
        let now = self.clock.now();
        let code = self.code_at(now);
        self.alerts.reply(&format!("{text} {code}"), now);
        code
    }
}
