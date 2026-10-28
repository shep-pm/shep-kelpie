//! Paddock's own routes, for a gateway of kind `paddock`, a shep dog
//!
//! [`Paddock`] is the one adapter of the gateway port.
//!
//! `GET /v1/models` lists its models and needs no key. `GET /paddock/status`
//! needs one, so it tells a key paddock takes from one it refuses. A call or
//! review round on a model behind it holds a heartbeat lease: `POST
//! /paddock/leases` waits for the grant, a `PUT` renews it, and a `DELETE`
//! releases it. A take paddock turns away as busy is asked again until the call
//! gives up. Each request is one HTTP/1.0 exchange over plain `http://`.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::forwarder::Upstream;
use crate::ports::{
    Ending, KeyCheck, ModelGateway, ModelLoad, PaddockHolder, Stand, Taking, Unheld,
};
use crate::settings::Timing;

// Paddock's default is 60s. Twice that rides out one lost renewal.
const TTL: &str = "120s";

/// How often a take waiting for its grant looks for a reason to give up
const POLL: Duration = Duration::from_millis(100);

/// How long `status` waits on a gateway's own status, since it is asked while a trigger is answered
const STATUS_WITHIN: Duration = Duration::from_secs(2);

/// The most of an answer read, in bytes
const ANSWER_MAX: u64 = 1 << 20;

/// The model names `GET /v1/models` lists, which needs no key
///
/// # Errors
///
/// Why the gateway did not answer with its list.
pub(crate) fn models(gateway: &Upstream) -> Result<Vec<String>, String> {
    list(gateway, false)
}

/// The model names `GET /v1/models` lists, with the gateway's key when `keyed`
///
/// # Errors
///
/// Why the gateway did not answer with its list.
pub(crate) fn list(gateway: &Upstream, keyed: bool) -> Result<Vec<String>, String> {
    let asked = ("GET", "/v1/models");
    let (status, body) = send(gateway, asked, None, keyed, Timing::RUNNER.answer)?;
    if status != 200 {
        return Err(format!("`GET /v1/models` answered HTTP {status}"));
    }
    let not_a_list = || "`GET /v1/models` answered something other than a model list".to_owned();
    let list: Value = serde_json::from_str(&body).map_err(|_| not_a_list())?;
    let entries = list["data"].as_array().ok_or_else(not_a_list)?;
    let id = |m: &Value| m["id"].as_str().map(str::to_owned).ok_or_else(not_a_list);
    entries.iter().map(id).collect()
}

/// Whether paddock takes the gateway's key, by `GET /paddock/status`
///
/// # Errors
///
/// Why it could not be asked, or an answer that is neither yes nor no.
pub(crate) fn key_taken(gateway: &Upstream) -> Result<bool, String> {
    let asked = ("GET", "/paddock/status");
    match send(gateway, asked, None, true, Timing::RUNNER.answer)?.0 {
        200 => Ok(true),
        401 => Ok(false),
        status => Err(format!("`GET /paddock/status` answered HTTP {status}")),
    }
}

/// Who holds a lease on `model`, by `GET /paddock/status`
///
/// `None` when no lease on the model is held, or one a revoke has ended. A
/// model that serves one sequence has at most one such lease.
///
/// # Errors
///
/// Why the gateway did not answer with its status.
pub(crate) fn holder(gateway: &Upstream, model: &str) -> Result<Option<PaddockHolder>, String> {
    let asked = ("GET", "/paddock/status");
    let (status, body) = send(gateway, asked, None, true, STATUS_WITHIN)?;
    if status != 200 {
        return Err(format!("`GET /paddock/status` answered HTTP {status}"));
    }
    holder_in(&body, model)
}

