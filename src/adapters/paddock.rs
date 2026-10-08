//! Paddock's own routes, for a gateway that is paddock, a shep dog
//!
//! `GET /v1/models` lists its models and needs no key. `GET /paddock/status`
//! needs one, so it tells a key paddock takes from one it refuses. A worker
//! turn on a model behind it holds a heartbeat lease, so nothing evicts the
//! model midway: `POST /paddock/leases` waits for the grant, a `PUT` renews
//! it, and a `DELETE` releases it when the turn ends. A take paddock turns
//! away as busy is asked again until the turn gives up. Each request is one
//! HTTP/1.0 exchange over plain `http://`, as the forwarder's are.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::forwarder::Upstream;

// Paddock's default is 60s. Twice that rides out one lost renewal.
const TTL: &str = "120s";

/// How often a take waiting for its grant looks for a reason to give up
const POLL: Duration = Duration::from_millis(100);

/// The most of an answer read, in bytes
const ANSWER_MAX: u64 = 1 << 20;

/// How long each step of talking to paddock may take
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timing {
    /// How long a request gets to connect, and one other than a take to answer
    pub(crate) answer: Duration,
    /// How long the release gets, since a turn's end waits on it
    pub(crate) release: Duration,
    /// How often a held lease is renewed: a quarter of its ttl
    pub(crate) renew: Duration,
    /// How long a take paddock turned away as busy waits to ask again
    pub(crate) retry: Duration,
}

impl Timing {
    /// What a runner uses
    pub(crate) const RUNNER: Self = Self {
        answer: Duration::from_secs(10),
        release: Duration::from_secs(3),
        renew: Duration::from_secs(30),
        retry: Duration::from_secs(30),
    };
}

