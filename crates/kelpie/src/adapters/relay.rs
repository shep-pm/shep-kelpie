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
use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::ports::{Relay, RelayError};
use crate::relay::{self, NAME};
use crate::settings::Effort;

mod lock;
mod session;

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

/// The running relay session `find` located
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    /// The short id `claude stop`/`claude rm` take
    id: String,
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
    /// Kelpie's shepherd, which the relay's commands must reach
    shep_home: PathBuf,
    /// The kelpie binary the relay's commands run, by its absolute path
    kelpie: PathBuf,
    /// The `claude` program every call runs
    claude: PathBuf,
}

impl RelayCli {
    /// A relay whose settings and instructions live under `folder`, and
    /// whose commands run `kelpie` against the shepherd at `shep_home`
    pub fn new(home: PathBuf, folder: PathBuf, shep_home: PathBuf, kelpie: PathBuf) -> Self {
        Self {
            home,
            folder,
            shep_home,
            kelpie,
            claude: PathBuf::from("claude"),
        }
    }

    // Every project is its own process, and all of them target the one
    // fixed relay name, so an in-process guard alone cannot stop two
    // processes racing to find none running and both start one.
    fn start_lock(&self) -> StartLock {
        StartLock::under(&self.folder)
    }

    // Kelpie owns exactly one relay. Any other session under the fixed name
    // is stopped and removed on the way: an older live one (kelpie never
    // starts a second while it can find the first) would answer messages
    // kelpie no longer expects it to see, and a stale one, listed with no
    // process, looks just like the live one under the same Remote Control
    // name on the maintainer's phone.
    fn find(&self) -> Result<Option<Found>, RelayError> {
        let output = Command::new(&self.claude)
            .args(["agents", "--json", "--all"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| RelayError::Unreachable(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(RelayError::Unreachable(stderr));
        }
        // The parse error is safe to surface as-is: an agent listing never
        // carries a secret, unlike a session's own registry or key file.
        let agents: Vec<Value> = serde_json::from_slice(&output.stdout)
            .map_err(|e| RelayError::Unreachable(format!("unreadable agent list: {e}")))?;
        let (newest, extras) = newest_and_extras(&agents);
        for id in extras {
            let _ = self.stop_and_remove(&id);
        }
        Ok(newest)
    }

    // `stop` only parks a session: Claude Code's own background handling
    // can bring a stopped or killed session back under the same id with
    // its conversation intact, which defeats the daily clear's whole
    // point of dropping whatever a worker's question tried to carry into
    // it. `rm` is the verb that actually deletes it.
    fn stop_and_remove(&self, id: &str) -> Result<(), RelayError> {
        let _ = Command::new(&self.claude)
            .args(["stop", id])
            .stdin(Stdio::null())
            .status();
        let status = Command::new(&self.claude)
            .args(["rm", id])
            .stdin(Stdio::null())
            .status()
            .map_err(|e| RelayError::CannotStop(e.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(RelayError::CannotStop(format!(
                "claude rm exited with {status}"
            )))
        }
    }

    // Kelpie is not the only thing that can bring the relay's process back:
    // Claude Code's own background-session handling was seen live on #14
    // reviving a killed session under its old pid, bypassing `start`
    // entirely. So the settings and instructions files are rewritten here,
    // unconditionally, on every `send`, whether or not this call ends up
    // starting a process itself: an upgrade's new settings reach the file
    // kelpie owns even when nothing of kelpie's runs to write it.
    fn write_relay_files(&self) -> Result<(PathBuf, PathBuf), RelayError> {
        let kelpie = relay::BarePath::of(&self.kelpie).ok_or_else(|| {
            RelayError::CannotStart(format!(
                "kelpie's path {} cannot be typed bare in a shell",
                self.kelpie.display()
            ))
        })?;
        fs::create_dir_all(&self.folder).map_err(|e| RelayError::CannotStart(e.to_string()))?;
        let settings = self.folder.join("settings.json");
        let instructions = self.folder.join("instructions.md");
        let text = serde_json::to_string_pretty(&relay::settings(&self.shep_home, kelpie))
            .expect("settings are JSON");
        fs::write(&settings, text).map_err(|e| RelayError::CannotStart(e.to_string()))?;
        fs::write(&instructions, relay::instructions(kelpie))
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
        let mut command = Command::new(&self.claude);
        self.relay_env(&mut command);
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

    // What the relay needs from kelpie's own environment: a home to find
    // its trust and sessions under, where `claude` and its own dependencies
    // resolve on `PATH`, a temporary folder, and whose account it is,
    // mirroring the minimal set a pinned shep sheep itself starts with
    // (see docs/design-log.md). Everything else kelpie's own process
    // happens to carry stays out of the relay's. The session's shell gets
    // `SHEP_HOME` from the settings' `env` block, not from this environment.
    //
    // `HOME` comes from `self.home`, not the ambient environment: `find`,
    // `current_dir` and the relay's own `~/.claude/sessions` all key off
    // `self.home`, and a process whose real `$HOME` ever differed from it
    // would otherwise start the relay somewhere it can never be found again.
    fn relay_env(&self, command: &mut Command) {
        command.env_clear();
        command.env("HOME", &self.home);
        for var in RELAY_ENV {
            if let Ok(value) = std::env::var(var) {
                command.env(var, value);
            }
        }
    }
}

impl Relay for RelayCli {
    fn send(&self, message: &str, model: &str, effort: Effort) -> Result<(), RelayError> {
        let (settings, instructions) = self.write_relay_files()?;
        let (found, fresh) = self.ensure_running(&settings, &instructions, model, effort)?;
        if fresh {
            thread::sleep(SOCKET_GRACE);
        }
        let registry = session::registry(&self.sessions(), found.pid)?;
        let token = session::peer_token(&self.sessions(), found.pid)?;
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
        self.stop_and_remove(&found.id)
    }
}

// `--remote-control` and `--name` share `NAME`: the first names the remote
// control channel, the second is what `claude agents --json --all` shows,
// and both must match for the same relay to be found again.
//
// A finished or stopped relay with the same fixed name stays in the
// listing, pid-less, so a name match without a pid is never live. Measured
// live on #14: taking the first name match regardless found a done session
// ahead of a live one and started a second relay. Among the live matches,
// the newest by `startedAt` is kept. Every other match is an extra, a
// stale one or an older live one, returned to be stopped and removed. A
// match whose pid cannot be read is neither, and is left alone.
fn newest_and_extras(agents: &[Value]) -> (Option<Found>, Vec<String>) {
    let mut live: Vec<(String, u32, i64)> = Vec::new();
    let mut extras: Vec<String> = Vec::new();
    for agent in agents.iter().filter(|a| a["name"] == json!(NAME)) {
        let Some(id) = agent["id"].as_str() else {
            continue;
        };
        match &agent["pid"] {
            Value::Null => extras.push(id.to_owned()),
            pid => {
                let Some(pid) = pid.as_u64().and_then(|p| u32::try_from(p).ok()) else {
                    continue;
                };
                let started = agent["startedAt"].as_i64().unwrap_or(0);
                live.push((id.to_owned(), pid, started));
            }
        }
    }
    live.sort_by_key(|(_, _, started)| *started);
    let newest = live.pop().map(|(id, pid, _)| Found { id, pid });
    extras.extend(live.into_iter().map(|(id, ..)| id));
    (newest, extras)
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

// The rest of `relay_env`'s allowed set, read from the ambient process
// environment: `HOME` itself comes from `self.home` (see `relay_env`).
const RELAY_ENV: [&str; 4] = ["PATH", "TMPDIR", "USER", "LANG"];

fn write_line(stream: &mut UnixStream, value: &Value) -> Result<(), RelayError> {
    let mut line = value.to_string();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .map_err(|e| RelayError::Unreachable(e.to_string()))
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
        let (found, extras) = newest_and_extras(&agents);
        assert_eq!(
            found,
            Some(Found {
                id: "a3069699".into(),
                pid: 3145
            })
        );
        assert_eq!(extras, ["2b8edbf5"]);
    }

    // A fake `claude` on disk: `agents` prints the recorded listing, every
    // call is logged one line each, and the calls are what the test reads.
    fn fake_claude(dir: &Path) -> RelayCli {
        let listing = dir.join("agents.json");
        fs::write(&listing, ROSTER).unwrap();
        let program = dir.join("claude");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{log}'\n[ \"$1\" = agents ] && cat '{listing}'\nexit 0\n",
            log = dir.join("calls").display(),
            listing = listing.display(),
        );
        crate::test::write_script(&program, &script);
        let mut relay = RelayCli::new(
            dir.to_owned(),
            dir.join("relay"),
            dir.join("shep"),
            "/k/bin/kelpie".into(),
        );
        relay.claude = program;
        relay
    }

    fn calls(dir: &Path) -> Vec<String> {
        fs::read_to_string(dir.join("calls"))
            .unwrap()
            .lines()
            .skip(1)
            .map(str::to_owned)
            .collect()
    }

    // Recorded from `claude agents --json --all` on a scratch session of
    // this branch's own, renamed to the relay's: a done and a stopped one
    // (no `pid`), the live one, and an unrelated session that must never
    // be touched.
    const ROSTER: &str = include_str!("../../fixtures/claude-agents-relay.json");

    #[test]
    fn the_lookup_takes_the_running_relay_and_removes_the_stale_ones() {
        let dir = tempfile::tempdir().unwrap();
        let found = fake_claude(dir.path()).find().unwrap();
        assert_eq!(
            found,
            Some(Found {
                id: "e9a38e1e".into(),
                pid: 76409
            })
        );
        assert_eq!(
            calls(dir.path()),
            [
                "stop de905a17",
                "rm de905a17",
                "stop 51b1967b",
                "rm 51b1967b"
            ]
        );
    }

    #[test]
    fn the_clear_stops_then_removes_the_running_relay_after_the_stale_ones() {
        let dir = tempfile::tempdir().unwrap();
        fake_claude(dir.path()).clear().unwrap();
        let calls = calls(dir.path());
        assert_eq!(
            calls,
            [
                "stop de905a17",
                "rm de905a17",
                "stop 51b1967b",
                "rm 51b1967b",
                "stop e9a38e1e",
                "rm e9a38e1e",
            ]
        );
        assert!(calls.iter().all(|c| !c.contains("1b2cf60f")));
    }

    #[test]
    fn no_matching_name_is_none() {
        let agents = vec![json!({ "pid": 1, "name": "something-else" })];
        assert_eq!(newest_and_extras(&agents), (None, Vec::new()));
    }

    #[test]
    fn a_pid_over_u32_max_is_dropped_rather_than_matched_wrong() {
        let agents = vec![json!({
            "pid": u64::from(u32::MAX) + 1,
            "id": "impossible",
            "name": NAME,
        })];
        assert_eq!(newest_and_extras(&agents), (None, Vec::new()));
    }

    // Measured live on #14: more than one live relay under the fixed name
    // at once, both with a pid. The newest is kept; the other is an extra
    // to stop and remove.
    #[test]
    fn more_than_one_live_relay_keeps_only_the_newest() {
        let agents = vec![
            json!({ "pid": 111, "id": "older", "name": NAME, "startedAt": 100 }),
            json!({ "pid": 222, "id": "newer", "name": NAME, "startedAt": 200 }),
        ];
        let (found, extras) = newest_and_extras(&agents);
        assert_eq!(
            found,
            Some(Found {
                id: "newer".into(),
                pid: 222
            })
        );
        assert_eq!(extras, ["older"]);
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
    fn a_kelpie_path_the_shell_would_split_starts_no_relay() {
        let dir = tempfile::tempdir().unwrap();
        let mut relay = fake_claude(dir.path());
        relay.kelpie = "/opt/my kelpie/kelpie".into();
        let sent = relay.send("[kelpie]", "claude-haiku-4-5-20251001", Effort::Low);
        assert!(matches!(sent, Err(RelayError::CannotStart(_))), "{sent:?}");
        assert!(!dir.path().join("relay/settings.json").exists());
        assert!(!dir.path().join("calls").exists(), "claude never ran");
    }

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

        let relay = RelayCli::new(
            dir.path().to_owned(),
            folder.clone(),
            "/k/shep".into(),
            "/k/bin/kelpie".into(),
        );
        let (settings, _) = relay.write_relay_files().unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
        assert_eq!(
            written,
            relay::settings(
                Path::new("/k/shep"),
                relay::BarePath::of(Path::new("/k/bin/kelpie")).unwrap()
            )
        );
    }

    #[test]
    fn the_relay_gets_only_the_minimal_environment() {
        let relay = RelayCli::new(
            PathBuf::from("/k/maintainer-home"),
            PathBuf::from("/k/relay"),
            PathBuf::from("/k/shep"),
            PathBuf::from("/k/bin/kelpie"),
        );
        let mut command = Command::new("true");
        relay.relay_env(&mut command);
        let envs: Vec<(&str, &str)> = command
            .get_envs()
            .map(|(name, value)| (name.to_str().unwrap(), value.unwrap().to_str().unwrap()))
            .collect();
        for (name, _) in &envs {
            assert!(
                *name == "HOME" || RELAY_ENV.contains(name),
                "{name} should not reach the relay"
            );
        }
        // HOME comes from the RelayCli itself, not this test process's own
        // $HOME, so `find` and the sessions it reads always agree on it.
        assert_eq!(
            envs.iter().find(|(n, _)| *n == "HOME"),
            Some(&("HOME", "/k/maintainer-home"))
        );
        // A var this process carries but `relay_env` does not name never
        // reaches the child: `env_clear` ran before the allowed set was applied.
        assert!(!envs.iter().any(|(n, _)| *n == "KELPIE_HOME"));
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
}
