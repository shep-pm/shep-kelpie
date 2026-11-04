//! A stand-in OpenAI-compatible server, for kelpie's own local reviewer
//!
//! It answers `GET …/models` with an empty list and each `POST …/chat/completions`
//! with the next scripted reply (as server-sent events when the request streams,
//! or cut off while thinking), or `CLEAN` once the script runs out. It keeps every
//! chat body, and each request's line, `Authorization` and `x-paddock-*` headers.
//! Given a key, it answers paddock's routes too: `/paddock/status` to that key
//! alone, a lease's take with the next scripted [`Take`] or a grant, and a renewal
//! or release with `204`, unless told to refuse or hang. [`like_openai`](StandInEndpoint::like_openai)
//! instead answers `/models` to that key alone and `404` to every `/paddock/` route.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

use super::Hold;
use crate::ports::CLEAN;

/// One scripted answer to a lease's take
#[derive(Debug, Clone)]
pub enum Take {
    /// This HTTP status, with this body
    Status(u16, &'static str),
    /// No answer, until the client hangs up, as a take paddock keeps queued
    Hang,
    /// No answer until the test releases this, and then a grant
    Held(Hold),
}

/// How the stand-in answers a lease's renewal
#[derive(Debug, Clone, Copy, Default)]
enum Renewals {
    /// `204`, as paddock does
    #[default]
    Granted,
    /// This HTTP status, as paddock answers a lease it no longer holds
    Refused(u16),
    /// No answer, until the client hangs up
    Hung,
}

/// One scripted answer to a chat request
#[derive(Debug, Clone)]
pub enum Answer {
    /// A chat completion whose message says this
    Says(&'static str),
    /// This HTTP status, with this body
    Status(u16, &'static str),
    /// A reply that spent this many tokens thinking and ran out of them: `finish_reason`
    /// `length`, a null `content` and a `reasoning_content`
    CutOff(u32),
    /// A reply that ran out of tokens partway through writing this, with `finish_reason` `length`
    RunsOutSaying(&'static str),
}

#[derive(Debug, Default)]
struct Shared {
    answers: VecDeque<Answer>,
    requests: Vec<Value>,
    lines: Vec<String>,
    auths: Vec<Option<String>>,
    every_auth: Vec<Vec<String>>,
    paddock: Vec<Vec<(String, String)>>,
    metrics: Option<String>,
    models: Vec<String>,
    models_body: Option<String>,
    renewals: Renewals,
    paddock_status: Option<String>,
    key: Option<String>,
    takes: VecDeque<Take>,
    openai: bool,
    models_status: Option<u16>,
    distinct_leases: bool,
    granted: u32,
    released_with: String,
}

/// A server on a free local port, until the test ends
#[derive(Debug, Clone)]
pub struct StandInEndpoint {
    url: String,
    shared: Arc<Mutex<Shared>>,
}

impl StandInEndpoint {
    /// Starts one that answers chat requests with `answers`, in order
    pub fn start(answers: impl IntoIterator<Item = Answer>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let shared = Arc::new(Mutex::new(Shared {
            answers: answers.into_iter().collect(),
            ..Shared::default()
        }));
        let serving = Arc::clone(&shared);
        // The thread outlives the test only while the test binary runs.
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let serving = Arc::clone(&serving);
                thread::spawn(move || serve(stream, &serving));
            }
        });
        Self { url, shared }
    }

    /// Answers as paddock does: `/models` lists `models`, and its own
    /// routes take `key` alone
    pub fn like_paddock(self, key: &str, models: &[&str]) -> Self {
        let mut shared = self.shared.lock().unwrap();
        shared.key = Some(format!("Bearer {key}"));
        shared.models = models.iter().map(|&m| m.to_owned()).collect();
        drop(shared);
        self
    }

    /// Answers as a bare OpenAI-compatible gateway does: `/models` lists
    /// `models` to `key` alone and refuses any other with `401`, and it has
    /// no `/paddock/` route
    pub fn like_openai(self, key: &str, models: &[&str]) -> Self {
        let this = self.like_paddock(key, models);
        this.shared.lock().unwrap().openai = true;
        this
    }

    /// Grants `L1`, `L2` and so on to each lease's take, not `L1` to every one
    pub fn granting_distinct_leases(self) -> Self {
        self.shared.lock().unwrap().distinct_leases = true;
        self
    }

    /// Has each lease's release answer with these headers, as paddock does
    /// with its energy
    pub fn releasing_with(self, headers: &[(&str, &str)]) -> Self {
        let lines = headers.iter().map(|(n, v)| format!("{n}: {v}\r\n"));
        self.shared.lock().unwrap().released_with = lines.collect();
        self
    }

