//! The outside half of [`crate::forwarder`]: one socket, for one call
//!
//! The sandbox's proxy hands the worker's model calls to this socket, in a
//! folder the sandbox cannot read, and the sandbox itself may connect to no
//! socket of it. Each connection carries one request, tunnelled or not. A chat call is sent on
//! to the model server with `Connection: close` and its reply is copied back
//! as it arrives. Anything else is refused by name, and no byte of it is sent
//! on, so a second request cannot ride behind the first. Dropping the
//! [`Forwarder`] stops every connection and removes the socket.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::json;

use super::bridge::{LONGEST_SOCKET, unguessable};
use crate::confine::Verdict;
use crate::forwarder::Upstream;

#[cfg(test)]
mod tests;

/// The most a request's head may take, in bytes
const HEAD_MAX: usize = 64 * 1024;

/// The most a request's body may take, in bytes: a chat call carries its whole context
const BODY_MAX: u64 = 64 << 20;

/// Seconds the forwarder gives each address of the model server to connect
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The headers of a chat call that go on to the model server
const PASSED: [&str; 4] = ["content-type", "accept", "authorization", "user-agent"];

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
        let socket = folder.join(format!("{}.sock", unguessable()?));
        if socket.as_os_str().len() > LONGEST_SOCKET {
            return Err(format!(
                "{} is too long a path for a socket",
                socket.display()
            ));
        }
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
        let listener = UnixListener::bind(&socket)
            .map_err(|e| format!("cannot open {}: {e}", socket.display()))?;
        let stopping = Arc::new(AtomicBool::new(false));
        let open: Open = Arc::default();
        let (stop, held) = (Arc::clone(&stopping), Arc::clone(&open));
        let thread = thread::spawn(move || accept(&listener, &upstream, &stop, &held));
        Ok(Self {
            socket,
            stopping,
            open,
            thread: Some(thread),
        })
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

fn accept(listener: &UnixListener, upstream: &Upstream, stopping: &AtomicBool, open: &Open) {
    let ids = AtomicU64::new(0);
    let mut serving = Vec::new();
    for stream in listener.incoming() {
        if stopping.load(Ordering::SeqCst) {
            break;
        }
        let Ok(client) = stream else { continue };
        let Ok(held) = client.try_clone() else {
            continue;
        };
        let id = ids.fetch_add(1, Ordering::Relaxed);
        open.lock().unwrap().insert(
            id,
            Ends {
                client: held,
                server: None,
            },
        );
        let (upstream, open) = (upstream.clone(), Arc::clone(open));
        let stop = stopping.load(Ordering::SeqCst);
        serving.push(thread::spawn(move || {
            if !stop {
                serve(client, &upstream, &open, id);
            }
            open.lock().unwrap().remove(&id);
        }));
        serving.retain(|t| !t.is_finished());
    }
    // A connection accepted as the stop came is ended here, once it is listed.
    end_all(open);
    for thread in serving {
        let _ = thread.join();
    }
}

/// A refusal or failure, as the reply the worker's harness reads
struct Reply {
    status: u16,
    text: String,
}

impl Reply {
    fn new(status: u16, text: impl Into<String>) -> Self {
        Self {
            status,
            text: text.into(),
        }
    }

    fn write(&self, to: &mut impl Write) -> io::Result<()> {
        let reason = match self.status {
            400 => "Bad Request",
            403 => "Forbidden",
            413 => "Content Too Large",
            501 => "Not Implemented",
            _ => "Bad Gateway",
        };
        let body = json!({ "error": { "message": self.text, "type": "kelpie_refused" } });
        let body = body.to_string();
        write!(
            to,
            "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.status,
            body.len()
        )?;
        to.flush()
    }
}

