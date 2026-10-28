//! The runner's own socket: the typed protocol, one thread a connection
//!
//! `run/<project>.sock` under kelpie's home answers the requests of
//! `shep-kelpie-protocol` (ADR 0017). A connection sends a hello and is
//! answered with the runner's, then asks a request a line and gets the body
//! `shep trigger` returns for it. The runner has no tokio, so each connection
//! is a thread of its own on `ipc::blocking`. Everything is bounded: a line
//! past [`LONGEST_FRAME`], silence, and too many connections close it.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use super::Runner;
use super::posted::Posted;
use super::trigger::{answer_request, wakes};
use crate::home::Build;
use crate::ipc::blocking::{Listener, Stream};
use crate::reply::Reply;
use shep_kelpie_protocol::frame::{FrameError, Hello, LONGEST_FRAME, PROTOCOL, UnknownRequest};

/// How long a connection has to send its hello
const HELLO_WITHIN: Duration = Duration::from_secs(10);

/// How long a connection may stay silent between two requests
const IDLE_WITHIN: Duration = Duration::from_secs(300);

/// How long a write to a peer that reads nothing may take
const WRITE_WITHIN: Duration = Duration::from_secs(10);

/// How many connections the runner serves at once
const MOST_CONNECTIONS: usize = 32;

/// How long the accept loop rests after a failed accept, such as out of files
const ACCEPT_REST: Duration = Duration::from_millis(500);

/// What bounds a connection
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// How long a connection has to send its hello
    pub hello_within: Duration,
    /// How long it may stay silent between two requests
    pub idle_within: Duration,
    /// How many connections are served at once; the next is closed
    pub most_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            hello_within: HELLO_WITHIN,
            idle_within: IDLE_WITHIN,
            most_connections: MOST_CONNECTIONS,
        }
    }
}

/// What a runner's loop shares with its socket
#[derive(Debug, Clone)]
pub struct Handles {
    /// The runner, whose lock a request takes unless the posted copy answers it
    pub runner: Arc<Mutex<Runner>>,
    /// What the loop last posted
    pub postings: Arc<Posted>,
    /// Wakes the loop, as a trigger that asks for a pass does
    pub wake: Sender<()>,
}

/// A runner's socket while it serves, ended by [`Serving::end`]
#[derive(Debug)]
pub struct Serving {
    socket: PathBuf,
    ending: Arc<AtomicBool>,
}