    /// Has `/models` answer this HTTP status, whatever key it gets
    pub fn with_models_status(self, status: u16) -> Self {
        self.shared.lock().unwrap().models_status = Some(status);
        self
    }

    /// Has `/models` answer `200` with this body, whatever it is
    pub fn with_models_body(self, body: &str) -> Self {
        self.shared.lock().unwrap().models_body = Some(body.to_owned());
        self
    }

    /// Has `/paddock/status` answer with this body, as paddock lists its leases
    pub fn with_paddock_status(self, body: &str) -> Self {
        self.shared.lock().unwrap().paddock_status = Some(body.to_owned());
        self
    }

    /// Refuses each lease renewal with this HTTP status
    pub fn refusing_renewals(self, status: u16) -> Self {
        self.shared.lock().unwrap().renewals = Renewals::Refused(status);
        self
    }

    /// Leaves each lease renewal unanswered, until the client hangs up
    pub fn hanging_renewals(self) -> Self {
        self.shared.lock().unwrap().renewals = Renewals::Hung;
        self
    }

    /// Answers the next lease takes with `takes`, in order, and then grants
    pub fn with_takes(self, takes: impl IntoIterator<Item = Take>) -> Self {
        self.shared.lock().unwrap().takes = takes.into_iter().collect();
        self
    }

    /// The `Authorization` every request carried, in order, `None` where it had none
    pub fn authorizations(&self) -> Vec<Option<String>> {
        self.shared.lock().unwrap().auths.clone()
    }

    /// Every `Authorization` header each request carried, in order, not only its last
    pub fn authorization_headers(&self) -> Vec<Vec<String>> {
        self.shared.lock().unwrap().every_auth.clone()
    }

    /// Every `x-paddock-*` header each request carried, in order: the name in
    /// lower case, then the value
    pub fn paddock_headers(&self) -> Vec<Vec<(String, String)>> {
        self.shared.lock().unwrap().paddock.clone()
    }

    /// Has `/metrics` answer with this page, as a Prometheus exporter does
    pub fn with_metrics(self, page: &str) -> Self {
        self.shared.lock().unwrap().metrics = Some(page.to_owned());
        self
    }

    /// The URL of its `/metrics` page
    pub fn metrics_url(&self) -> String {
        format!("{}/metrics", self.host())
    }

    /// Its Ollama host: the base URL without `/v1`
    pub fn host(&self) -> &str {
        self.url.strip_suffix("/v1").unwrap_or(&self.url)
    }

    /// Its base URL, up to and including `/v1`
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The request line of every request it got, such as `POST /v1/chat/completions HTTP/1.1`
    pub fn seen(&self) -> Vec<String> {
        self.shared.lock().unwrap().lines.clone()
    }

