//! The dog reading its `[kelpie]` section again, and applying what can apply live
//!
//! Every [`LOAD_TICK`] the dog asks the shepherd for the section. A text that
//! differs from the last good one is read whole, as the start reads it, with
//! no salvage. A good read applies `leases.cargo-test`, `agent_board.prefer`,
//! `gateways` and `codex_home`. The webhook waits for a restart, and `status` names
//! its keys. A read that fails applies nothing, and the webhook the dog started
//! with is told once per reason.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::time::{Instant, MissedTickBehavior};

use super::availability::{self, LOAD_TICK, Live};
use super::board::{self, Board};
use super::door::{self, Waiters};
use super::section::{self, Asked, Started};
use super::triggers::{Refused, SettingsState};
use super::{Kept, lock_desk};
use crate::ports::{Alert, Alerts, Timestamp};
use crate::settings::{AgentName, Gateway, GatewayName};
use crate::shepherd::Connection;
use crate::webhook::{KelpieSettings, Webhook};

/// The title of the alert that a change to the section was refused
const REFUSED_TITLE: &str = "kelpie: dog settings change refused";

/// What a reread changes, and where it says so
pub(super) struct Handles {
    pub(super) desk: Arc<Mutex<Kept>>,
    pub(super) board: Arc<Mutex<Board>>,
    pub(super) waiters: Waiters,
    pub(super) alerts: Arc<dyn Alerts>,
    pub(super) standing: Arc<Mutex<SettingsState>>,
    pub(super) live: Live,
}

// A refused section: why, since when, and whether the webhook was told or there is none to tell.
// Copies `Refusal` of the runner, which sits above the dog.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal {
    reason: String,
    at: Timestamp,
    told: bool,
}

/// The dog's reread of its section, one [`pass`](Reread::pass) at a time
pub(super) struct Reread {
    handles: Handles,
    // What the start held: the restart-only keys are compared with it.
    started: KelpieSettings,
    // The last text that read whole, or the start's.
    text: String,
    capacity: NonZeroU32,
    prefer: Vec<AgentName>,
    gateways: BTreeMap<GatewayName, Gateway>,
    codex_home: Option<PathBuf>,
    waiting: Vec<&'static str>,
    refusal: Option<Refusal>,
    // Why the last ask returned no section, so it is said once.
    unreadable: Option<String>,
}

impl Reread {
    /// A reread whose baseline is what the start read
    pub(super) fn new(started: &Started, handles: Handles) -> Self {
        Self {
            handles,
            started: started.settings.clone(),
            text: started.text.clone(),
            capacity: started.settings.leases.cargo_test_capacity(),
            prefer: started.settings.agent_board.prefer.clone(),
            gateways: started.settings.gateways.clone(),
            codex_home: started.settings.codex_home.clone(),
            waiting: Vec::new(),
            refusal: None,
            unreadable: None,
        }
    }

    /// Asks for the section and applies what changed, and returns the lines for the log
    pub(super) async fn pass(&mut self, connection: &dyn Connection) -> Vec<String> {
        let mut log = Vec::new();
        match section::ask(connection).await {
            Asked::Text(text) => {
                self.unreadable = None;
                self.read(&text, &mut log).await;
            }
            Asked::Refused(reason) => {
                self.unreadable = None;
                self.refuse(&reason, &mut log).await;
            }
            Asked::Unreadable(why) => {
                if self.unreadable.as_ref() != Some(&why) {
                    log.push(why.clone());
                }
                self.unreadable = Some(why);
            }
        }
        self.publish();
        log
    }

    async fn read(&mut self, text: &str, log: &mut Vec<String>) {
        if text == self.text {
            self.refusal = None;
            return;
        }
        // A runner's reread checks the Codex home as its start does. The dog's start leaves the
        // account unknown instead, so only a home that changed is refused: an edit elsewhere
        // still applies while the home the dog runs on stands.
        let read = section::read_whole(text).and_then(|settings| {
            let changed = settings.codex_home != self.codex_home;
            match changed
                .then(|| availability::refused_codex_home(&settings))
                .flatten()
            {
                Some(reason) => Err(reason),
                None => Ok(settings),
            }
        });
        match read {
            Ok(settings) => {
                self.refusal = None;
                text.clone_into(&mut self.text);
                self.apply(&settings, log);
            }
            Err(reason) => self.refuse(&reason, log).await,
        }
    }

    // Applies each live key that differs from the last applied, then notes the restart-only ones.
    fn apply(&mut self, settings: &KelpieSettings, log: &mut Vec<String>) {
        let capacity = settings.leases.cargo_test_capacity();
        if capacity != self.capacity {
            let granted =
                lock_desk(&self.handles.desk).change(|kept| kept.set_test_capacity(capacity));
            door::wake(&self.handles.waiters, granted);
            self.capacity = capacity;
            log.push(format!("applied leases.cargo-test: {capacity} at a time"));
        }
        let prefer = &settings.agent_board.prefer;
        if *prefer != self.prefer {
            board::lock(&self.handles.board).set_prefer(prefer.clone());
            self.prefer.clone_from(prefer);
            let names: Vec<_> = prefer.iter().map(AgentName::as_str).collect();
            let names = if names.is_empty() {
                "none".into()
            } else {
                names.join(", ")
            };
            log.push(format!("applied agent_board.prefer: {names}"));
        }
        self.apply_availability(settings, log);
        let waiting = waiting_for_restart(&self.started, settings);
        let fresh = waiting.iter().filter(|key| !self.waiting.contains(key));
        log.extend(fresh.map(|key| format!("waiting for restart: {key}")));
        self.waiting = waiting;
    }

