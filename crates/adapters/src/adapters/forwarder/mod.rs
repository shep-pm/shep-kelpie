//! The outside half of [`crate::forwarder`]: one socket, for one call
//!
//! The sandbox's proxy hands the worker's model calls to this socket, and only
//! the proxy may connect to it. Each connection carries one request, or one
//! tunnel and the one request inside it. A chat call is sent on to the model
//! server with `Connection: close` and its reply is copied back as it arrives.
//! Anything else is refused by name and no byte of it is sent on, so a second
//! request cannot ride behind the first. Every read is bounded in bytes and
//! time and connections are capped. Dropping the [`Forwarder`] stops every
//! connection and removes the socket.

use std::collections::HashMap;
use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::confine::Verdict;
use crate::forwarder::Upstream;
use crate::ipc::blocking::{Listener, Stream};
use crate::random;
use http::{Head, Reply, Timed};

pub(in crate::adapters) mod http;
#[cfg(test)]
mod tests;

/// Seconds the forwarder gives each address of the model server to connect
pub(in crate::adapters) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a client may take to send its whole request
pub(in crate::adapters) const REQUEST_WAIT: Duration = Duration::from_secs(30);

/// How long a client may leave a reply unread before the forwarder drops it
pub(in crate::adapters) const REPLY_STALL: Duration = Duration::from_secs(60);

/// How often the forwarder looks to see whether the reply it is relaying is done
const WATCH_EVERY: Duration = Duration::from_millis(100);

/// How many connections are served at once. pi's calls come one at a time.
const MAX_CONNECTIONS: usize = 8;

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

// The connections being served, across every forwarder
static RELAYING: AtomicUsize = AtomicUsize::new(0);

/// Whether a forwarder is serving a connection, which a handover waits out
///
/// A reply being relayed is copied by a thread that does not cross an
/// `execve`, so the model would be cut off mid-stream.
pub(crate) fn relaying() -> bool {
    RELAYING.load(Ordering::SeqCst) > 0
}

// Counts one connection in `count` while it lives
struct Serving(&'static AtomicUsize);

impl Serving {
    fn begin(count: &'static AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

// Both ends of each connection being served, so a stop can end them.
type Open = Arc<Mutex<HashMap<u64, Ends>>>;

#[derive(Debug)]
struct Ends {
    client: Stream,
    server: Option<TcpStream>,
}

/// The socket one call's model traffic is served on, open until dropped
#[derive(Debug)]
pub struct Forwarder {
    /// The socket the sandbox's proxy dials for the call's model host
    pub socket: PathBuf,
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
    pub fn open(folder: &Path, upstream: Upstream) -> Result<Self, String> {
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
        if crate::ipc::too_long(&socket) {
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
        Self::bind(socket, upstream, limits, &RELAYING)
    }

    /// Opens `socket` again, the path a forwarder of an earlier image of the
    /// runner served on, which that image's `execve` left bound to nothing
    ///
    /// # Errors
    ///
    /// The reason, when `socket` is not one a forwarder opens or cannot be
    /// bound.
    pub(crate) fn reopen(socket: &Path, upstream: Upstream) -> Result<Self, String> {
        if !is_forwarders(socket) {
            return Err(format!("{} is not a forwarder's socket", socket.display()));
        }
        match std::fs::remove_file(socket) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                return Err(format!("cannot clear {}: {e}", socket.display()));
            }
            _ => {}
        }
        Self::bind(socket.to_owned(), upstream, Limits::default(), &RELAYING)
    }

    // Serves `upstream` on `socket`, counting each connection in `count`.
    fn bind(
        socket: PathBuf,
        upstream: Upstream,
        limits: Limits,
        count: &'static AtomicUsize,
    ) -> Result<Self, String> {
        let listener = Listener::bind(&socket)
            .map_err(|e| format!("cannot open {}: {e}", socket.display()))?;
        let stopping = Arc::new(AtomicBool::new(false));
        let open: Open = Arc::default();
        let (stop, held) = (Arc::clone(&stopping), Arc::clone(&open));
        let thread = thread::Builder::new()
            .name("forwarder".into())
            .spawn(move || accept(&listener, &upstream, limits, count, &stop, &held))
            .map_err(|e| format!("cannot start the forwarder: {e}"))?;
        Ok(Self {
            socket,
            stopping,
            open,
            thread: Some(thread),
        })
    }

    /// [`Forwarder::open_with`], counting its connections in `count`, which a
    /// test owns to read alone
    #[cfg(test)]
    pub(crate) fn open_counted(
        folder: &Path,
        upstream: Upstream,
        count: &'static AtomicUsize,
    ) -> Result<Self, String> {
        let socket = folder.join(format!("{}.sock", unguessable()?));
        Self::bind(socket, upstream, Limits::default(), count)
    }
}

/// How old a socket must be before a sweep may remove it
const SWEEP_AGE: Duration = Duration::from_secs(60);

// When this process first opened a forwarder: a socket older than that is an
// earlier kelpie's. One this process made may be bound and not yet listening.
static STARTED: LazyLock<SystemTime> = LazyLock::new(SystemTime::now);

// Whether `path` is named as a forwarder names its socket.
fn is_forwarders(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".sock"))
        .is_some_and(|n| n.len() == 32 && n.bytes().all(|b| b.is_ascii_hexdigit()))
}

