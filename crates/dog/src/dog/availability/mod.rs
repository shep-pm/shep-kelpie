//! How busy each model and plan the dog's agent files name is right now
//!
//! Every [`LOAD_TICK`] the dog reads the load of each model its agents call
//! behind a paddock gateway. In the same pass it reads the plan meter of
//! each account an agent's `limit` names, at most every [`RECHECK_SECS`].
//! A failed load read is unknown, never free, and logged when its reason
//! changes. A failed meter read keeps the last reading, with the time it was
//! read. `status` shows it, and the agent board's pick waits on it. A
//! refusal a runner reports holds the API as a whole until it may run again.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::MissedTickBehavior;

use crate::agents::Agents;
use crate::api_refusal::Refusal;
use crate::board::briefing::date_time;
use crate::dog::door::{Place, say};
use crate::dog::lock_desk;
use crate::forwarder::Upstream;
use crate::ipc::ServerWriteHalf;
use crate::lease::door::{Answer, Ask};
use crate::pacer::RECHECK_SECS;
use crate::ports::{Clock, LocalLeases, Meter, ModelLoad, Timestamp, Utilization};
use crate::settings::{Account, GatewayKind, GatewayName, Gateways, Limit, ModelHost};
use crate::webhook::KelpieSettings;

/// How often the dog reads each model's load. A load is a few leases on a
/// local server, read in one request each.
pub const LOAD_TICK: Duration = Duration::from_secs(30);

// The runner's Claude login, as the adapters' `LOGIN_VAR` names it.
const LOGIN_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";

// The key an API agent's calls bill, as the adapters' `API_VAR` names it.
const API_VAR: &str = "ANTHROPIC_API_KEY";

const NO_LOAD: &str = "the gateway gave no load for it";
const READ_FAILED: &str = "the read failed";

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
    // Why each model's last read failed, so a reason is logged once.
    unread: BTreeMap<ModelKey, String>,
    // Every `login: api` agent shares one key, so the API is held as a whole.
    api: Option<ApiHold>,
    // Which apply of the settings the sources came from: a pass that began on an older one
    // writes nothing, since what it read is of a login or gateway no longer named.
    epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApiHold {
    refusal: Refusal,
    until: Timestamp,
}

