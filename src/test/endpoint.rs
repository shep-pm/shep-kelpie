//! A stand-in OpenAI-compatible server, for kelpie's own local reviewer
//!
//! It answers `GET …/models` with an empty list, `GET /api/ps` with what the
//! test gave it (a 404, as a server that is not Ollama answers, until then), and each
//! `POST …/chat/completions` with the next scripted reply (as server-sent events
//! when the request streams), or `CLEAN` once the script runs out. It keeps
//! every chat request's body, and the request line and `Authorization` of
//! every request it got. Given a key, it answers paddock's own routes too:
//! `/paddock/status` to that key alone, a lease's take with the next scripted
//! [`Take`] or a grant, and a renewal or release with `204`, as paddock does.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

/// One scripted answer to a lease's take
#[derive(Debug, Clone)]
pub(crate) enum Take {
    /// This HTTP status, with this body
    Status(u16, &'static str),
    /// No answer, until the client hangs up, as a take paddock keeps queued
    Hang,
}

/// One scripted answer to a chat request
#[derive(Debug, Clone)]
pub(crate) enum Answer {
    /// A chat completion whose message says this
    Says(&'static str),
    /// This HTTP status, with this body
    Status(u16, &'static str),
}

#[derive(Debug, Default)]
struct Shared {
    answers: VecDeque<Answer>,
    requests: Vec<Value>,
    lines: Vec<String>,
    auths: Vec<Option<String>>,
    ps: Option<String>,
    metrics: Option<String>,
    models: Vec<String>,
    key: Option<String>,
    takes: VecDeque<Take>,
}

/// A server on a free local port, until the test ends
#[derive(Debug, Clone)]
pub(crate) struct StandInEndpoint {
    url: String,
    shared: Arc<Mutex<Shared>>,
}

impl StandInEndpoint {
    /// Starts one that answers chat requests with `answers`, in order
    pub(crate) fn start(answers: impl IntoIterator<Item = Answer>) -> Self {
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
                serve(stream, &serving);
            }
        });
        Self { url, shared }
    }

    /// Has `/api/ps` answer with this body, as Ollama does
    pub(crate) fn with_ps(self, body: &str) -> Self {
        self.shared.lock().unwrap().ps = Some(body.to_owned());
        self
    }

    /// Answers as paddock does: `/models` lists `models`, and its own
    /// routes take `key` alone
    pub(crate) fn like_paddock(self, key: &str, models: &[&str]) -> Self {
        let mut shared = self.shared.lock().unwrap();
        shared.key = Some(format!("Bearer {key}"));
        shared.models = models.iter().map(|&m| m.to_owned()).collect();
        drop(shared);
        self
    }

    /// Answers the next lease takes with `takes`, in order, and then grants
    pub(crate) fn with_takes(self, takes: impl IntoIterator<Item = Take>) -> Self {
        self.shared.lock().unwrap().takes = takes.into_iter().collect();
        self
    }

    /// The `Authorization` every request carried, in order, `None` where it had none
    pub(crate) fn authorizations(&self) -> Vec<Option<String>> {
        self.shared.lock().unwrap().auths.clone()
    }

    /// Has `/metrics` answer with this page, as a Prometheus exporter does
    pub(crate) fn with_metrics(self, page: &str) -> Self {
        self.shared.lock().unwrap().metrics = Some(page.to_owned());
        self
    }

    /// The URL of its `/metrics` page
    pub(crate) fn metrics_url(&self) -> String {
        format!("{}/metrics", self.host())
    }

    /// Its Ollama host: the base URL without `/v1`
    pub(crate) fn host(&self) -> &str {
        self.url.strip_suffix("/v1").unwrap_or(&self.url)
    }

    /// Its base URL, up to and including `/v1`
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// The request line of every request it got, such as `POST /v1/chat/completions HTTP/1.1`
    pub(crate) fn seen(&self) -> Vec<String> {
        self.shared.lock().unwrap().lines.clone()
    }

    /// Every chat request's body, and every lease take's, in order
    pub(crate) fn requests(&self) -> Vec<Value> {
        self.shared.lock().unwrap().requests.clone()
    }
}

/// A base URL nothing answers on: port 1, which is never served
pub(crate) fn unreachable_url() -> String {
    "http://127.0.0.1:1/v1".to_owned()
}

fn serve(stream: TcpStream, shared: &Mutex<Shared>) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    shared
        .lock()
        .unwrap()
        .lines
        .push(request_line.trim().to_owned());
    let (mut length, mut auth) = (0, None);
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
                auth = Some(value.trim().to_owned());
            }
            _ => {}
        }
    }
    shared.lock().unwrap().auths.push(auth.clone());
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let (status, reply) = if request_line.contains("/metrics ") {
        match &shared.lock().unwrap().metrics {
            Some(page) => (200, page.clone()),
            None => (404, "404 page not found".to_owned()),
        }
    } else if request_line.contains("/api/ps ") {
        match &shared.lock().unwrap().ps {
            Some(body) => (200, body.clone()),
            None => (404, "404 page not found".to_owned()),
        }
    } else if request_line.contains("/models ") {
        let models = shared.lock().unwrap().models.clone();
        let data: Vec<_> = models.iter().map(|id| json!({ "id": id })).collect();
        (200, json!({ "object": "list", "data": data }).to_string())
    } else if request_line.contains(" /paddock/") {
        let mut held = shared.lock().unwrap();
        let keyed = held.key.is_some() && held.key == auth;
        let take = request_line.starts_with("POST ");
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
            (true, true, Some(Take::Status(status, body))) => (status, body.to_owned()),
            (true, true, None) => (200, json!({ "id": "L1", "ttl": "120s" }).to_string()),
            (true, false, _) if request_line.starts_with("GET ") => (200, "{}".to_owned()),
            (true, false, _) => (204, String::new()),
        }
    } else {
        let mut shared = shared.lock().unwrap();
        shared
            .requests
            .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
        let streams = shared.requests.last().is_some_and(|b| b["stream"] == true);
        match shared.answers.pop_front().unwrap_or(Answer::Says("CLEAN")) {
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
        "HTTP/1.1 {status} Stand-in\r\nContent-Type: {kind}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
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
