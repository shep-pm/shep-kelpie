//! The outside half of [`crate::bridge`]: one socket per MCP server, for one call
//!
//! Each server named in the call's MCP config gets a socket with a name no
//! one can guess, in a folder the sandbox can neither read nor write, and the
//! sandbox may connect to that path alone. Connections are served one after
//! another, each by a fresh server that stops when it ends. Dropping the
//! [`Bridges`] stops every server and removes every socket.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Deserialize;
use serde_json::{Value, json};

use super::process::stop_group;
use crate::bridge::{Filter, Passage};

// macOS refuses a socket path of 104 bytes or more.
pub(super) const LONGEST_SOCKET: usize = 103;

/// The sockets one call's MCP servers are reached through, open until dropped
#[derive(Debug)]
pub(crate) struct Bridges {
    /// The MCP config that reaches each server through its socket
    pub(crate) config: Value,
    /// Every socket, which the sandbox lets the call connect to
    pub(crate) sockets: Vec<PathBuf>,
    listeners: Vec<Listener>,
}

// The connection being served, and its server's process group
type Serving = Arc<Mutex<Option<(UnixStream, u32)>>>;

#[derive(Debug)]
struct Listener {
    socket: PathBuf,
    stopping: Arc<AtomicBool>,
    serving: Serving,
    thread: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Deserialize)]