impl Serving {
    /// Stops accepting and removes the socket, best effort
    ///
    /// A connection in flight finishes its request, and the process ending
    /// takes the rest.
    pub fn end(self) {
        self.ending.store(true, Ordering::SeqCst);
        // The accept loop is blocked in `accept`: a connection of our own wakes it.
        let _ = Stream::connect(&self.socket);
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Opens `socket` for the maintainer's user alone and serves it, or says why
/// the runner goes on without
///
/// A path too long to bind, a live runner's socket and a folder that cannot be
/// made are each one line to stderr. The runner starts either way: nothing it
/// does needs the socket, and the verbs fall back to `shep trigger`.
pub fn start(socket: &Path, handles: Handles) -> Option<Serving> {
    match serve(socket, handles, Limits::default()) {
        Ok(serving) => Some(serving),
        Err(why) => {
            eprintln!(
                "the runner has no socket of its own, so its verbs use `shep trigger`: {why}"
            );
            None
        }
    }
}

/// Opens `socket` and serves it on threads of its own
///
/// # Errors
///
/// A message when the path is too long, taken by a live runner, or cannot be
/// made owner only or bound, or when the accept thread cannot start.
pub fn serve(socket: &Path, handles: Handles, limits: Limits) -> Result<Serving, String> {
    let listener = open(socket)?;
    let ending = Arc::new(AtomicBool::new(false));
    let accepting = Accepting {
        handles,
        limits,
        ending: Arc::clone(&ending),
        open: Arc::new(AtomicUsize::new(0)),
    };
    let spawned = std::thread::Builder::new()
        .name("socket".into())
        .spawn(move || accepting.run(&listener));
    if let Err(e) = spawned {
        let _ = std::fs::remove_file(socket);
        return Err(format!("cannot start the thread that serves it: {e}"));
    }
    Ok(Serving {
        socket: socket.to_owned(),
        ending,
    })
}

/// Binds `socket`, in a folder only its owner can use
///
/// # Errors
///
/// As [`ipc::open_owner_only`](crate::ipc::open_owner_only), and a message when
/// its folder cannot be made or narrowed.
pub fn open(socket: &Path) -> Result<Listener, String> {
    crate::home::socket_fits(socket)?;
    if let Some(folder) = socket.parent() {
        let narrowed = crate::owner_only::private_dir(folder)
            .and_then(|()| crate::owner_only::restrict_dir(folder));
        narrowed.map_err(|e| format!("cannot make {} owner only: {e}", folder.display()))?;
    }
    crate::ipc::open_owner_only(socket, "runner", |at| Listener::bind(at))
}

// The accept loop's state: it hands each connection a thread.
struct Accepting {
    handles: Handles,
    limits: Limits,
    ending: Arc<AtomicBool>,
    open: Arc<AtomicUsize>,
}

impl Accepting {
    fn run(self, listener: &Listener) {
        for stream in listener.incoming() {
            if self.ending.load(Ordering::SeqCst) {
                return;
            }
            let stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    eprintln!("the runner's socket could not take a connection: {e}");
                    std::thread::sleep(ACCEPT_REST);
                    continue;
                }
            };
            if self.open.fetch_add(1, Ordering::SeqCst) >= self.limits.most_connections {
                self.open.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let (handles, limits) = (self.handles.clone(), self.limits);
            let open = Arc::clone(&self.open);
            let visit = std::thread::Builder::new()
                .name("socket-visit".into())
                .spawn(move || {
                    visit(&stream, &handles, limits);
                    open.fetch_sub(1, Ordering::SeqCst);
                });
            if visit.is_err() {
                self.open.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

// One connection, until it ends or breaks a bound.
fn visit(stream: &Stream, handles: &Handles, limits: Limits) {
    if stream.set_write_timeout(Some(WRITE_WITHIN)).is_err()
        || stream.set_read_timeout(Some(limits.hello_within)).is_err()
    {
        return;
    }
    let mut reader = BufReader::new(stream);
    let mut write = stream;
    let Some(hello) = read_line(&mut reader) else {
        return;
    };
    if serde_json::from_str::<Hello>(&hello).is_err()
        || say(&mut write, &Hello { protocol: PROTOCOL }).is_err()
        || stream.set_read_timeout(Some(limits.idle_within)).is_err()
    {
        return;
    }
    while let Some(line) = read_line(&mut reader) {
        let said = match answered(handles, &line) {
            Answer::Reply(reply) => say(&mut write, &reply),
            Answer::Unknown(unknown) => say(&mut write, &unknown),
            Answer::Close => return,
        };
        if said.is_err() {
            return;
        }
    }
}

// What a request line is answered with.
enum Answer {
    Reply(Reply),
    Unknown(UnknownRequest),
    Close,
}

fn answered(handles: &Handles, line: &str) -> Answer {
    let request = match shep_kelpie_protocol::frame::decode(line) {
        Ok(request) => request,
        Err(FrameError::Unknown { request }) => {
            return Answer::Unknown(UnknownRequest {
                request,
                version: Build::this().kelpie,
            });
        }
        Err(FrameError::TooLong { .. }) => return Answer::Close,
        Err(e @ FrameError::Unreadable(_)) => return Answer::Reply(Reply::Refused(e.to_string())),
    };
    let reply = answer_request(&handles.runner, &handles.postings, &request);
    if wakes(request.action()) || handles.postings.needs_wake() {
        let _ = handles.wake.send(());
    }
    Answer::Reply(reply)
}

// The next line without its end, or `None` at the end of the connection, a
// silence past the read timeout, a line past the bound, or bytes that are not text.
fn read_line(reader: &mut BufReader<&Stream>) -> Option<String> {
    let mut line = Vec::new();
    let mut bounded = reader.by_ref().take(LONGEST_FRAME as u64 + 1);
    let read = bounded.read_until(b'\n', &mut line).ok()?;
    if read == 0 || line.pop() != Some(b'\n') {
        return None;
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).ok()
}

// Writes `value` as one line.
fn say(write: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
    line.push(b'\n');
    write.write_all(&line)
}

#[cfg(test)]
mod tests;
