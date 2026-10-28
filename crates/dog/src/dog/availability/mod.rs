//! How busy each model and plan the dog's agent files name is right now
//!
//! Every [`LOAD_TICK`] the dog reads the load of each model its agents call
//! behind a paddock gateway. In the same pass it reads the plan meter of
//! each account an agent's `limit` names, at most every [`RECHECK_SECS`].
//! A failed load read is unknown, never free. A failed meter read keeps the
//! last reading, with the time it was read. Only `status` shows it so far.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::MissedTickBehavior;

use crate::agents::Agents;
use crate::forwarder::Upstream;
use crate::pacer::RECHECK_SECS;
use crate::ports::{Clock, LocalLeases, Meter, ModelLoad, Timestamp, Utilization};
use crate::settings::{Account, GatewayKind, GatewayName, Gateways, Limit, ModelHost};
use crate::webhook::KelpieSettings;

/// How often the dog reads each model's load. A load is a few leases on a
/// local server, read in one request each.
pub const LOAD_TICK: Duration = Duration::from_secs(30);

/// What the dog reads availability through, which the binary builds from the adapters
pub struct Ports {
    /// Reads a gateway model's load
    pub local_leases: Arc<dyn LocalLeases>,
    /// Reads the Claude account's plan meter
    pub meter: Arc<dyn Meter>,
    /// The Codex account's plan meter, on the login in kelpie's Codex home
    pub codex_meter: fn(PathBuf) -> Arc<dyn Meter>,
}

impl fmt::Debug for Ports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ports").finish_non_exhaustive()
    }
}

type ModelKey = (GatewayName, String);

/// The last load of each model and the last meter reading of each account
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Availability {
    models: BTreeMap<ModelKey, Seen<ModelLoad>>,
    accounts: BTreeMap<Account, Seen<Utilization>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen<T> {
    last: Option<(T, Timestamp)>,
    asked: Option<Timestamp>,
}

impl<T> Default for Seen<T> {
    fn default() -> Self {
        Self {
            last: None,
            asked: None,
        }
    }
}

impl Availability {
    /// Every model and account in `sources`, each unknown until read
    pub(super) fn new(sources: &Sources) -> Self {
        Self {
            models: (sources.models.keys().cloned())
                .map(|key| (key, Seen::default()))
                .collect(),
            accounts: (sources.accounts.iter())
                .map(|account| (*account, Seen::default()))
                .collect(),
        }
    }

    /// Each model's load and each account's windows, as `status` shows them
    pub fn status(&self) -> Value {
        let read_at = |at: Timestamp| json!({ "read_at": at });
        let unknown = json!({ "unknown": true, "read_at": null });
        let models = self.models.iter().map(|((gateway, model), seen)| {
            let mut line = json!({ "gateway": gateway.as_str(), "model": model });
            let shown = match seen.last {
                Some((load, at)) => merged(json!(load), read_at(at)),
                None => unknown.clone(),
            };
            merged(line.take(), shown)
        });
        let accounts = self.accounts.iter().map(|(account, seen)| {
            let line = json!({ "account": account });
            let shown = match seen.last {
                Some((usage, at)) => {
                    let windows = json!({ "session": usage.session, "week": usage.week });
                    merged(windows, read_at(at))
                }
                None => unknown.clone(),
            };
            merged(line, shown)
        });
        json!({ "models": models.collect::<Vec<_>>(), "accounts": accounts.collect::<Vec<_>>() })
    }

    fn loaded(&mut self, key: ModelKey, load: Option<ModelLoad>, now: Timestamp) {
        let seen = self.models.entry(key).or_default();
        seen.last = load.map(|load| (load, now));
        seen.asked = Some(now);
    }

    fn metered(&mut self, account: Account, usage: Option<Utilization>, now: Timestamp) {
        let seen = self.accounts.entry(account).or_default();
        if let Some(usage) = usage {
            seen.last = Some((usage, now));
        }
        seen.asked = Some(now);
    }

    // The accounts not asked within the last `RECHECK_SECS`.
    fn due(&self, now: Timestamp) -> Vec<Account> {
        let due = |seen: &Seen<_>| seen.asked.is_none_or(|at| now.0 >= at.0 + RECHECK_SECS);
        (self.accounts.iter())
            .filter(|(_, seen)| due(seen))
            .map(|(account, _)| *account)
            .collect()
    }
}

fn merged(mut into: Value, from: Value) -> Value {
    if let (Some(into), Value::Object(from)) = (into.as_object_mut(), from) {
        into.extend(from);
    }
    into
}

/// The models and accounts the agent files name, and where each model is asked
pub(super) struct Sources {
    // `None` where the gateway's key is not set, so the model stays unknown.
    models: BTreeMap<ModelKey, Option<Upstream>>,
    accounts: BTreeSet<Account>,
}

impl Sources {
    /// Each model an agent calls behind a paddock in `gateways`, and each account an agent paces on
    pub(super) fn of(agents: &Agents, gateways: &Gateways) -> Self {
        let mut models = BTreeMap::new();
        for (gateway, model) in agents.iter().filter_map(|(_, a)| a.runs.gateway()) {
            // parity: gateway.lease-availability
            if gateways.get(gateway).map(|g| g.kind) != Some(GatewayKind::Paddock) {
                continue;
            }
            let up = gateways.upstream(&ModelHost::Gateway(gateway.clone()));
            models.insert((gateway.clone(), model.to_owned()), up.ok());
        }
        let accounts = (agents.iter())
            .filter_map(|(_, agent)| match agent.runs.session()?.1 {
                Limit::Account(account) => Some(*account),
                Limit::Unqueued | Limit::Gateway => None,
            })
            .collect();
        Self { models, accounts }
    }
}

/// The ports a pass reads through, with each account's meter
pub(super) struct Readers {
    pub(super) leases: Arc<dyn LocalLeases>,
    pub(super) meters: BTreeMap<Account, Arc<dyn Meter>>,
    pub(super) clock: Arc<dyn Clock>,
}

/// Reads what `agents` name through `ports` from now on, into what this returns
///
/// A Codex home `settings` cannot give leaves the Codex account unknown.
pub(super) fn begin(
    agents: &Agents,
    settings: &KelpieSettings,
    ports: Ports,
) -> Arc<Mutex<Availability>> {
    let sources = Sources::of(agents, &settings.gateways());
    let shown = Arc::new(Mutex::new(Availability::new(&sources)));
    let mut meters = BTreeMap::from([(Account::Claude, ports.meter)]);
    match codex_home(settings) {
        Ok(home) => _ = meters.insert(Account::Codex, (ports.codex_meter)(home)),
        Err(e) => println!("{e}: the Codex account's plan is not read"),
    }
    let readers = Readers {
        leases: ports.local_leases,
        meters,
        clock: Arc::new(crate::ports::SystemClock),
    };
    tokio::spawn(keep(Arc::clone(&shown), sources, readers));
    shown
}

fn codex_home(settings: &KelpieSettings) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let kelpie_home = crate::home::kelpie_home()?;
    (settings.codex_home(Path::new(&home), &kelpie_home)).map_err(|e| e.to_string())
}

