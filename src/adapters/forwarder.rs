//! The outside half of [`crate::forwarder`]: one socket, for one call
//!
//! The sandbox's proxy hands the worker's model calls to this socket. The
//! sandbox may not connect to it, only the proxy does. Each connection carries
//! one request, or one tunnel and the one request inside it. A chat call is
//! sent on to the model server with `Connection: close` and its reply is
//! copied back as it arrives. Anything else is refused by name, and no byte
//! of it is sent on, so a second request cannot ride behind the first.
//! Anything inside the sandbox can reach the socket through the proxy, so
//! every read is bounded in bytes and in time and the connections are capped.
//! Dropping the [`Forwarder`] stops every connection and removes the socket.

use std::collections::HashMap;
use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::confine::Verdict;
use crate::forwarder::Upstream;
use http::{Head, Reply, Timed};

mod http;
#[cfg(test)]
mod tests;

/// Seconds the forwarder gives each address of the model server to connect
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a client may take to send its whole request
const REQUEST_WAIT: Duration = Duration::from_secs(30);

/// How long a client may leave a reply unread before the forwarder drops it
const REPLY_STALL: Duration = Duration::from_secs(60);

/// How many connections are served at once. pi's calls come one at a time.
const MAX_CONNECTIONS: usize = 8;

// macOS refuses a socket path of 104 bytes or more.
const LONGEST_SOCKET: usize = 103;

/// The headers of a chat call that go on to the model server
const PASSED: [&str; 4] = ["content-type", "accept", "authorization", "user-agent"];

/// What a forwarder holds a client to
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// The connections served at once, the rest being refused
    pub(crate) connections: usize,
    /// How long a client may take to send its whole request
    pub(crate) wait: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: MAX_CONNECTIONS,
            wait: REQUEST_WAIT,
        }
    }
}

// Both ends of each connection being served, so a stop can end them.
type Open = Arc<Mutex<HashMap<u64, Ends>>>;

#[derive(Debug)]
struct Ends {
    client: UnixStream,
    server: Option<TcpStream>,
}

/// The socket one call's model traffic is served on, open until dropped
#[derive(Debug)]
pub(crate) struct Forwarder {
    /// The socket the sandbox's proxy dials for the call's model host
    pub(crate) socket: PathBuf,
    stopping: Arc<AtomicBool>,
    open: Open,
    thread: Option<thread::JoinHandle<()>>,
}

impl Forwarder {
    /// Opens a socket in `folder` that forwards chat calls to `upstream`
    ///
    /// # Errors
    ///
    /// The reason, when the socket cannot be opened.
    pub(crate) fn open(folder: &Path, upstream: Upstream) -> Result<Self, String> {
        Self::open_with(folder, upstream, Limits::default())
    }

    /// [`Forwarder::open`], holding clients to `limits`
    ///
    /// # Errors
    ///
    /// The reason, when the socket cannot be opened.
    pub(crate) fn open_with(
        folder: &Path,
        upstream: Upstream,
        limits: Limits,
    ) -> Result<Self, String> {
        let socket = folder.join(format!("{}.sock", unguessable()?));
        if socket.as_os_str().len() > LONGEST_SOCKET {
            return Err(format!(
                "{} is too long a path for a socket",
                socket.display()
            ));
        }
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
        let settled = SystemTime::now().checked_sub(SWEEP_AGE);
        sweep(
            folder,
            (*STARTED).min(settled.unwrap_or(SystemTime::UNIX_EPOCH)),
        );
        let listener = UnixListener::bind(&socket)
            .map_err(|e| format!("cannot open {}: {e}", socket.display()))?;
        let stopping = Arc::new(AtomicBool::new(false));
        let open: Open = Arc::default();
        let (stop, held) = (Arc::clone(&stopping), Arc::clone(&open));
        let thread = thread::Builder::new()
            .name("forwarder".into())
            .spawn(move || accept(&listener, &upstream, limits, &stop, &held))
            .map_err(|e| format!("cannot start the forwarder: {e}"))?;
        Ok(Self {
            socket,
            stopping,
            open,
            thread: Some(thread),
        })
    }
}

/// How old a socket must be before a sweep may remove it
const SWEEP_AGE: Duration = Duration::from_secs(60);

// When this process first opened a forwarder: a socket older than that is an
// earlier kelpie's. One this process made may be bound and not yet listening.
static STARTED: LazyLock<SystemTime> = LazyLock::new(SystemTime::now);

// Removes the sockets an earlier kelpie left in `folder` when it was killed
// before it could, those last changed before `before` that nothing answers on.
fn sweep(folder: &Path, before: SystemTime) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for path in entries.filter_map(Result::ok).map(|e| e.path()) {
        let ours = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".sock"))
            .is_some_and(|n| n.len() == 32 && n.bytes().all(|b| b.is_ascii_hexdigit()));
        let old = path
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|changed| changed < before);
        if ours
            && old
            && UnixStream::connect(&path)
                .is_err_and(|e| e.kind() == io::ErrorKind::ConnectionRefused)
        {
            let _ = std::fs::remove_file(&path);
        }
    }
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // Wakes the accept, which then sees it is stopping.
        let _ = UnixStream::connect(&self.socket);
        let _ = std::fs::remove_file(&self.socket);
        end_all(&self.open);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn end_all(open: &Open) {
    for ends in open.lock().unwrap().values() {
        let _ = ends.client.shutdown(Shutdown::Both);
        if let Some(server) = &ends.server {
            let _ = server.shutdown(Shutdown::Both);
        }
    }
}