// The first lease on `model` in a status body that a revoke has not ended.
fn holder_in(body: &str, model: &str) -> Result<Option<PaddockHolder>, String> {
    let read: Value = serde_json::from_str(body)
        .map_err(|_| "`GET /paddock/status` answered something other than JSON".to_owned())?;
    let mut held = read["leases"].as_array().into_iter().flatten();
    let text = |lease: &Value, field: &str| lease[field].as_str().unwrap_or_default().to_owned();
    Ok(held
        .find(|l| l["model"] == model && l["revoked"].is_null())
        .map(|lease| PaddockHolder {
            lease: text(lease, "id"),
            client: text(lease, "client"),
            what: text(lease, "note"),
            since: text(lease, "since"),
        }))
}

pub(crate) fn queued(gateway: &Upstream, model: &str) -> bool {
    let asked = ("GET", "/paddock/status");
    match send(gateway, asked, None, true, STATUS_WITHIN) {
        Ok((200, body)) => queued_in(&body, model),
        _ => false,
    }
}

fn queued_in(body: &str, model: &str) -> bool {
    let Ok(read) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let Some(turns) = on_model(&read, "models", model)
        .next()
        .and_then(|entry| entry["turns"]["lease"].as_u64())
        .filter(|turns| *turns > 0)
    else {
        return false;
    };
    held_on(&read, model) >= turns && on_model(&read, "waiters", model).next().is_some()
}

// The entries of the status's `list` that name `model`.
fn on_model<'a>(read: &'a Value, list: &str, model: &'a str) -> impl Iterator<Item = &'a Value> {
    let entries = read[list].as_array().into_iter().flatten();
    entries.filter(move |entry| entry["model"] == model)
}

// The leases on `model` that neither a revoke nor a reclaim has ended.
fn held_on(read: &Value, model: &str) -> u64 {
    let live = |lease: &&Value| lease["revoked"].is_null() && lease["reclaimable"] == false;
    on_model(read, "leases", model).filter(live).count() as u64
}

/// How busy `model` is, by `GET /paddock/status`
///
/// # Errors
///
/// Why the gateway did not answer with its status.
pub(crate) fn load(gateway: &Upstream, model: &str) -> Result<ModelLoad, String> {
    let asked = ("GET", "/paddock/status");
    let (status, body) = send(gateway, asked, None, true, STATUS_WITHIN)?;
    if status != 200 {
        return Err(format!("`GET /paddock/status` answered HTTP {status}"));
    }
    load_in(&body, model)
}

fn load_in(body: &str, model: &str) -> Result<ModelLoad, String> {
    let not_a_status =
        || "`GET /paddock/status` answered something other than its status".to_owned();
    let read: Value = serde_json::from_str(body).map_err(|_| not_a_status())?;
    read["models"].as_array().ok_or_else(not_a_status)?;
    let slots = on_model(&read, "models", model).next();
    let lease_ask = |waiter: &&Value| waiter["kind"] == "lease";
    Ok(ModelLoad {
        slots: slots
            .and_then(|entry| entry["turns"]["lease"].as_u64())
            .unwrap_or(0),
        held: held_on(&read, model),
        waiting: on_model(&read, "waiters", model).filter(lease_ask).count() as u64,
    })
}

/// The gateway port for a paddock, taking its leases on `timing`'s schedule
#[derive(Debug, Clone, Copy)]
pub(crate) struct Paddock {
    pub(crate) timing: Timing,
}

impl Default for Paddock {
    fn default() -> Self {
        Self {
            timing: Timing::RUNNER,
        }
    }
}

impl ModelGateway for Paddock {
    fn models(&self, gateway: &Upstream) -> Result<Vec<String>, String> {
        models(gateway)
    }

    fn key(&self, gateway: &Upstream) -> KeyCheck {
        match key_taken(gateway) {
            Ok(true) => KeyCheck::Taken,
            Ok(false) => KeyCheck::Refused,
            Err(why) => KeyCheck::Unsure(why),
        }
    }