/// The model names `GET /v1/models` lists, which needs no key
///
/// # Errors
///
/// Why the gateway did not answer with its list.
pub(crate) fn models(gateway: &Upstream) -> Result<Vec<String>, String> {
    let asked = ("GET", "/v1/models");
    let (status, body) = send(gateway, asked, None, false, Timing::RUNNER.answer)?;
    if status != 200 {
        return Err(format!("`GET /v1/models` answered HTTP {status}"));
    }
    let list: Value = serde_json::from_str(&body)
        .map_err(|_| "`GET /v1/models` answered something other than a model list")?;
    let ids = list["data"].as_array().into_iter().flatten();
    Ok(ids
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect())
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

/// A heartbeat lease on one model, renewed until dropped and then released
#[derive(Debug)]
pub(crate) struct Lease {
    gateway: Upstream,
    id: String,
    release: Duration,
    // Dropping the sender stops the renewals.
    renewing: Option<(mpsc::Sender<()>, thread::JoinHandle<()>)>,
}

impl Lease {
    /// Takes a lease on `model`, labelled `note`, waiting for its grant
    /// until `give_up` says to stop
    ///
    /// `None` when it gave up, which hangs up and so leaves paddock's queue.
    ///
    /// # Errors
    ///
    /// Why paddock refused it, other than as busy, or could not be asked.
    pub(crate) fn take(
        gateway: &Upstream,
        model: &str,
        note: &str,
        give_up: &dyn Fn() -> bool,
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
        Ok(Some(Self::renewed(gateway, id, timing)))
    }

    fn renewed(gateway: &Upstream, id: &str, timing: Timing) -> Self {
        let (stop, stopped) = mpsc::channel();
        let (renewer, path) = (gateway.clone(), format!("/paddock/leases/{id}"));
        let renewing = thread::spawn(move || {
            let mut told = false;
            while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(timing.renew) {
                let renewed = send(&renewer, ("PUT", &path), None, true, timing.answer);
                let why = match renewed {
                    Ok((204 | 200, _)) => continue,
                    Ok((status, _)) => format!("HTTP {status}"),
                    Err(why) => why,
                };
                if !told {
                    eprintln!("kelpie: cannot renew the gateway's lease {path}: {why}");
                    told = true;
                }
            }
        });
        Self {
            gateway: gateway.clone(),
            id: id.to_owned(),
            release: timing.release,
            renewing: Some((stop, renewing)),
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some((stop, renewing)) = self.renewing.take() {
            drop(stop);
            let _ = renewing.join();
        }
        // A release that fails still ends once the ttl runs out unrenewed.
        let path = format!("/paddock/leases/{}", self.id);
        let _ = send(&self.gateway, ("DELETE", &path), None, true, self.release);
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
fn send(
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
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;
    use crate::settings::{EndpointUrl, GatewayKey};
    use crate::test::{StandInEndpoint, Take};

    const QUICK: Timing = Timing {
        answer: Duration::from_secs(5),
        release: Duration::from_secs(5),
        renew: Duration::from_millis(20),
        retry: Duration::from_millis(20),
    };

    const BUSY: &str = r#"{"error":"busy","reason":"qwen is loading"}"#;

    fn gateway(server: &StandInEndpoint) -> Upstream {
        let base = EndpointUrl::try_from(server.url().to_owned()).unwrap();
        Upstream::new(&base)
            .unwrap()
            .with_key(GatewayKey::for_test("pk"))
    }

    fn calls(server: &StandInEndpoint) -> Vec<String> {
        let seen = server.seen().into_iter();
        seen.filter_map(|l| l.strip_suffix(" HTTP/1.0").map(str::to_owned))
            .collect()
    }

    #[test]
    fn a_take_asks_for_a_heartbeat_renews_it_and_releases_it_when_dropped() {
        let server = StandInEndpoint::start([]).like_paddock("pk", &[]);
        let lease = Lease::take(
            &gateway(&server),
            "qwen",
            "kelpie #7 worker",
            &|| false,
            QUICK,
        );
        let lease = lease.unwrap().unwrap();
        let asked = &server.requests()[0];
        let wanted = json!({ "model": "qwen", "hold": "heartbeat", "ttl": "120s",
                             "note": "kelpie #7 worker" });
        assert_eq!(asked, &wanted);
        let until = Instant::now() + Duration::from_secs(5);
        while !calls(&server).contains(&"PUT /paddock/leases/L1".to_owned()) {
            assert!(Instant::now() < until, "no renewal: {:?}", server.seen());
            thread::sleep(Duration::from_millis(10));
        }
        drop(lease);
        assert_eq!(calls(&server).last().unwrap(), "DELETE /paddock/leases/L1");
    }

    #[test]
    fn a_busy_take_is_asked_again_and_a_refused_one_names_why() {
        let server = StandInEndpoint::start([])
            .like_paddock("pk", &[])
            .with_takes([Take::Status(503, BUSY)]);
        let lease = Lease::take(&gateway(&server), "qwen", "n", &|| false, QUICK).unwrap();
        assert!(lease.is_some());
        let takes = calls(&server)
            .iter()
            .filter(|l| l.starts_with("POST"))
            .count();
        assert_eq!(takes, 2);

        let wrong = gateway(&server).with_key(GatewayKey::for_test("other"));
        let err = Lease::take(&wrong, "qwen", "n", &|| false, QUICK).unwrap_err();
        assert_eq!(
            err,
            "`POST /paddock/leases` answered HTTP 401, unauthorized"
        );
        let bad = r#"{"error":"bad_lease_request","detail":"ttl is at most 1h"}"#;
        let server = StandInEndpoint::start([])
            .like_paddock("pk", &[])
            .with_takes([Take::Status(400, bad)]);
        let err = Lease::take(&gateway(&server), "qwen", "n", &|| false, QUICK).unwrap_err();
        assert!(
            err.ends_with("bad_lease_request: ttl is at most 1h"),
            "{err}"
        );
    }

    #[test]
    fn a_take_paddock_keeps_queued_or_busy_ends_when_the_turn_gives_up() {
        for take in [Take::Hang, Take::Status(503, BUSY)] {
            let server = StandInEndpoint::start([])
                .like_paddock("pk", &[])
                .with_takes([take.clone(), take]);
            let looks = AtomicUsize::new(0);
            let give_up = || looks.fetch_add(1, Ordering::SeqCst) >= 3;
            let started = Instant::now();
            let lease = Lease::take(&gateway(&server), "qwen", "n", &give_up, QUICK);
            assert!(matches!(lease, Ok(None)), "{lease:?}");
            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(!calls(&server).iter().any(|l| l.starts_with("DELETE")));
        }
    }
}
