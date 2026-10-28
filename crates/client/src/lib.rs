//! A runner's socket, from the asking side (ADR 0017)
//!
//! [`connect`] opens `run/<project>.sock`, makes the hello and refuses a runner
//! on an older protocol. [`Connection::ask`] sends one [`Request`] and reads its
//! [`Reply`] within a deadline.
//!
//! The outcomes a caller acts on are separate. [`Unreachable`] covers a socket
//! that is missing, refused, silent at the hello or on an older protocol, so
//! the caller can fall back to `shep trigger`. [`Failure::TimedOut`] is its own
//! outcome: the runner may still take the request, so asking again would do it
//! twice, and a connection that timed out asks nothing more.
//!
//! Uses only `shep-kelpie-protocol` and `shep-kelpie-core` (for `ipc`).

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

use std::fmt;
use std::io;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;

use shep_kelpie_core::ipc::{self, ClientStream};
use shep_kelpie_protocol::frame::{self, Hello, LONGEST_FRAME, PROTOCOL, UnknownRequest};
use shep_kelpie_protocol::{Reply, Request};

/// Why a runner cannot be asked at all, so nothing was sent
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreachable {
    /// What was found, such as `No such file or directory` or the protocol it speaks
    pub why: String,
}

impl fmt::Display for Unreachable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.why)
    }
}

impl core::error::Error for Unreachable {}

/// Why an ask has no reply
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The deadline passed. The runner may still take the request, so the
    /// caller does not send it again, and this connection is spent
    TimedOut,
    /// The connection broke or the runner said something that is not a reply,
    /// with what happened. The request may have been taken
    Broken(String),
    /// The runner is on this protocol but has no variant for the request
    Unknown(UnknownRequest),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut => f.write_str("the runner did not answer in time"),
            Self::Broken(why) => write!(f, "the runner's socket broke: {why}"),
            Self::Unknown(unknown) => unknown.fmt(f),
        }
    }
}

impl core::error::Error for Failure {}

/// A connection past the hello, asking one request at a time
#[derive(Debug)]
pub struct Connection {
    stream: BufReader<ClientStream>,
    // Set by a timeout: an answer may still arrive, and would be read as the next ask's.
    spent: bool,
}

/// Connects to the runner's socket and makes the hello
///
/// `deadline` bounds the connect and the hello together.
///
/// # Errors
///
/// [`Unreachable`] when nothing answers at `socket`, the runner is silent or
/// says something that is no hello, or it speaks a protocol below this one's.
pub async fn connect(socket: &Path, deadline: Duration) -> Result<Connection, Unreachable> {
    let unreachable = |why: String| Unreachable { why };
    let opened = async {
        let stream = ipc::connect(socket).await?;
        let mut stream = BufReader::new(stream);
        let hello = frame::encode(&Hello { protocol: PROTOCOL }).map_err(io::Error::other)?;
        stream.write_all(hello.as_bytes()).await?;
        let line = read_line(&mut stream).await?;
        Ok::<_, io::Error>((stream, line))
    };
    let (stream, line) = match timeout(deadline, opened).await {
        Ok(Ok(opened)) => opened,
        Ok(Err(e)) => return Err(unreachable(e.to_string())),
        Err(_) => return Err(unreachable("no hello within the deadline".into())),
    };
    let hello: Hello = serde_json::from_str(&line)
        .map_err(|_| unreachable(format!("answered {line:?}, which is no hello")))?;
    if hello.protocol < PROTOCOL {
        return Err(unreachable(format!(
            "the runner speaks protocol {}, and this kelpie speaks {PROTOCOL}",
            hello.protocol
        )));
    }
    Ok(Connection {
        stream,
        spent: false,
    })
}

impl Connection {
    /// Sends `request` and reads its reply, within `deadline`
    ///
    /// A runner's refusal is a [`Reply::Refused`], not a failure.
    ///
    /// # Errors
    ///
    /// [`Failure::TimedOut`] when the deadline passes, which spends the
    /// connection: every later ask gives [`Failure::Broken`] and sends nothing.
    /// [`Failure::Unknown`] when the runner has no such request, and
    /// [`Failure::Broken`] when the connection ends or the reply does not read.
    pub async fn ask(&mut self, request: &Request, deadline: Duration) -> Result<Reply, Failure> {
        if self.spent {
            return Err(Failure::Broken(
                "an earlier ask timed out on this connection".into(),
            ));
        }
        match timeout(deadline, self.exchange(request)).await {
            Ok(done) => done,
            Err(_) => {
                self.spent = true;
                Err(Failure::TimedOut)
            }
        }
    }

    async fn exchange(&mut self, request: &Request) -> Result<Reply, Failure> {
        let broken = |e: io::Error| Failure::Broken(e.to_string());
        let line = frame::encode(request).map_err(|e| Failure::Broken(e.to_string()))?;
        self.stream
            .write_all(line.as_bytes())
            .await
            .map_err(broken)?;
        let line = read_line(&mut self.stream).await.map_err(broken)?;
        let shown: Value = serde_json::from_str(&line)
            .map_err(|e| Failure::Broken(format!("answered {line:?} ({e})")))?;
        if let Some(unknown) = unknown_request(&shown) {
            return Err(Failure::Unknown(unknown));
        }
        Reply::decode(request, &line)
            .map_err(|e| Failure::Broken(format!("answered {line:?} ({e})")))
    }
}

// The runner's answer to a request it has no variant for: `request` and `version` alone.
fn unknown_request(shown: &Value) -> Option<UnknownRequest> {
    let fields = shown.as_object()?;
    if fields.len() != 2 {
        return None;
    }
    serde_json::from_value(shown.clone()).ok()
}

// The next line without its end, within the frame bound.
async fn read_line(stream: &mut BufReader<ClientStream>) -> io::Result<String> {
    let mut line = Vec::new();
    let mut bounded = stream.take(LONGEST_FRAME as u64 + 1);
    bounded.read_until(b'\n', &mut line).await?;
    if line.pop() != Some(b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the connection ended before the line did, or the line was too long",
        ));
    }
    String::from_utf8(line).map_err(io::Error::other)
}

#[cfg(test)]
mod tests;