impl ApiHold {
    // The kind, the time and the one line `shep kelpie status` shows.
    fn status(self) -> Value {
        let (kind, said) = match self.refusal {
            Refusal::NoCredit => ("no_credit", "out of credit"),
            Refusal::SpendLimit { .. } => ("spend_limit", "spend limit reached"),
        };
        let back = match self.refusal {
            Refusal::SpendLimit { resumes: Some(at) } if at == self.until => {
                format!("back at {} UTC", date_time(at))
            }
            _ => {
                let day = self.until.0 % 86_400;
                format!(
                    "checking again at {:02}:{:02} UTC",
                    day / 3600,
                    day % 3600 / 60
                )
            }
        };
        json!({ "kind": kind, "until": self.until, "line": format!("API: {said}, {back}") })
    }
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
            unread: BTreeMap::new(),
            api: None,
            epoch: sources.epoch,
        }
    }

    /// Keeps the reading of each model and account `sources` still names, and lists a new one unknown
    ///
    /// A model or account no longer named is dropped with its logged reason, so
    /// it is logged again if it comes back. `reset` accounts are unknown until read, since
    /// the login they were read on is gone.
    pub(super) fn rename(&mut self, sources: &Sources, reset: &[Account]) {
        self.epoch = sources.epoch;
        self.models
            .retain(|key, _| sources.models.contains_key(key));
        for key in sources.models.keys() {
            self.models.entry(key.clone()).or_default();
        }
        self.unread
            .retain(|key, _| sources.models.contains_key(key));
        self.accounts
            .retain(|account, _| sources.accounts.contains(account));
        for account in &sources.accounts {
            self.accounts.entry(*account).or_default();
        }
        for account in reset {
            if let Some(seen) = self.accounts.get_mut(account) {
                *seen = Seen::default();
            }
        }
    }

    /// Holds the API after `refusal`, seen at `now`, and says until when
    ///
    /// A hold still standing is kept when it outlasts the new one.
    pub(super) fn refused(&mut self, refusal: Refusal, now: Timestamp) -> Timestamp {
        let new = ApiHold {
            refusal,
            until: refusal.hold_until(now),
        };
        match self.api {
            Some(held) if held.until > now && held.until >= new.until => held.until,
            _ => {
                self.api = Some(new);
                new.until
            }
        }
    }

    /// Until when the API is held at `now`, or None when it is not
    pub(super) fn api_held(&self, now: Timestamp) -> Option<Timestamp> {
        self.api.map(|held| held.until).filter(|until| *until > now)
    }

    // Drops a hold whose time has come, so `status` stops showing it.
    fn release_api(&mut self, now: Timestamp) {
        if self.api_held(now).is_none() {
            self.api = None;
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
        let mut shown = json!({
            "models": models.collect::<Vec<_>>(),
            "accounts": accounts.collect::<Vec<_>>(),
        });
        if let Some(held) = self.api {
            shown["api"] = held.status();
        }
        shown
    }

    /// The last load of `model` behind `gateway`, None inside while unknown,
    /// and None when the dog reads no load for it
    pub(super) fn load(&self, gateway: &GatewayName, model: &str) -> Option<Option<ModelLoad>> {
        let seen = self.models.get(&(gateway.clone(), model.to_owned()))?;
        Some(seen.last.map(|(load, _)| load))
    }

    /// The last meter reading of `account`, None until one is read
    pub(super) fn usage(&self, account: Account) -> Option<Utilization> {
        self.accounts.get(&account)?.last.map(|(usage, _)| usage)
    }

    // The line to log when the read failed for a reason other than the last one's.
    fn loaded(
        &mut self,
        key: ModelKey,
        load: Result<ModelLoad, String>,
        now: Timestamp,
        epoch: u64,
    ) -> Option<String> {
        // A pass that began before a reread can end on a model no longer named.
        if epoch != self.epoch {
            return None;
        }
        let seen = self.models.get_mut(&key)?;
        seen.last = load.as_ref().ok().map(|load| (*load, now));
        seen.asked = Some(now);
        let Err(why) = load else {
            self.unread.remove(&key);
            return None;
        };
        let line = format!(
            "the dog cannot read the load of `{}` behind gateway `{}`: {why}",
            key.1, key.0
        );
        (self.unread.insert(key, why.clone()) != Some(why)).then_some(line)
    }

    fn metered(
        &mut self,
        account: Account,
        usage: Option<Utilization>,
        now: Timestamp,
        epoch: u64,
    ) {
        if epoch != self.epoch {
            return;
        }
        let Some(seen) = self.accounts.get_mut(&account) else {
            return;
        };
        if let Some(usage) = usage {
            seen.last = Some((usage, now));
        }
        seen.asked = Some(now);
    }

    // The accounts not asked within the last `RECHECK_SECS`.
    fn due(&self, now: Timestamp, epoch: u64) -> Vec<Account> {
        if epoch != self.epoch {
            return Vec::new();
        }
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
#[derive(Clone)]
pub(super) struct Sources {
    // The reason where the model cannot be asked, such as an unset key.
    models: BTreeMap<ModelKey, Result<Upstream, String>>,
    accounts: BTreeSet<Account>,
    // The apply of the settings these came from, 0 at the start.
    epoch: u64,
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
            models.insert((gateway.clone(), model.to_owned()), up);
        }
        let accounts = (agents.iter())
            .filter_map(|(_, agent)| match agent.runs.session()?.1 {
                Limit::Account(account) => Some(*account),
                Limit::Unqueued | Limit::Gateway | Limit::Api => None,
            })
            .collect();
        Self {
            models,
            accounts,
            epoch: 0,
        }
    }
}

