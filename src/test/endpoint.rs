//! A stand-in OpenAI-compatible server, for kelpie's own local reviewer
//!
//! It answers `GET …/models` with an empty list, `GET /api/ps` with what the
//! test gave it (a 404, as a server that is not Ollama answers, until then), and each
//! `POST …/chat/completions` with the next scripted reply (as server-sent events
//! when the request streams), or `CLEAN` once the script runs out. It keeps
//! every chat request's body, and the request line of every request it got.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

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
    ps: Option<String>,
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
            requests: Vec::new(),
            lines: Vec::new(),
            ps: None,
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

    /// Every chat request's body, in order
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
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let (status, reply) = if request_line.contains("/api/ps ") {
        match &shared.lock().unwrap().ps {
            Some(body) => (200, body.clone()),
            None => (404, "404 page not found".to_owned()),
        }
    } else if request_line.contains("/models ") {
        (200, json!({ "object": "list", "data": [] }).to_string())
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
