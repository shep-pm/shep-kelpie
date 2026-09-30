//! Reading a background session's own files: its messaging socket's path
//! and its peer token
//!
//! `claude agents --json --all` can name a pid before that pid has
//! written its own registry (`~/.claude/sessions/<pid>.json`) or
//! peer-token (`~/.claude/sessions/<pid>.*.key`) file, so both are
//! retried a few times rather than failed on the first miss.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::ports::RelayError;

/// How many times a session's registry or peer-token file is looked for
/// before giving up, and how long to wait between looks.
const SESSION_FILE_TRIES: u32 = 10;
const SESSION_FILE_POLL: Duration = Duration::from_millis(200);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Registry {
    pub(super) messaging_socket_path: PathBuf,
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
pub(super) struct PeerToken(String);

impl PeerToken {
    pub(super) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PeerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PeerToken(..)")
    }
}

/// The registry file naming `pid`'s messaging socket, retried a few times
pub(super) fn registry(sessions: &Path, pid: u32) -> Result<Registry, RelayError> {
    retry_session_file(|| read_json(&sessions.join(format!("{pid}.json"))))
}

/// `pid`'s own peer token, retried a few times
pub(super) fn peer_token(sessions: &Path, pid: u32) -> Result<PeerToken, RelayError> {
    let prefix = format!("{pid}.");
    retry_session_file(|| {
        let entries = fs::read_dir(sessions).map_err(|e| RelayError::Unreachable(e.to_string()))?;
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

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, RelayError> {
    let text = fs::read_to_string(path).map_err(|e| RelayError::Unreachable(e.to_string()))?;
    // The parser's own message is never surfaced: a malformed key file's
    // error could otherwise quote the token it failed on.
    serde_json::from_str(&text)
        .map_err(|_| RelayError::Unreachable("malformed session file".into()))
}

fn retry_session_file<T>(read: impl FnMut() -> Result<T, RelayError>) -> Result<T, RelayError> {
    retry(SESSION_FILE_TRIES, SESSION_FILE_POLL, read)
}

pub(super) fn retry<T>(
    tries: u32,
    poll: Duration,
    mut attempt: impl FnMut() -> Result<T, RelayError>,
) -> Result<T, RelayError> {
    for _ in 0..tries.saturating_sub(1) {
        if let Ok(value) = attempt() {
            return Ok(value);
        }
        std::thread::sleep(poll);
    }
    attempt()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn registry_and_peer_token_are_read_from_the_pids_own_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.path().join("123.json"),
            r#"{"messagingSocketPath":"/tmp/x.sock","sessionId":"abc"}"#,
        )
        .unwrap();
        fs::write(dir.path().join("123.xyz.key"), r#"{"peerToken":"s3cr3t"}"#).unwrap();

        assert_eq!(
            registry(dir.path(), 123).unwrap().messaging_socket_path,
            PathBuf::from("/tmp/x.sock")
        );
        assert_eq!(peer_token(dir.path(), 123).unwrap().expose(), "s3cr3t");
    }

    // Pid 123's prefix must not match pid 1234's key file.
    //
    // Real time: peer_token retries a missing file for the whole session-
    // file wait before giving up, about 1.8s here.
    #[test]
    fn a_pid_never_takes_another_pids_key() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("1234.abc.key"), r#"{"peerToken":"wrong"}"#).unwrap();
        assert!(peer_token(dir.path(), 123).is_err());
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