fn accept(
    listener: &UnixListener,
    upstream: &Upstream,
    limits: Limits,
    stopping: &Arc<AtomicBool>,
    open: &Open,
) {
    let ids = AtomicU64::new(0);
    let mut serving = Vec::new();
    for stream in listener.incoming() {
        if stopping.load(Ordering::SeqCst) {
            break;
        }
        let Ok(mut client) = stream else { continue };
        let Ok(held) = client.try_clone() else {
            continue;
        };
        let _ = client.set_write_timeout(Some(REPLY_STALL));
        let id = ids.fetch_add(1, Ordering::Relaxed);
        {
            let mut listed = open.lock().unwrap();
            if listed.len() >= limits.connections {
                drop(listed);
                let busy = Reply::new(503, "kelpie serves only a few model calls at once");
                let _ = busy.write(&mut client);
                continue;
            }
            listed.insert(
                id,
                Ends {
                    client: held,
                    server: None,
                },
            );
        }
        let (upstream, open, stopping) = (upstream.clone(), Arc::clone(open), Arc::clone(stopping));
        let spawned = thread::Builder::new().name("forwarding".into()).spawn({
            let open = Arc::clone(&open);
            move || {
                serve(client, &upstream, limits, &stopping, &open, id);
                open.lock().unwrap().remove(&id);
            }
        });
        match spawned {
            Ok(handle) => serving.push(handle),
            // The client is dropped with the closure, which closes its connection.
            Err(_) => {
                open.lock().unwrap().remove(&id);
            }
        }
        serving.retain(|t| !t.is_finished());
    }
    // A connection listed as the stop came is ended here.
    end_all(open);
    for thread in serving {
        let _ = thread.join();
    }
}

fn serve(
    mut client: UnixStream,
    upstream: &Upstream,
    limits: Limits,
    stopping: &AtomicBool,
    open: &Open,
    id: u64,
) {
    if let Err(reply) = exchange(&mut client, upstream, limits, stopping, open, id) {
        let _ = reply.write(&mut client);
    }
    let _ = client.shutdown(Shutdown::Both);
}

// Judges the request, then sends it on and copies the reply back. A client
// that tunnels first, as pi's does, sends its one request through the tunnel.
// An `Err` is what the worker is told, and nothing was sent to the model
// server for it.
fn exchange(
    client: &mut UnixStream,
    upstream: &Upstream,
    limits: Limits,
    stopping: &AtomicBool,
    open: &Open,
    id: u64,
) -> Result<(), Reply> {
    let gone = |_: io::Error| Reply::new(400, "kelpie could not read the request");
    let timed = Timed {
        stream: client.try_clone().map_err(gone)?,
        deadline: Instant::now() + limits.wait,
    };
    let mut reader = BufReader::new(timed);
    let mut head = Head::read(&mut reader)?;
    if head.method == "CONNECT" {
        if let Verdict::Refuse(why) = upstream.judge_tunnel(&head.target) {
            return Err(Reply::new(403, why));
        }
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .map_err(gone)?;
        head = Head::read(&mut reader)?;
    }
    if let Verdict::Refuse(why) = upstream.judge(&head.method, &head.target) {
        return Err(Reply::new(403, why));
    }
    if head.expects_continue() {
        client
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .map_err(gone)?;
    }
    let body = head.body(&mut reader)?;
    let mut server = connect(upstream).map_err(|e| {
        Reply::new(
            502,
            format!("kelpie could not reach the model server: {}", e.kind()),
        )
    })?;
    // Listed under the lock a stop takes, so a stop either sees this stream or is seen here.
    {
        let mut listed = open.lock().unwrap();
        if stopping.load(Ordering::SeqCst) {
            return Err(Reply::new(503, "kelpie is stopping"));
        }
        if let Some(ends) = listed.get_mut(&id) {
            ends.server = server.try_clone().ok();
        }
    }
    let unsent =
        |e: io::Error| Reply::new(502, format!("kelpie lost the model server: {}", e.kind()));
    server
        .write_all(&request(upstream, &head, body.len()))
        .and_then(|()| server.write_all(&body))
        .map_err(unsent)?;
    io::copy(&mut server, client).map_err(unsent)?;
    Ok(())
}

fn connect(upstream: &Upstream) -> io::Result<TcpStream> {
    let mut last = io::Error::from(io::ErrorKind::NotFound);
    for address in upstream.address().to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

// The chat call as the model server gets it: its own path, and only the
// headers a chat client needs, with a length the forwarder counted itself.
// A gateway gets its own key, never the one the worker sent.
fn request(upstream: &Upstream, head: &Head, length: usize) -> Vec<u8> {
    let mut text = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {length}\r\n",
        upstream.chat_path(),
        upstream.address(),
    );
    for (name, value) in &head.headers {
        let replaced = upstream.key().is_some() && name == "authorization";
        if PASSED.contains(&name.as_str()) && !replaced {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    if let Some(key) = upstream.key() {
        text.push_str(&format!("authorization: Bearer {}\r\n", key.expose()));
    }
    text.push_str("\r\n");
    text.into_bytes()
}

// 128 random bits as hex, from the system's own source.
fn unguessable() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("cannot draw a socket name: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
