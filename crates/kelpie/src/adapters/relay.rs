//! The real relay: a background `claude --bg` session, reached over its
//! undocumented messaging socket
//!
//! Kelpie never holds this session open: `claude --bg` starts it and
//! returns at once, and every later call looks it up again by its fixed
//! name in `claude agents --json --all`. The registry file
//! (`~/.claude/sessions/<pid>.json`, for `messagingSocketPath`) and the
//! peer-token file (`~/.claude/sessions/<pid>.*.key`) are read fresh each
//! send, since the relay can restart between rulings.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::ports::{Relay, RelayError};
use crate::relay::{self, EFFORT, MODEL, NAME};

/// How many times a started relay is looked for before giving up, and how
/// long to wait between looks: about the 2 seconds a real session took to
/// register, with room to spare.
const APPEAR_TRIES: u32 = 20;
const APPEAR_POLL: Duration = Duration::from_millis(250);

/// The running relay session `find` located
struct Found {
    pid: u32,
}

/// A background Claude Code session reached over its messaging socket
#[derive(Debug, Clone)]
pub struct RelayCli {
    /// The maintainer's real home, where `~/.claude/sessions` and the
    /// trust prompt this folder already passed both live
    home: PathBuf,
    /// Where the relay's own settings and instructions files are written,
    /// under kelpie's home
    folder: PathBuf,
}

impl RelayCli {
    /// A relay whose settings and instructions live under `folder`
    pub fn new(home: PathBuf, folder: PathBuf) -> Self {
        Self { home, folder }
    }

    fn find(&self) -> Result<Option<Found>, RelayError> {
        let output = Command::new("claude")
            .args(["agents", "--json", "--all"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| RelayError::Unreachable(e.to_string()))?;
        let agents: Vec<Value> = serde_json::from_slice(&output.stdout)
            .map_err(|_| RelayError::Unreachable("unreadable agent list".into()))?;
        let found = agents.iter().find(|a| a["name"] == json!(NAME));
        Ok(found
            .and_then(|a| a["pid"].as_u64())
            .map(|pid| Found { pid: pid as u32 }))
    }

    fn start(&self) -> Result<(), RelayError> {
        fs::create_dir_all(&self.folder).map_err(|e| RelayError::CannotStart(e.to_string()))?;
        let settings = self.folder.join("settings.json");
        let instructions = self.folder.join("instructions.md");
        let text = serde_json::to_string_pretty(&relay::settings()).expect("settings are JSON");
        fs::write(&settings, text).map_err(|e| RelayError::CannotStart(e.to_string()))?;
        fs::write(&instructions, relay::INSTRUCTIONS)
            .map_err(|e| RelayError::CannotStart(e.to_string()))?;
        let output = Command::new("claude")
            .args(start_argv(&settings, &instructions))
            .current_dir(&self.home)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| RelayError::CannotStart(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(RelayError::CannotStart(stderr));
        }
        Ok(())
    }

    fn ensure_running(&self) -> Result<Found, RelayError> {
        if let Some(found) = self.find()? {
            return Ok(found);
        }
        self.start()?;
        for _ in 0..APPEAR_TRIES {
            thread::sleep(APPEAR_POLL);
            if let Some(found) = self.find()? {
                return Ok(found);
            }
        }
        Err(RelayError::NeverAppeared)
    }

    fn sessions(&self) -> PathBuf {
        self.home.join(".claude/sessions")
    }

    fn registry(&self, pid: u32) -> Result<Registry, RelayError> {
        read_json(&self.sessions().join(format!("{pid}.json")))
    }

    fn peer_token(&self, pid: u32) -> Result<PeerToken, RelayError> {
        let prefix = format!("{pid}.");
        let entries =
            fs::read_dir(self.sessions()).map_err(|e| RelayError::Unreachable(e.to_string()))?;
        let key = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                name.starts_with(&prefix) && name.ends_with(".key")
            })
            .ok_or_else(|| RelayError::Unreachable("no peer-token file".into()))?;
        read_json::<Key>(&key).map(|k| k.peer_token)
    }
}

impl Relay for RelayCli {
    fn send(&self, message: &str) -> Result<(), RelayError> {
        let found = self.ensure_running()?;
        let registry = self.registry(found.pid)?;
        let token = self.peer_token(found.pid)?;
        let mut stream = UnixStream::connect(&registry.messaging_socket_path)
            .map_err(|e| RelayError::Unreachable(e.to_string()))?;
        write_line(
            &mut stream,
            &json!({ "type": "auth", "token": token.expose() }),
        )?;
        write_line(
            &mut stream,
            &json!({
                "type": "user",
                "message": { "role": "user", "content": message },
            }),
        )
    }