    fn lease(
        &self,
        gateway: &Upstream,
        taking: &Taking<'_>,
        call: &mut dyn FnMut(&str),
    ) -> Option<Result<(), Unheld>> {
        (taking.tell)(Stand::Asking);
        let taken = Lease::take(
            gateway,
            taking.model,
            taking.note,
            taking.give_up,
            taking.tell,
            taking.ending,
            self.timing,
        );
        let mut lease = match taken {
            Ok(Some(lease)) => lease,
            Ok(None) => return Some(Err(Unheld::GaveUp)),
            Err(why) => return Some(Err(Unheld::Refused(why))),
        };
        (taking.tell)(Stand::Granted);
        call(lease.id());
        // Finishing it first settles that no later renewal can end the call.
        Some(match lease.finish() {
            Some(why) => Err(Unheld::Lost(why)),
            None => Ok(()),
        })
    }

    fn resume(
        &self,
        gateway: &Upstream,
        id: &str,
        ending: &Ending,
        call: &mut dyn FnMut(),
    ) -> Option<Result<(), Unheld>> {
        let mut lease = match Lease::resume(gateway, id, ending, self.timing) {
            Ok(lease) => lease,
            Err(why) => return Some(Err(Unheld::Refused(why))),
        };
        call();
        Some(match lease.finish() {
            Some(why) => Err(Unheld::Lost(why)),
            None => Ok(()),
        })
    }
}

/// A heartbeat lease on one model, renewed until dropped and then released
///
/// A renewal paddock refuses, or renewals it leaves unanswered for the lease's
/// whole ttl, mean the lease is gone: the call's [`Ending`] is asked to end
/// and [`Lease::finish`] says why.
#[derive(Debug)]
pub(crate) struct Lease {
    gateway: Upstream,
    id: String,
    release: Duration,
    held: Arc<Mutex<Held>>,
    // Dropping the sender stops the renewals, which are not waited for.
    renewing: Option<mpsc::Sender<()>>,
}

// What a lease is, as its holder and the renewing thread see it under one lock.
#[derive(Debug)]
enum Held {
    Renewing,
    /// Why it cannot be renewed, with the call asked to end
    Lost(String),
    /// Let go of by its holder, so nothing may end the call for it any more
    Finished,
}

impl Lease {
    /// Takes a lease on `model`, labelled `note`, waiting for its grant
    /// until `give_up` says to stop, and telling `busy` of each busy answer,
    /// as [`Stand::Turn`] or [`Stand::Busy`]
    ///
    /// `None` when it gave up, which hangs up and so leaves paddock's queue.
    /// Once held, a lease it can no longer renew asks `ending` to end the call.
    ///
    /// # Errors
    ///
    /// Why paddock refused it, other than as busy, or could not be asked.
    pub(crate) fn take(
        gateway: &Upstream,
        model: &str,
        note: &str,
        give_up: &dyn Fn() -> bool,
        busy: &dyn Fn(Stand),
        ending: &Ending,
        timing: Timing,
    ) -> Result<Option<Self>, String> {
        let body = json!({ "model": model, "hold": "heartbeat", "ttl": TTL, "note": note });
        let (status, answer) = loop {
            let Some((status, answer)) = ask_once(gateway, &body, give_up)? else {
                return Ok(None);
            };
            if status != 503 {
                break (status, answer);
            }
            busy(busy_stand(&answer));
            if !wait(timing.retry, give_up) {
                return Ok(None);
            }
        };
        let read: Value = serde_json::from_str(&answer).unwrap_or(Value::Null);
        let Some(id) = read["id"].as_str().filter(|_| status == 200) else {
            let error = read["error"].as_str().unwrap_or("no reason given");
            let why = read["reason"].as_str().or(read["detail"].as_str());
            let why = why.map(|why| format!(": {why}")).unwrap_or_default();
            return Err(format!(
                "`POST /paddock/leases` answered HTTP {status}, {error}{why}"
            ));
        };
        if !fits_header(id) {
            return Err(format!(
                "`POST /paddock/leases` granted the id {id:?}, which cannot go in a \
                 header, so the take is refused"
            ));
        }
        Ok(Some(Self::renewed(gateway, id, ending, timing, false)))
    }