/// The ports a pass reads through, with each account's meter
#[derive(Clone)]
pub(super) struct Readers {
    pub(super) leases: Arc<dyn LocalLeases>,
    pub(super) meters: BTreeMap<Account, Arc<dyn Meter>>,
    pub(super) clock: Arc<dyn Clock>,
}

/// Reads what `agents` name through `ports` from now on, into what this returns
///
/// A Codex home `settings` cannot give leaves the Codex account unknown. No
/// process the dog starts from then on sees a gateway key, the Claude login or the API key.
/// The [`Live`] it returns takes a change to the gateways and the Codex home.
pub(super) fn begin(
    agents: &Agents,
    settings: &KelpieSettings,
    ports: Ports,
) -> (Arc<Mutex<Availability>>, Live) {
    crate::spawn::hide([LOGIN_VAR.to_owned(), API_VAR.to_owned()]);
    let sources = Sources::of(agents, &settings.gateways());
    let shown = Arc::new(Mutex::new(Availability::new(&sources)));
    let readers = Readers {
        leases: ports.local_leases,
        meters: BTreeMap::from([(Account::Claude, ports.meter)]),
        clock: Arc::new(crate::ports::SystemClock),
    };
    let live = Live {
        shown: Arc::clone(&shown),
        reading: Arc::new(Mutex::new(Reading {
            agents: agents.clone(),
            gateways: settings.gateways(),
            sources,
            readers,
            home: None,
        })),
        codex_meter: ports.codex_meter,
    };
    live.apply(settings);
    tokio::spawn(keep(Arc::clone(&shown), live.clone()));
    (shown, live)
}

// What the next pass reads, and the Codex home its meter was built on.
struct Reading {
    agents: Agents,
    gateways: Gateways,
    sources: Sources,
    readers: Readers,
    // None before the first apply.
    home: Option<Result<PathBuf, String>>,
}

/// What the availability task reads, which a reread of the settings and of the agent files changes
#[derive(Clone)]
pub(super) struct Live {
    shown: Arc<Mutex<Availability>>,
    reading: Arc<Mutex<Reading>>,
    codex_meter: fn(PathBuf) -> Arc<dyn Meter>,
}

impl Live {
    /// Reads the gateways and the Codex home in `settings` from the next pass
    ///
    /// Each model and account still named keeps its reading and a new one is
    /// unknown until read. A Codex home that differs from the last builds the meter
    /// again, and the Codex account is unknown until that login is read. Every
    /// gateway key variable is hidden from the processes the dog starts from now
    /// on, and none is shown again. A gateway's new key reaches the dog's
    /// environment only from a fresh spawn.
    pub(super) fn apply(&self, settings: &KelpieSettings) {
        let gateways = settings.gateways();
        crate::spawn::hide(gateways.key_vars());
        let mut reading = self.reading.lock().unwrap_or_else(PoisonError::into_inner);
        let mut sources = Sources::of(&reading.agents, &gateways);
        reading.gateways = gateways;
        sources.epoch = reading.sources.epoch + 1;
        let home = codex_home(settings);
        let changed = reading.home.as_ref() != Some(&home);
        if changed {
            match &home {
                Ok(home) => {
                    let meter = (self.codex_meter)(home.clone());
                    reading.readers.meters.insert(Account::Codex, meter);
                }
                Err(e) => {
                    println!("{e}: the Codex account's plan is not read");
                    reading.readers.meters.remove(&Account::Codex);
                }
            }
            reading.home = Some(home);
        }
        let reset: &[Account] = if changed { &[Account::Codex] } else { &[] };
        lock(&self.shown).rename(&sources, reset);
        reading.sources = sources;
    }

    /// Reads the models and accounts `agents` name from the next pass
    ///
    /// Each model and account still named keeps its reading and a new one is
    /// unknown until read.
    pub(super) fn set_agents(&self, agents: &Agents) {
        let mut reading = self.reading.lock().unwrap_or_else(PoisonError::into_inner);
        let mut sources = Sources::of(agents, &reading.gateways);
        sources.epoch = reading.sources.epoch + 1;
        lock(&self.shown).rename(&sources, &[]);
        reading.agents = agents.clone();
        reading.sources = sources;
    }