async fn keep(shown: Arc<Mutex<Availability>>, sources: Sources, readers: Readers) {
    let mut ticks = tokio::time::interval(LOAD_TICK);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        read(&shown, &sources, &readers).await;
    }
}

/// One pass: every model's load, then each account's meter that is due
///
/// The reads block, so each runs off the dog's loop, and nothing holds `shown` meanwhile.
pub(super) async fn read(shown: &Mutex<Availability>, sources: &Sources, readers: &Readers) {
    let models = sources.models.clone();
    let leases = Arc::clone(&readers.leases);
    let loads = blocking(move || {
        let load = |(key, up): (ModelKey, Option<Upstream>)| {
            let load = up.and_then(|up| leases.paddock_load(&up, &key.1));
            (key, load)
        };
        models.into_iter().map(load).collect::<Vec<_>>()
    });
    let loads = loads.await.unwrap_or_default();
    let now = readers.clock.now();
    for (key, load) in loads {
        lock(shown).loaded(key, load, now);
    }
    let due = lock(shown).due(readers.clock.now());
    for account in due {
        let usage = match readers.meters.get(&account) {
            Some(meter) => {
                let (meter, now) = (Arc::clone(meter), readers.clock.now());
                blocking(move || meter.read(now).ok()).await.flatten()
            }
            None => None,
        };
        lock(shown).metered(account, usage, readers.clock.now());
    }
}

async fn blocking<T: Send + 'static>(read: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(read).await.ok()
}

/// What the dog has read, under its lock
///
/// Nothing holding it can leave it half changed, so a poisoned lock is fine.
pub(super) fn lock(shown: &Mutex<Availability>) -> MutexGuard<'_, Availability> {
    shown.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
