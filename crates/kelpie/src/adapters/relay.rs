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
use crate::relay::{self, NAME};
use crate::settings::Effort;

mod lock;

use lock::StartLock;

/// How many times a started relay is looked for before giving up, and how
/// long to wait between looks: about the 2 seconds a real session took to
/// register, with room to spare.
const APPEAR_TRIES: u32 = 20;
const APPEAR_POLL: Duration = Duration::from_millis(250);

/// Once a fresh relay appears in the agent listing, how much longer its
/// messaging socket is given before the first write to it: measured live
/// on #14, a message sent the moment a brand-new session appeared never
/// reached it, though the same session took a message minutes later.
const SOCKET_GRACE: Duration = Duration::from_secs(2);

/// How many times a session's registry or peer-token file is looked for
/// before giving up, and how long to wait between looks: the listing can
/// name a pid before that pid has written its own files.
const SESSION_FILE_TRIES: u32 = 10;
const SESSION_FILE_POLL: Duration = Duration::from_millis(200);

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

    // Every project is its own process, and all of them target the one
    // fixed relay name, so an in-process guard alone cannot stop two
    // processes racing to find none running and both start one.
    fn start_lock(&self) -> StartLock {
        StartLock::under(&self.folder)
    }

    fn find(&self) -> Result<Option<Found>, RelayError> {
        let output = Command::new("claude")
            .args(["agents", "--json", "--all"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| RelayError::Unreachable(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(RelayError::Unreachable(stderr));
        }
        let agents: Vec<Value> = serde_json::from_slice(&output.stdout)
            .map_err(|_| RelayError::Unreachable("unreadable agent list".into()))?;
        Ok(pid_of_the_relay(&agents).map(|pid| Found { pid }))
    }

    // Kelpie is not the only thing that can bring the relay's process back:
    // Claude Code's own background-session handling was seen live on #14
    // reviving a killed session under its old pid, bypassing `start`
    // entirely. So the settings and instructions files are rewritten here,
    // unconditionally, on every `send`, whether or not this call ends up
    // starting a process itself: an upgrade's new settings reach the file
    // kelpie owns even when nothing of kelpie's runs to write it.
    fn write_relay_files(&self) -> Result<(PathBuf, PathBuf), RelayError> {
        fs::create_dir_all(&self.folder).map_err(|e| RelayError::CannotStart(e.to_string()))?;
        let settings = self.folder.join("settings.json");
        let instructions = self.folder.join("instructions.md");
        let text = serde_json::to_string_pretty(&relay::settings()).expect("settings are JSON");
        fs::write(&settings, text).map_err(|e| RelayError::CannotStart(e.to_string()))?;
        fs::write(&instructions, relay::INSTRUCTIONS)
            .map_err(|e| RelayError::CannotStart(e.to_string()))?;
        Ok((settings, instructions))
    }

    fn start(
        &self,
        settings: &Path,
        instructions: &Path,
        model: &str,
        effort: Effort,
    ) -> Result<(), RelayError> {
        let mut command = Command::new("claude");
        relay_env(&mut command);
        let output = command
            .args(start_argv(settings, instructions, model, effort))
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

    // Whether a fresh relay had to be started, alongside where it is: a
    // freshly started one is given `SOCKET_GRACE` longer before the first
    // write to its socket, past what it took to appear in the listing.
    fn ensure_running(
        &self,
        settings: &Path,
        instructions: &Path,
        model: &str,
        effort: Effort,
    ) -> Result<(Found, bool), RelayError> {
        if let Some(found) = self.find()? {
            return Ok((found, false));
        }
        let _held = self.start_lock().acquire()?;
        // Another process may have started one while this one waited.
        if let Some(found) = self.find()? {
            return Ok((found, false));
        }
        self.start(settings, instructions, model, effort)?;
        for _ in 0..APPEAR_TRIES {
            thread::sleep(APPEAR_POLL);
            // A transient hiccup in `claude agents` here is not the relay
            // failing to start: only running out of tries is.
            if let Ok(Some(found)) = self.find() {
                return Ok((found, true));
            }
        }
        Err(RelayError::NeverAppeared)
    }

    fn sessions(&self) -> PathBuf {
        self.home.join(".claude/sessions")
    }

    // Retried: `claude agents` can name a pid before that pid has written
    // its own registry file.
    fn registry(&self, pid: u32) -> Result<Registry, RelayError> {
        retry_session_file(|| read_json(&self.sessions().join(format!("{pid}.json"))))
    }

    fn peer_token(&self, pid: u32) -> Result<PeerToken, RelayError> {
        let prefix = format!("{pid}.");
        retry_session_file(|| {
            let entries = fs::read_dir(self.sessions())
                .map_err(|e| RelayError::Unreachable(e.to_string()))?;
            let key = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .find(|p| {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                    name.starts_with(&prefix) && name.ends_with(".key")
                })
                .ok_or_else(|| RelayError::Unreachable("no peer-token file".into()))?;
            read_json::<Key>(&key).map(|k| k.peer_token)
        })
    }
}

impl Relay for RelayCli {
    fn send(&self, message: &str, model: &str, effort: Effort) -> Result<(), RelayError> {
        let (settings, instructions) = self.write_relay_files()?;
        let (found, fresh) = self.ensure_running(&settings, &instructions, model, effort)?;
        if fresh {
            thread::sleep(SOCKET_GRACE);
        }
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
// A finished relay with the same fixed name stays in `claude agents --json
// --all`'s listing, pid-less, so the first name match is not necessarily a
// live one: every match is checked for a pid before one still running is
// given up on. Measured live on #14: taking only the first name match found
// a done session ahead of a live one and started a second relay.
fn pid_of_the_relay(agents: &[Value]) -> Option<u32> {
    agents
        .iter()
        .filter(|a| a["name"] == json!(NAME))
        .find_map(|a| a["pid"].as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
}

fn start_argv(settings: &Path, instructions: &Path, model: &str, effort: Effort) -> Vec<OsString> {
    vec![
        "--bg".into(),
        "--remote-control".into(),
        NAME.into(),
        "--name".into(),
        NAME.into(),
        "--model".into(),
        model.into(),
        "--effort".into(),
        effort.as_str().into(),
        "--setting-sources".into(),
        "".into(),
        "--settings".into(),
        settings.to_owned().into(),
        "--append-system-prompt-file".into(),
        instructions.to_owned().into(),
    ]
}

// A session file the listing can name before its own pid has written it.
fn retry_session_file<T>(read: impl FnMut() -> Result<T, RelayError>) -> Result<T, RelayError> {
    retry(SESSION_FILE_TRIES, SESSION_FILE_POLL, read)
}

fn retry<T>(
    tries: u32,
    poll: Duration,
    mut attempt: impl FnMut() -> Result<T, RelayError>,
) -> Result<T, RelayError> {
    for _ in 0..tries.saturating_sub(1) {
        if let Ok(value) = attempt() {
            return Ok(value);
        }
        thread::sleep(poll);
    }
    attempt()
}

// What the relay needs from kelpie's own environment: a home to find its
// trust and sessions under, a shell to run in, a temporary folder, and
// whose account it is, mirroring the minimal set a pinned shep sheep
// itself starts with (see docs/design-log.md). Everything else kelpie's
// own process happens to carry stays out of the relay's.
const RELAY_ENV: [&str; 5] = ["HOME", "PATH", "TMPDIR", "USER", "LANG"];

fn relay_env(command: &mut Command) {
    command.env_clear();
    for var in RELAY_ENV {
        if let Ok(value) = std::env::var(var) {
            command.env(var, value);
        }
    }
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

    // Recorded live on #14: a finished relay with no `pid` field, listed
    // before a live one with the same name.
    #[test]
    fn a_finished_relay_ahead_of_a_live_one_is_not_mistaken_for_none_running() {
        let agents = vec![
            json!({ "id": "2b8edbf5", "name": NAME, "state": "done" }),
            json!({ "pid": 3145, "id": "a3069699", "name": NAME, "status": "idle" }),
        ];
        assert_eq!(pid_of_the_relay(&agents), Some(3145));
    }

    #[test]
    fn no_matching_name_is_none() {
        let agents = vec![json!({ "pid": 1, "name": "something-else" })];
        assert_eq!(pid_of_the_relay(&agents), None);
    }

    #[test]
    fn the_relay_starts_isolated_and_findable_by_its_fixed_name() {
        let argv: Vec<String> = start_argv(
            Path::new("/k/relay/settings.json"),
            Path::new("/k/relay/instructions.md"),
            "claude-haiku-4-5-20251001",
            Effort::Low,
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
                "claude-haiku-4-5-20251001",
                "--effort",
                "low",
                "--setting-sources",
                "",
                "--settings",
                "/k/relay/settings.json",
                "--append-system-prompt-file",
                "/k/relay/instructions.md",
            ]
        );
    }

    // The `ask` rule on `relay-yes`, and the prompt for anything the
    // settings do not name, are the whole fence: a permissive permission
    // mode here would let the relay run past either one.
    #[test]
    fn the_relay_never_starts_in_a_permissive_permission_mode() {
        let argv: Vec<String> = start_argv(
            Path::new("/k/relay/settings.json"),
            Path::new("/k/relay/instructions.md"),
            "claude-haiku-4-5-20251001",
            Effort::Low,
        )
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
        assert!(!argv.contains(&"--permission-mode".to_owned()), "{argv:?}");
        assert!(
            !argv.iter().any(|a| a == "bypassPermissions" || a == "auto"),
            "{argv:?}"
        );
        assert!(
            !argv.contains(&"--dangerously-skip-permissions".to_owned()),
            "{argv:?}"
        );
    }

    // Measured live on #14: Claude Code's own background-session handling
    // can revive a killed relay under its old pid, bypassing `start`
    // entirely, so a settings.json an upgrade left stale never gets
    // rewritten unless writing it does not wait on `start` running at all.
    #[test]
    fn a_stale_settings_file_is_rewritten_whether_or_not_a_start_happens() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("relay");
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join("settings.json"),
            r#"{"crossSessionInbound":"accept"}"#,
        )
        .unwrap();

        let relay = RelayCli::new(dir.path().to_owned(), folder.clone());
        let (settings, _) = relay.write_relay_files().unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
        assert_eq!(written, relay::settings());
    }

    #[test]
    fn the_relay_gets_only_the_minimal_environment() {
        let mut command = Command::new("true");
        relay_env(&mut command);
        let names: Vec<&str> = command
            .get_envs()
            .map(|(name, _)| name.to_str().unwrap())
            .collect();
        for name in &names {
            assert!(
                RELAY_ENV.contains(name),
                "{name} should not reach the relay"
            );
        }
        // A var this process carries but `relay_env` does not name never
        // reaches the child: `env_clear` ran before the allowed set was applied.
        assert!(!names.contains(&"KELPIE_HOME"));
    }

    #[test]
    fn retry_succeeds_once_the_attempt_stops_failing() {
        let tries = std::cell::Cell::new(0);
        let result = retry(5, Duration::from_millis(1), || {
            tries.set(tries.get() + 1);
            if tries.get() < 3 {
                Err(RelayError::Unreachable("not yet".into()))
            } else {
                Ok(tries.get())
            }
        });
        assert_eq!(result, Ok(3));
    }

    #[test]
    fn retry_gives_up_after_its_tries_and_surfaces_the_last_error() {
        let calls = std::cell::Cell::new(0);
        let result = retry(3, Duration::from_millis(1), || {
            calls.set(calls.get() + 1);
            Err::<(), _>(RelayError::Unreachable("still not there".into()))
        });
        assert_eq!(calls.get(), 3);
        assert_eq!(
            result,
            Err(RelayError::Unreachable("still not there".into()))
        );
    }

    #[test]
    fn retry_with_zero_tries_still_makes_the_one_attempt() {
        let calls = std::cell::Cell::new(0);
        let result = retry(0, Duration::from_millis(1), || {
            calls.set(calls.get() + 1);
            Err::<(), _>(RelayError::Unreachable("no".into()))
        });
        assert_eq!(calls.get(), 1, "a bad tries count must not hang or panic");
        assert_eq!(result, Err(RelayError::Unreachable("no".into())));
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
    //
    // Real time: peer_token retries a missing file for the whole session-
    // file wait before giving up, about 1.8s here.
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