    // What the next pass reads, taken whole so a pass ends on what it began with.
    fn now(&self) -> (Sources, Readers) {
        let reading = self.reading.lock().unwrap_or_else(PoisonError::into_inner);
        (reading.sources.clone(), reading.readers.clone())
    }
}

/// Why `settings` names a Codex home the dog refuses, or None when it is fine
///
/// A home folder or kelpie's home that cannot be worked out is refused too, as the start
/// leaves the account unknown for it.
pub(super) fn refused_codex_home(settings: &KelpieSettings) -> Option<String> {
    codex_home(settings).err()
}

fn codex_home(settings: &KelpieSettings) -> Result<PathBuf, String> {
    let home = crate::home::user_home().ok_or(crate::home::HOME_UNSET)?;
    let kelpie_home = crate::home::kelpie_home()?;
    (settings.codex_home(&home, &kelpie_home)).map_err(|e| e.to_string())
}

async fn keep(shown: Arc<Mutex<Availability>>, live: Live) {
    let mut ticks = tokio::time::interval(LOAD_TICK);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let (sources, readers) = live.now();
        for line in read(&shown, &sources, &readers).await {
            println!("{line}");
        }
    }
}

/// One pass: every model's load, then each account's meter that is due,
/// returning a line for each load read that fails for a new reason
///
/// The reads block, so each runs off the dog's loop, and nothing holds `shown` meanwhile.
pub(super) async fn read(
    shown: &Mutex<Availability>,
    sources: &Sources,
    readers: &Readers,
) -> Vec<String> {
    let models = sources.models.clone();
    let leases = Arc::clone(&readers.leases);
    let loads = blocking(move || {
        let load = |(key, up): (ModelKey, Result<Upstream, String>)| {
            let load = up.and_then(|up| {
                (leases.paddock_load(&up, &key.1)).ok_or_else(|| NO_LOAD.to_owned())
            });
            (key, load)
        };
        models.into_iter().map(load).collect::<Vec<_>>()
    });
    let unread = || {
        sources
            .models
            .keys()
            .map(|key| (key.clone(), Err(READ_FAILED.to_owned())))
            .collect()
    };
    let loads = loads.await.unwrap_or_else(unread);
    let now = readers.clock.now();
    let logged = (loads.into_iter())
        .filter_map(|(key, load)| lock(shown).loaded(key, load, now, sources.epoch))
        .collect();
    lock(shown).release_api(readers.clock.now());
    let due = lock(shown).due(readers.clock.now(), sources.epoch);
    for account in due {
        let usage = match readers.meters.get(&account) {
            Some(meter) => {
                let (meter, now) = (Arc::clone(meter), readers.clock.now());
                blocking(move || meter.read(now).ok()).await.flatten()
            }
            None => None,
        };
        lock(shown).metered(account, usage, readers.clock.now(), sources.epoch);
    }
    logged
}

async fn blocking<T: Send + 'static>(read: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(read).await.ok()
}

/// Answers a runner's `refused` ask with the time the API is held until, and ends the visit
///
/// Every runner on the key reports the same refusal, so a hold already
/// standing keeps the later time.
pub(super) async fn visit(place: &Place, ask: Ask, write: &mut ServerWriteHalf) {
    let Some(refusal) = ask.refused else {
        let why = "a refused ask says why the API refused the key".to_owned();
        return say(write, &Answer::Error(why)).await;
    };
    let now = lock_desk(&place.desk).desk.book.now();
    let until = lock(&place.shown).refused(refusal, now);
    say(write, &Answer::Held(until.0)).await;
}

/// What the dog has read, under its lock
///
/// Nothing holding it can leave it half changed, so a poisoned lock is fine.
pub(super) fn lock(shown: &Mutex<Availability>) -> MutexGuard<'_, Availability> {
    shown.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