    /// Every chat request's body, and every lease take's, in order
    pub fn requests(&self) -> Vec<Value> {
        self.shared.lock().unwrap().requests.clone()
    }
}

/// A base URL nothing answers on: port 1, which is never served
pub fn unreachable_url() -> String {
    "http://127.0.0.1:1/v1".to_owned()
}

fn serve(stream: TcpStream, shared: &Mutex<Shared>) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let (mut length, mut auth, mut every, mut paddock) = (0, None, Vec::new(), Vec::new());
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        match header.split_once(':') {
            Some((name, value)) if name.eq_ignore_ascii_case("content-length") => {
                length = value.trim().parse().unwrap_or(0);
            }
            Some((name, value)) if name.eq_ignore_ascii_case("authorization") => {
                every.push(value.trim().to_owned());
                auth = Some(value.trim().to_owned());
            }
            Some((name, value)) if name.trim().to_ascii_lowercase().starts_with("x-paddock-") => {
                paddock.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
            }
            _ => {}
        }
    }
    {
        // Under one lock, so the two records stay in the same order.
        let mut held = shared.lock().unwrap();
        held.lines.push(request_line.trim().to_owned());
        held.every_auth.push(every);
        held.paddock.push(paddock);
        held.auths.push(auth.clone());
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let mut extra = String::new();
    let (status, reply) = if request_line.contains("/metrics ") {
        match &shared.lock().unwrap().metrics {
            Some(page) => (200, page.clone()),
            None => (404, "404 page not found".to_owned()),
        }
    } else if request_line.contains("/models ") {
        let held = shared.lock().unwrap();
        let (models, scripted) = (held.models.clone(), held.models_body.clone());
        let (refused, status) = (held.openai && held.key != auth, held.models_status);
        drop(held);
        let data: Vec<_> = models.iter().map(|id| json!({ "id": id })).collect();
        let list = json!({ "object": "list", "data": data }).to_string();
        match (status, refused) {
            (Some(status), _) => (status, json!({ "error": "scripted" }).to_string()),
            (None, true) => (401, json!({ "error": "unauthorized" }).to_string()),
            (None, false) => (200, scripted.unwrap_or(list)),
        }
    } else if request_line.contains(" /paddock/") && shared.lock().unwrap().openai {
        (404, "404 page not found".to_owned())
    } else if request_line.contains(" /paddock/") {
        let mut held = shared.lock().unwrap();
        let keyed = held.key.is_some() && held.key == auth;
        let take = request_line.starts_with("POST ");
        let renewals = held.renewals;
        let released_with = held.released_with.clone();
        let listing = held
            .paddock_status
            .clone()
            .unwrap_or_else(|| "{}".to_owned());
        let scripted = (keyed && take).then(|| {
            held.requests
                .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
            held.takes.pop_front()
        });
        drop(held);
        match (keyed, take, scripted.flatten()) {
            (false, ..) => (401, json!({ "error": "unauthorized" }).to_string()),
            (true, true, Some(Take::Hang)) => {
                let _ = reader.read_to_end(&mut Vec::new());
                return;
            }
            (true, true, Some(Take::Held(hold))) => {
                hold.block();
                (200, grant(shared))
            }
            (true, true, Some(Take::Status(status, body))) => (status, body.to_owned()),
            (true, true, None) => (200, grant(shared)),
            (true, false, _) if request_line.starts_with("GET ") => (200, listing),
            (true, false, _) if request_line.starts_with("PUT ") => match renewals {
                Renewals::Granted => (204, String::new()),
                Renewals::Refused(status) => {
                    (status, json!({ "error": "unknown_lease" }).to_string())
                }
                Renewals::Hung => {
                    let _ = reader.read_to_end(&mut Vec::new());
                    return;
                }
            },
            (true, false, _) => {
                extra = released_with;
                (204, String::new())
            }
        }
    } else {
        let mut shared = shared.lock().unwrap();
        shared
            .requests
            .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
        let streams = shared.requests.last().is_some_and(|b| b["stream"] == true);
        match shared.answers.pop_front().unwrap_or(Answer::Says(CLEAN)) {
            Answer::Says(content) if streams => (200, events(content)),
            Answer::Says(content) => (
                200,
                json!({
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": content },
                        "finish_reason": "stop",
                    }],
                })
                .to_string(),
            ),
            Answer::Status(status, body) => (status, body.to_owned()),
            Answer::RunsOutSaying(content) => (
                200,
                json!({
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": content },
                        "finish_reason": "length",
                    }],
                    "usage": { "prompt_tokens": 700, "completion_tokens": 2048 },
                })
                .to_string(),
            ),
            Answer::CutOff(tokens) => (
                200,
                json!({
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "reasoning_content": "Let me think about this change again",
                        },
                        "finish_reason": "length",
                    }],
                    "usage": {
                        "prompt_tokens": 700,
                        "completion_tokens": tokens,
                        "total_tokens": u64::from(tokens) + 700,
                    },
                })
                .to_string(),
            ),
        }
    };
    let kind = if reply.starts_with("data: ") {
        "text/event-stream"
    } else {
        "application/json"
    };
    let mut stream = reader.into_inner();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Stand-in\r\nContent-Type: {kind}\r\n{extra}\
         Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
}

// A lease's grant, with the next id when the stand-in hands out distinct ones.
fn grant(shared: &Mutex<Shared>) -> String {
    let mut held = shared.lock().unwrap();
    let id = if held.distinct_leases {
        held.granted += 1;
        format!("L{}", held.granted)
    } else {
        "L1".to_owned()
    };
    json!({ "id": id, "ttl": "120s" }).to_string()
}

// What a streaming chat request is answered with: the words in one chunk, the
// stop, the usage and the end marker, as server-sent events.
fn events(content: &str) -> String {
    let chunk = |delta: Value, finish: Value, usage: Value| {
        let chunk = json!({
            "id": "stand-in",
            "object": "chat.completion.chunk",
            "model": "stand-in",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
            "usage": usage,
        });
        format!("data: {chunk}\n\n")
    };
    let usage = json!({ "prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5 });
    format!(
        "{}{}data: [DONE]\n\n",
        chunk(
            json!({ "role": "assistant", "content": content }),
            Value::Null,
            Value::Null
        ),
        chunk(json!({}), json!("stop"), usage),
    )
}