struct Server {
    command: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

impl Bridges {
    /// Opens a socket in `folder` for each server in `config`, served from `cwd`
    ///
    /// The config's servers become `kelpie mcp-connect <socket>`, run by `kelpie`.
    ///
    /// # Errors
    ///
    /// The reason, when a server is not a command or a socket cannot be opened.
    pub(crate) fn open(
        config: &Value,
        folder: &Path,
        kelpie: &Path,
        cwd: &Path,
        filter: &Filter,
    ) -> Result<Self, String> {
        let servers: BTreeMap<String, Server> =
            serde_json::from_value(config["mcpServers"].clone())
                .map_err(|e| format!("kelpie runs only command MCP servers: {e}"))?;
        let mut bridges = Self {
            config: json!({ "mcpServers": {} }),
            sockets: Vec::new(),
            listeners: Vec::new(),
        };
        for (name, server) in servers {
            let socket = folder.join(format!("{}.sock", unguessable()?));
            if socket.as_os_str().len() > LONGEST_SOCKET {
                return Err(format!(
                    "{} is too long a path for a socket",
                    socket.display()
                ));
            }
            let listener = UnixListener::bind(&socket)
                .map_err(|e| format!("cannot open {}: {e}", socket.display()))?;
            bridges.config["mcpServers"][&name] = json!({
                "command": kelpie,
                "args": ["mcp-connect", socket],
            });
            bridges.sockets.push(socket.clone());
            bridges
                .listeners
                .push(Listener::start(listener, socket, server, cwd, filter));
        }
        Ok(bridges)
    }
}

impl Drop for Bridges {
    fn drop(&mut self) {
        for listener in &mut self.listeners {
            listener.stop();
        }
    }
}

impl Listener {
    fn start(
        listener: UnixListener,
        socket: PathBuf,
        server: Server,
        cwd: &Path,
        filter: &Filter,
    ) -> Self {
        let stopping = Arc::new(AtomicBool::new(false));
        let serving: Serving = Arc::new(Mutex::new(None));
        let (stop, held, cwd, filter) = (
            Arc::clone(&stopping),
            Arc::clone(&serving),
            cwd.to_owned(),
            filter.clone(),
        );
        let thread = thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    serve(stream, &server, &cwd, &filter, &held, &stop);
                }
            }
        });
        Self {
            socket,
            stopping,
            serving,
            thread: Some(thread),
        }
    }

    fn stop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // Wakes the accept, which then sees it is stopping.
        let _ = UnixStream::connect(&self.socket);
        let _ = std::fs::remove_file(&self.socket);
        if let Some((stream, pgid)) = self.serving.lock().unwrap().take() {
            let _ = stream.shutdown(Shutdown::Both);
            stop_group(pgid);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// Serves one connection with a fresh server, filtering what goes in, until
// either end closes. The server runs in its own process group, stopped whole.
fn serve(
    stream: UnixStream,
    server: &Server,
    cwd: &Path,
    filter: &Filter,
    serving: &Serving,
    stopping: &AtomicBool,
) {
    let spawned = Command::new(&server.command)
        .args(&server.args)
        .envs(&server.env)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    let Ok(mut child) = spawned else { return };
    let pgid = child.id();
    let (Ok(back), Ok(held)) = (stream.try_clone(), stream.try_clone()) else {
        stop_group(pgid);
        let _ = child.wait();
        return;
    };
    *serving.lock().unwrap() = Some((held, pgid));
    // Stopped between the accept and here: nothing else would end this one.
    if stopping.load(Ordering::SeqCst) {
        let _ = stream.shutdown(Shutdown::Both);
    }
    let back = Arc::new(Mutex::new(back));
    let replies = Arc::clone(&back);
    let from_server = child.stdout.take();
    let out = thread::spawn(move || {
        let Some(mut from_server) = from_server else {
            return;
        };
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = from_server.read(&mut buf) {
            if n == 0 || replies.lock().unwrap().write_all(&buf[..n]).is_err() {
                break;
            }
        }
    });
    if let Some(mut to_server) = child.stdin.take() {
        let mut lines = BufReader::new(stream);
        let mut line = String::new();
        while lines.read_line(&mut line).is_ok_and(|n| n > 0) {
            let sent = match filter.judge(line.trim_end()) {
                Passage::Pass => to_server
                    .write_all(line.as_bytes())
                    .and_then(|()| to_server.flush()),
                Passage::Stop(Some(reply)) => writeln!(back.lock().unwrap(), "{reply}"),
                Passage::Stop(None) => Ok(()),
            };
            if sent.is_err() {
                break;
            }
            line.clear();
        }
    }
    stop_group(pgid);
    let _ = child.wait();
    let _ = back.lock().unwrap().shutdown(Shutdown::Both);
    let _ = out.join();
    *serving.lock().unwrap() = None;
}

// 128 random bits as hex, from the system's own source.
pub(super) fn unguessable() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("cannot draw a socket name: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    // A stand-in server: writes its pid, logs each line it gets, and echoes it.
    // In `/tmp`, as macOS's own temporary folder is too long for a socket.
    fn world() -> (tempfile::TempDir, Value) {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let server = root.join("server");
        let script = format!(
            "#!/bin/sh\necho $$ > {root}/pid\nwhile read -r line; do\n  echo \"$line\" >> {root}/got\n  echo \"$line\"\ndone\n",
            root = root.display()
        );
        crate::test::write_script(&server, &script);
        let config = json!({ "mcpServers": { "playwright": { "command": server, "args": [] } } });
        (dir, config)
    }

    fn open(dir: &tempfile::TempDir, config: &Value) -> Bridges {
        let filter = Filter {
            domains: Vec::new(),
            denied_tools: vec!["browser_run_code_unsafe".into()],
        };
        let root = dir.path().canonicalize().unwrap();
        Bridges::open(config, &root, Path::new("/k/kelpie"), &root, &filter).unwrap()
    }

    fn ask(stream: &mut BufReader<UnixStream>, line: &str) -> Value {
        writeln!(stream.get_mut(), "{line}").unwrap();
        let mut answer = String::new();
        stream.read_line(&mut answer).unwrap();
        serde_json::from_str(&answer).unwrap()
    }

    fn wait_for(what: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if what() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn the_config_reaches_each_server_through_kelpie_and_a_socket() {
        let (dir, config) = world();
        let bridges = open(&dir, &config);
        let [socket] = bridges.sockets.as_slice() else {
            panic!("{:?}", bridges.sockets);
        };
        assert_eq!(
            bridges.config,
            json!({ "mcpServers": { "playwright": {
                "command": "/k/kelpie",
                "args": ["mcp-connect", socket],
            } } })
        );
        let name = socket.file_name().unwrap().to_string_lossy();
        assert_eq!(name.len(), 32 + ".sock".len(), "{name}");
    }

    #[test]
    fn what_passes_reaches_the_server_and_what_is_refused_never_does() {
        let (dir, config) = world();
        let bridges = open(&dir, &config);
        let mut stream = BufReader::new(UnixStream::connect(&bridges.sockets[0]).unwrap());
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        assert_eq!(ask(&mut stream, list)["id"], 1);
        let escape = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"browser_navigate","arguments":{"url":"view-source:file:///etc/hosts"}}}"#;
        let refused = ask(&mut stream, escape);
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        let read = r#"{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"file:///etc/hosts"}}"#;
        assert_eq!(ask(&mut stream, read)["error"]["code"], -32601);
        let got = std::fs::read_to_string(dir.path().join("got")).unwrap();
        assert_eq!(got.trim(), list);
    }

    #[test]
    fn dropping_the_bridges_stops_the_server_and_removes_the_socket() {
        let (dir, config) = world();
        let bridges = open(&dir, &config);
        let socket = bridges.sockets[0].clone();
        let mut stream = BufReader::new(UnixStream::connect(&socket).unwrap());
        ask(&mut stream, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        let pid = std::fs::read_to_string(dir.path().join("pid")).unwrap();
        // The client is still connected, as a lingering `mcp-connect` would be.
        drop(bridges);
        assert!(!socket.exists());
        let alive = || {
            Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        assert!(wait_for(|| !alive()), "the server outlived its bridge");
        assert!(UnixStream::connect(&socket).is_err());
    }

    #[test]
    fn a_second_connection_waits_for_the_first_to_end() {
        let (dir, config) = world();
        let bridges = open(&dir, &config);
        let mut first = BufReader::new(UnixStream::connect(&bridges.sockets[0]).unwrap());
        ask(&mut first, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        let mut second = UnixStream::connect(&bridges.sockets[0]).unwrap();
        second
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        writeln!(second, r#"{{"jsonrpc":"2.0","id":2,"method":"ping"}}"#).unwrap();
        let mut buf = [0u8; 16];
        assert!(second.read(&mut buf).is_err(), "served beside the first");
        drop(first);
        second
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut answer = String::new();
        BufReader::new(second).read_line(&mut answer).unwrap();
        assert!(answer.contains("\"id\":2"), "{answer}");
    }
}
