//! A stand-in OpenAI-compatible server, for kelpie's own local reviewer
//!
//! It answers `GET …/models` with an empty list, and each
//! `POST …/chat/completions` with the next scripted reply, or `CLEAN` once
//! the script runs out. It keeps every chat request's body.

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

    /// Its base URL, up to and including `/v1`
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// Every chat request's body, in order
    pub(crate) fn requests(&self) -> Vec<Value> {
        self.shared.lock().unwrap().requests.clone()
    }
}

/// A base URL nothing answers on: a port bound and let go at once
pub(crate) fn unreachable_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}/v1", listener.local_addr().unwrap())
}

fn serve(stream: TcpStream, shared: &Mutex<Shared>) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
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
    let (status, reply) = if request_line.contains("/models ") {
        (200, json!({ "object": "list", "data": [] }).to_string())
    } else {
        let mut shared = shared.lock().unwrap();
        shared
            .requests
            .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
        match shared.answers.pop_front().unwrap_or(Answer::Says("CLEAN")) {
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
    let mut stream = reader.into_inner();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Stand-in\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
}