// Removes the sockets an earlier kelpie left in `folder` when it was killed
// before it could, those last changed before `before` that nothing answers on.
fn sweep(folder: &Path, before: SystemTime) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for path in entries.filter_map(Result::ok).map(|e| e.path()) {
        let ours = is_forwarders(&path);
        let old = path
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|changed| changed < before);
        if ours
            && old
            && Stream::connect(&path).is_err_and(|e| e.kind() == io::ErrorKind::ConnectionRefused)
        {
            let _ = std::fs::remove_file(&path);
        }
    }
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // Wakes the accept, which then sees it is stopping.
        let _ = Stream::connect(&self.socket);
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
    listener: &Listener,
    upstream: &Upstream,
    limits: Limits,
    count: &'static AtomicUsize,
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
        let serving_now = Serving::begin(count);
        let spawned = thread::Builder::new().name("forwarding".into()).spawn({
            let open = Arc::clone(&open);
            move || {
                let _serving = serving_now;
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
    mut client: Stream,
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
    client: &mut Stream,
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
    let sent = request(upstream, &head, body.len())?;
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
        .write_all(&sent)
        .and_then(|()| server.write_all(&body))
        .map_err(unsent)?;
    let (watched, upstream_end) = (client.try_clone(), server.try_clone());
    let (Ok(watched), Ok(upstream_end)) = (watched, upstream_end) else {
        return Err(Reply::new(502, "kelpie could not watch the client"));
    };
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        // The watcher is an extra: a reply is relayed whole without one.
        let _watcher = thread::Builder::new()
            .name("watching".into())
            .spawn_scoped(scope, || hang_up_ends(&watched, &upstream_end, &done));
        let copied = io::copy(&mut server, client);
        done.store(true, Ordering::SeqCst);
        copied
    })
    .map_err(unsent)?;
    Ok(())
}

// Reads what the client sends after its request, which a chat call has no use
// for, until it hangs up or the reply is done. A hang-up shuts the model
// server's stream, as the copy would not notice it until the next reply byte.
fn hang_up_ends(mut client: &Stream, server: &TcpStream, done: &AtomicBool) {
    let mut unused = [0u8; 512];
    if client.set_read_timeout(Some(WATCH_EVERY)).is_err() {
        return;
    }
    while !done.load(Ordering::SeqCst) {
        match client.read(&mut unused) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => break,
        }
    }
    if !done.load(Ordering::SeqCst) {
        let _ = server.shutdown(Shutdown::Both);
    }
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

// The chat call as the model server gets it: only a chat client's headers,
// with a length the forwarder counted. A gateway gets its own key, never the
// worker's, and the batch turn on its upstream's lease, if any. A key holding
// a line break is refused, as it would start a header of the worker's choosing.
fn request(upstream: &Upstream, head: &Head, length: usize) -> Result<Vec<u8>, Reply> {
    if let Some(key) = upstream.key()
        && key.expose().contains(['\r', '\n'])
    {
        return Err(Reply::new(
            502,
            "kelpie will not send the gateway's key, which holds a line break",
        ));
    }
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
        text.push_str("x-paddock-priority: batch\r\n");
    }
    if let Some(lease) = upstream.lease() {
        text.push_str(&format!("x-paddock-lease: {lease}\r\n"));
    }
    text.push_str("\r\n");
    Ok(text.into_bytes())
}

pub(super) fn unguessable() -> Result<String, String> {
    random::hex128().map_err(|e| format!("cannot draw a socket name: {e}"))
}