    /// The lease `id`, which an earlier image of the runner took and left
    /// held, renewed at once and then as the earlier image did
    ///
    /// A renewal paddock refuses means the lease was lost in between, which
    /// asks `ending` to end the call and [`Lease::finish`] says.
    ///
    /// # Errors
    ///
    /// Why `id` cannot be a lease paddock granted.
    pub(crate) fn resume(
        gateway: &Upstream,
        id: &str,
        ending: &Ending,
        timing: Timing,
    ) -> Result<Self, String> {
        if !fits_header(id) {
            return Err(format!(
                "the carried lease id {id:?} cannot go in a header, so it is not resumed"
            ));
        }
        Ok(Self::renewed(gateway, id, ending, timing, true))
    }

    /// The id paddock granted, exactly as it wrote it
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Stops the lease's renewals, and says why it was lost, if it was
    ///
    /// Once this returns, a renewal still in flight can no longer end the
    /// call: the thread settles that under the same lock.
    pub(crate) fn finish(&mut self) -> Option<String> {
        drop(self.renewing.take());
        match std::mem::replace(&mut *self.held.lock().unwrap(), Held::Finished) {
            Held::Lost(why) => Some(why),
            Held::Renewing | Held::Finished => None,
        }
    }

    // `resumed` has the first renewal made at once and not after `timing.renew`.
    fn renewed(
        gateway: &Upstream,
        id: &str,
        ending: &Ending,
        timing: Timing,
        resumed: bool,
    ) -> Self {
        let (stop, stopped) = mpsc::channel::<()>();
        let held = Arc::new(Mutex::new(Held::Renewing));
        let (renewer, path) = (gateway.clone(), format!("/paddock/leases/{id}"));
        let (record, ending) = (Arc::clone(&held), ending.clone());
        // Not joined: a renewal in flight must not hold up the release.
        thread::spawn(move || {
            let mut renewed_at = Instant::now();
            let mut last = "no renewal was due before it ran out".to_owned();
            let mut told = false;
            let mut due = if resumed {
                Duration::ZERO
            } else {
                timing.renew
            };
            let why = loop {
                // The ttl runs from the last renewal paddock took, so the lease
                // is lost at that boundary and not before: tries go on up to it,
                // the last of them cut short so none runs past it.
                let left = timing.lapse.saturating_sub(renewed_at.elapsed());
                if left.is_zero() {
                    break last;
                }
                let waited = stopped.recv_timeout(due.min(left));
                due = timing.renew;
                if !matches!(waited, Err(RecvTimeoutError::Timeout)) {
                    return;
                }
                let left = timing.lapse.saturating_sub(renewed_at.elapsed());
                if left.is_zero() {
                    continue;
                }
                let within = timing.answer.min(left);
                match send(&renewer, ("PUT", &path), None, true, within) {
                    Ok((204 | 200, _)) => renewed_at = Instant::now(),
                    Ok((status, _)) => break format!("paddock answered HTTP {status}"),
                    Err(why) => {
                        if !told {
                            eprintln!("kelpie: cannot renew the gateway's lease {path}: {why}");
                            told = true;
                        }
                        last = why;
                    }
                }
            };
            // Under the lock the holder lets go under, so a lease let go of
            // has its call over already and is not ended.
            let mut held = record.lock().unwrap();
            if matches!(*held, Held::Renewing) {
                *held = Held::Lost(format!(
                    "the gateway's lease {path} cannot be renewed: {why}"
                ));
                ending.end();
            }
        });
        Self {
            gateway: gateway.clone(),
            id: id.to_owned(),
            release: timing.release,
            held,
            renewing: Some(stop),
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.finish();
        // A release that fails still ends once the ttl runs out unrenewed.
        let path = format!("/paddock/leases/{}", self.id);
        let _ = send(&self.gateway, ("DELETE", &path), None, true, self.release);
    }
}

// The id goes in a header and a path, so it is held to what paddock writes.
fn fits_header(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

// What a busy answer says paddock turned the take away for.
fn busy_stand(answer: &str) -> Stand {
    let read: Value = serde_json::from_str(answer).unwrap_or(Value::Null);
    match read["reason_kind"].as_str() {
        Some("turn") => Stand::Turn,
        _ => Stand::Busy,
    }
}

// One take: its status and body, or `None` once `give_up` says to stop.
fn ask_once(
    gateway: &Upstream,
    body: &Value,
    give_up: &dyn Fn() -> bool,
) -> Result<Option<(u16, String)>, String> {
    let stream = open(gateway, Timing::RUNNER.answer)?;
    let hang_up = stream.try_clone().map_err(|e| e.to_string())?;
    let request = request(gateway, ("POST", "/paddock/leases"), Some(body), true)?;
    let (sent, answered) = mpsc::channel();
    thread::spawn(move || {
        let _ = sent.send(exchange(&stream, &request, None));
    });
    loop {
        match answered.recv_timeout(POLL) {
            Ok(answer) => return answer.map(Some),
            Err(RecvTimeoutError::Timeout) if give_up() => {
                let _ = hang_up.shutdown(Shutdown::Both);
                return Ok(None);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("the take was lost".into()),
        }
    }
}

// Waits `pause`, or less when `give_up` says to stop, which it returns false for.
fn wait(pause: Duration, give_up: &dyn Fn() -> bool) -> bool {
    let mut waited = Duration::ZERO;
    while waited < pause {
        if give_up() {
            return false;
        }
        let nap = POLL.min(pause - waited);
        thread::sleep(nap);
        waited += nap;
    }
    !give_up()
}

fn open(gateway: &Upstream, within: Duration) -> Result<TcpStream, String> {
    let cannot = |e: std::io::Error| format!("cannot reach the gateway: {}", e.kind());
    let mut last = std::io::Error::from(std::io::ErrorKind::NotFound);
    for address in gateway.address().to_socket_addrs().map_err(cannot)? {
        match TcpStream::connect_timeout(&address, within) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(cannot(last))
}

// One request on a connection of its own, and its status and body, each step within `within`.
pub(crate) fn send(
    gateway: &Upstream,
    asked: (&str, &str),
    body: Option<&Value>,
    keyed: bool,
    within: Duration,
) -> Result<(u16, String), String> {
    let request = request(gateway, asked, body, keyed)?;
    exchange(&open(gateway, within)?, &request, Some(within))
}

// The request's bytes, with the gateway's key when `keyed`.
fn request(
    gateway: &Upstream,
    (method, path): (&str, &str),
    body: Option<&Value>,
    keyed: bool,
) -> Result<String, String> {
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut head = format!(
        "{method} {path} HTTP/1.0\r\nHost: {}\r\nContent-Length: {}\r\n",
        gateway.address(),
        body.len()
    );
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    if keyed {
        let key = gateway.key().ok_or("the gateway has no key")?;
        head.push_str(&format!("Authorization: Bearer {}\r\n", key.expose()));
    }
    Ok(format!("{head}\r\n{body}"))
}

// Sends `request` and reads the answer to its end, for at most `within`.
fn exchange(
    stream: &TcpStream,
    request: &str,
    within: Option<Duration>,
) -> Result<(u16, String), String> {
    let lost = |e: std::io::Error| format!("the gateway did not answer: {}", e.kind());
    stream.set_read_timeout(within).map_err(lost)?;
    stream.set_write_timeout(within).map_err(lost)?;
    let mut writer = stream;
    writer.write_all(request.as_bytes()).map_err(lost)?;
    let mut bytes = Vec::new();
    stream
        .take(ANSWER_MAX)
        .read_to_end(&mut bytes)
        .map_err(lost)?;
    let text = String::from_utf8_lossy(&bytes);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok());
    let status = status.ok_or("the gateway's answer is not HTTP")?;
    Ok((status, body.to_owned()))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod status_tests;