    fn clear(&self) -> Result<(), RelayError> {
        let Some(found) = self.find()? else {
            return Ok(());
        };
        let status = Command::new("kill")
            .arg(found.pid.to_string())
            .stdin(Stdio::null())
            .status()
            .map_err(|e| RelayError::CannotStop(e.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(RelayError::CannotStop(format!("kill exited with {status}")))
        }
    }
}

// `--remote-control` and `--name` share `NAME`: the first names the remote
// control channel, the second is what `claude agents --json --all` shows,
// and both must match for the same relay to be found again.
fn start_argv(settings: &Path, instructions: &Path) -> Vec<OsString> {
    vec![
        "--bg".into(),
        "--remote-control".into(),
        NAME.into(),
        "--name".into(),
        NAME.into(),
        "--model".into(),
        MODEL.into(),
        "--effort".into(),
        EFFORT.into(),
        "--setting-sources".into(),
        "".into(),
        "--settings".into(),
        settings.to_owned().into(),
        "--append-system-prompt-file".into(),
        instructions.to_owned().into(),
    ]
}

fn write_line(stream: &mut UnixStream, value: &Value) -> Result<(), RelayError> {
    let mut line = value.to_string();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|e| RelayError::Unreachable(e.to_string()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, RelayError> {
    let text = fs::read_to_string(path).map_err(|e| RelayError::Unreachable(e.to_string()))?;
    // The parser's own message is never surfaced: a malformed key file's
    // error could otherwise quote the token it failed on.
    serde_json::from_str(&text)
        .map_err(|_| RelayError::Unreachable("malformed session file".into()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registry {
    messaging_socket_path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Key {
    peer_token: PeerToken,
}

/// A relay session's peer token: a credential, since anyone holding it can
/// send it messages. `Debug` does not leak it.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
struct PeerToken(String);

impl PeerToken {
    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PeerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PeerToken(..)")
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;

    use super::*;

    #[test]
    fn the_relay_starts_isolated_and_findable_by_its_fixed_name() {
        let argv: Vec<String> = start_argv(
            Path::new("/k/relay/settings.json"),
            Path::new("/k/relay/instructions.md"),
        )
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
        assert_eq!(
            argv,
            [
                "--bg",
                "--remote-control",
                NAME,
                "--name",
                NAME,
                "--model",
                MODEL,
                "--effort",
                EFFORT,
                "--setting-sources",
                "",
                "--settings",
                "/k/relay/settings.json",
                "--append-system-prompt-file",
                "/k/relay/instructions.md",
            ]
        );
    }

    #[test]
    fn debug_does_not_leak_the_peer_token() {
        let token = PeerToken("s3cr3t-token".into());
        assert_eq!(format!("{token:?}"), "PeerToken(..)");
    }

    // Framed the way the experiments repo's `relay_inject.py send` framed
    // it: an auth line naming the peer token, then one user message, each
    // one newline-delimited JSON.
    #[test]
    fn a_send_writes_an_auth_line_then_one_user_message() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("relay.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let served = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut lines = BufReader::new(stream).lines();
            let auth: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            let user: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            (auth, user)
        });

        let mut stream = UnixStream::connect(&socket).unwrap();
        write_line(&mut stream, &json!({ "type": "auth", "token": "s3cr3t" })).unwrap();
        write_line(
            &mut stream,
            &json!({
                "type": "user",
                "message": { "role": "user", "content": "[kelpie]\nproject=shep ruling=1\n\nq" },
            }),
        )
        .unwrap();
        drop(stream);

        let (auth, user) = served.join().unwrap();
        assert_eq!(auth, json!({ "type": "auth", "token": "s3cr3t" }));
        assert_eq!(
            user,
            json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": "[kelpie]\nproject=shep ruling=1\n\nq",
                },
            })
        );
    }

    #[test]
    fn registry_and_peer_token_are_read_from_the_pids_own_files() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join(".claude/sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("123.json"),
            r#"{"messagingSocketPath":"/tmp/x.sock","sessionId":"abc"}"#,
        )
        .unwrap();
        fs::write(sessions.join("123.xyz.key"), r#"{"peerToken":"s3cr3t"}"#).unwrap();

        let relay = RelayCli::new(dir.path().to_owned(), dir.path().join("relay"));
        assert_eq!(
            relay.registry(123).unwrap().messaging_socket_path,
            PathBuf::from("/tmp/x.sock")
        );
        assert_eq!(relay.peer_token(123).unwrap().expose(), "s3cr3t");
    }

    // Pid 123's prefix must not match pid 1234's key file.
    #[test]
    fn a_pid_never_takes_another_pids_key() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join(".claude/sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("1234.abc.key"), r#"{"peerToken":"wrong"}"#).unwrap();

        let relay = RelayCli::new(dir.path().to_owned(), dir.path().join("relay"));
        assert!(relay.peer_token(123).is_err());
    }

    #[test]
    fn a_key_file_that_fails_to_parse_never_names_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.key");
        fs::write(&path, "not json, but s3cr3t-token if it were").unwrap();
        let err = read_json::<Key>(&path).unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot reach the relay: malformed session file"
        );
    }
}