fn serve(mut client: UnixStream, upstream: &Upstream, open: &Open, id: u64) {
    if let Err(reply) = exchange(&mut client, upstream, open, id) {
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
    open: &Open,
    id: u64,
) -> Result<(), Reply> {
    let gone = |_: io::Error| Reply::new(400, "kelpie could not read the request");
    let mut reader = BufReader::new(client.try_clone().map_err(gone)?);
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
    if let Some(ends) = open.lock().unwrap().get_mut(&id) {
        ends.server = server.try_clone().ok();
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
fn request(upstream: &Upstream, head: &Head, length: usize) -> Vec<u8> {
    let mut text = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {length}\r\n",
        upstream.chat_path(),
        upstream.address(),
    );
    for (name, value) in &head.headers {
        if PASSED.contains(&name.as_str()) {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    text.push_str("\r\n");
    text.into_bytes()
}

/// A request's line and headers, names in lower case
#[derive(Debug)]
struct Head {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn read(reader: &mut impl BufRead) -> Result<Self, Reply> {
        let bad = |why: &str| Reply::new(400, format!("kelpie cannot read the request: {why}"));
        let mut taken = 0;
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .map_err(|_| bad("it is not text"))?;
            taken += n;
            if n == 0 {
                return Err(bad("it ended early"));
            }
            if taken > HEAD_MAX {
                return Err(bad("its head is too long"));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() && !lines.is_empty() {
                break;
            }
            if !line.is_empty() {
                lines.push(line.to_owned());
            }
        }
        let mut first = lines[0].split(' ');
        let (Some(method), Some(target), Some(version), None) =
            (first.next(), first.next(), first.next(), first.next())
        else {
            return Err(bad("its first line is not a request line"));
        };
        if !version.starts_with("HTTP/1.") {
            return Err(bad("it is not HTTP/1"));
        }
        let headers = lines[1..]
            .iter()
            .map(|line| {
                let (name, value) = line
                    .split_once(':')
                    .ok_or_else(|| bad("a header has no name"))?;
                Ok((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            })
            .collect::<Result<_, Reply>>()?;
        Ok(Self {
            method: method.to_owned(),
            target: target.to_owned(),
            headers,
        })
    }

    fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.headers
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn expects_continue(&self) -> bool {
        self.values("expect")
            .any(|v| v.eq_ignore_ascii_case("100-continue"))
    }

    // The body, whole: counted by its length or decoded from its chunks. A
    // request that says both is the shape of a smuggled one and is refused.
    fn body(&self, reader: &mut impl BufRead) -> Result<Vec<u8>, Reply> {
        let bad = |why: &str| Reply::new(400, format!("kelpie cannot read the request: {why}"));
        let short = |_: io::Error| bad("its body ended early");
        let encodings: Vec<_> = self.values("transfer-encoding").collect();
        let lengths: Vec<_> = self.values("content-length").collect();
        match (encodings.as_slice(), lengths.as_slice()) {
            ([], []) => Ok(Vec::new()),
            ([encoding], []) if encoding.eq_ignore_ascii_case("chunked") => chunked(reader)
                .map_err(|e| match e.kind() {
                    io::ErrorKind::FileTooLarge => Reply::new(413, "the request body is too large"),
                    _ => bad("its chunks are not well formed"),
                }),
            ([_, ..], _) if !lengths.is_empty() => Err(bad("it gives a length and an encoding")),
            ([_, ..], _) => Err(Reply::new(501, "kelpie reads only chunked bodies")),
            ([], [first, rest @ ..]) => {
                let length: u64 = first
                    .parse()
                    .map_err(|_| bad("its length is not a number"))?;
                if rest.iter().any(|other| other != first) {
                    return Err(bad("it gives two lengths"));
                }
                if length > BODY_MAX {
                    return Err(Reply::new(413, "the request body is too large"));
                }
                let mut body = Vec::new();
                reader.take(length).read_to_end(&mut body).map_err(short)?;
                if body.len() as u64 == length {
                    Ok(body)
                } else {
                    Err(bad("its body ended early"))
                }
            }
        }
    }
}

// Decodes a chunked body, its trailers read and dropped.
fn chunked(reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidData);
    let mut body = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let size = line.split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size, 16).map_err(|_| invalid())?;
        if size == 0 {
            break;
        }
        if body.len() as u64 + size > BODY_MAX {
            return Err(io::Error::from(io::ErrorKind::FileTooLarge));
        }
        let before = body.len();
        reader.by_ref().take(size).read_to_end(&mut body)?;
        let mut end = [0u8; 2];
        reader.read_exact(&mut end)?;
        if (body.len() - before) as u64 != size || &end != b"\r\n" {
            return Err(invalid());
        }
    }
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
            return Ok(body);
        }
    }
}