    // The gateways and the Codex home, which the availability task takes together.
    fn apply_availability(&mut self, settings: &KelpieSettings, log: &mut Vec<String>) {
        let gateways = settings.gateways != self.gateways;
        let codex_home = settings.codex_home != self.codex_home;
        if !gateways && !codex_home {
            return;
        }
        self.handles.live.apply(settings);
        if gateways {
            self.gateways.clone_from(&settings.gateways);
            let names: Vec<_> = settings.gateways.keys().map(GatewayName::as_str).collect();
            let names = if names.is_empty() {
                "none".into()
            } else {
                names.join(", ")
            };
            log.push(format!("applied gateways: {names}"));
        }
        if codex_home {
            self.codex_home.clone_from(&settings.codex_home);
            let home = settings.codex_home.as_deref();
            let home = home.map_or("kelpie's own".into(), |home| home.display().to_string());
            log.push(format!("applied codex_home: {home}"));
        }
    }

    // Keeps the old settings, and tells the webhook the dog started with the first time it sees `reason`.
    async fn refuse(&mut self, reason: &str, log: &mut Vec<String>) {
        let standing = self.refusal.take().filter(|r| r.reason == reason);
        let mut refusal = standing.unwrap_or_else(|| {
            log.push(format!(
                "the [kelpie] section is refused and the old settings stand: {reason}"
            ));
            Refusal {
                reason: reason.to_owned(),
                at: lock_desk(&self.handles.desk).desk.book.now(),
                told: false,
            }
        });
        if !refusal.told {
            refusal.told = self.tell(reason, log).await;
        }
        self.refusal = Some(refusal);
    }

    // Posts the refusal, and whether it landed or there is no webhook to post to.
    async fn tell(&self, reason: &str, log: &mut Vec<String>) -> bool {
        let Some(webhook) = self.started.webhook.clone() else {
            return true;
        };
        let alert = Alert {
            title: REFUSED_TITLE.to_owned(),
            text: format!(
                "{reason}. The dog keeps its old settings until its [kelpie] section is fixed."
            ),
            reply: None,
        };
        let alerts = Arc::clone(&self.handles.alerts);
        let posted = tokio::task::spawn_blocking(move || alerts.post(&webhook, &alert)).await;
        match posted {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                log.push(format!(
                    "cannot post that a settings change was refused: {e}"
                ));
                false
            }
            Err(e) => {
                log.push(format!(
                    "cannot post that a settings change was refused: {e}"
                ));
                false
            }
        }
    }

    // What `status` shows.
    fn publish(&self) {
        let refused = self.refusal.as_ref().map(|r| Refused {
            reason: r.reason.clone(),
            at: r.at,
        });
        let state = SettingsState {
            waiting: self.waiting.clone(),
            refused,
        };
        *self
            .handles
            .standing
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = state;
    }
}

// The restart-only keys whose value differs from the one the dog started with, by name.
// Both settings are taken apart without `..`, so a key added to one is a compile error
// until this says whether the dog reads it live, at start, or not at all.
fn waiting_for_restart(started: &KelpieSettings, now: &KelpieSettings) -> Vec<&'static str> {
    let KelpieSettings {
        webhook: was_webhook,
        leases: _,          // applied live
        gpu_metrics_url: _, // a runner's, read at each of its wakes
        codex_home: _,      // applied live
        gateways: _,        // applied live
        agent_board: _,     // applied live
    } = started;
    let KelpieSettings {
        webhook: now_webhook,
        leases: _,
        gpu_metrics_url: _,
        codex_home: _,
        gateways: _,
        agent_board: _,
    } = now;
    webhook_keys(was_webhook.as_ref(), now_webhook.as_ref())
}

// A webhook that appears or goes is `webhook`, and one that changes is each key that differs.
fn webhook_keys(was: Option<&Webhook>, now: Option<&Webhook>) -> Vec<&'static str> {
    match (was, now) {
        (None, None) => Vec::new(),
        (Some(_), None) | (None, Some(_)) => vec!["webhook"],
        (Some(was), Some(now)) => {
            let Webhook {
                kind: was_kind,
                url: was_url,
                ca_cert: was_ca,
                token: was_token,
                trusted: was_trusted,
            } = was;
            let Webhook {
                kind: now_kind,
                url: now_url,
                ca_cert: now_ca,
                token: now_token,
                trusted: now_trusted,
            } = now;
            [
                ("webhook.kind", was_kind != now_kind),
                ("webhook.url", was_url != now_url),
                ("webhook.ca_cert", was_ca != now_ca),
                ("webhook.token", was_token != now_token),
                ("webhook.trusted", was_trusted != now_trusted),
            ]
            .into_iter()
            .filter_map(|(key, differs)| differs.then_some(key))
            .collect()
        }
    }
}

/// Starts rereading the section every [`LOAD_TICK`], off the dog's loop since each ask waits on the shepherd
pub(super) fn begin(mut reread: Reread, connection: Arc<dyn Connection>) {
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval_at(Instant::now() + LOAD_TICK, LOAD_TICK);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            for line in reread.pass(&*connection).await {
                println!("{line}");
            }
        }
    });
}

#[cfg(test)]
mod tests;
