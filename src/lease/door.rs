//! The dog's door: the socket a command asks for a counted lease at
//!
//! A command connects to `<kelpie home>/dog/lease.sock` and sends one
//! [`Ask`] line. The dog answers with [`Answer`] lines: queued, then
//! granted. The command holds the lease for as long as it keeps the
//! connection open, so one that ends or dies gives its place back. A
//! worker's sandbox may connect to this socket and to no shep socket.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// The socket's path under kelpie's home, beside the dog's book
pub const SOCKET: &str = "dog/lease.sock";

/// The door under kelpie's home `kelpie_home`
pub fn socket_in(kelpie_home: &Path) -> PathBuf {
    kelpie_home.join(SOCKET)
}

/// The door: under `KELPIE_HOME`, or `~/.kelpie` when that is unset
///
/// # Errors
///
/// A message when neither `KELPIE_HOME` nor `HOME` is set.
pub fn socket() -> Result<PathBuf, String> {
    socket_from(std::env::var_os("KELPIE_HOME"), std::env::var_os("HOME"))
}

fn socket_from(kelpie_home: Option<OsString>, home: Option<OsString>) -> Result<PathBuf, String> {
    match (kelpie_home, home) {
        (Some(kelpie_home), _) => Ok(socket_in(Path::new(&kelpie_home))),
        (None, Some(home)) => Ok(socket_in(&Path::new(&home).join(".kelpie"))),
        (None, None) => Err("neither KELPIE_HOME nor HOME is set".into()),
    }
}

/// What a command asks for, its first and only line
// wire format: changing this is a breaking change to the door
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    /// The counted lease it wants
    pub take: String,
    /// Its process, for `status`
    pub pid: u32,
    /// What it runs, for `status`
    pub what: String,
}

/// What the dog answers, one line each
// wire format: changing this is a breaking change to the door
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    /// Waiting, with this many ahead
    Queued(usize),
    /// Held until the connection closes
    Granted,
    /// Refused, saying why, and the dog closes the connection
    Error(String),
}

/// One command's connection to the door, holding its place while open
#[derive(Debug)]
pub struct Visit {
    answers: Lines<BufReader<OwnedReadHalf>>,
    // Kept open: dropping it is how the dog hears the lease given back.
    _ask: OwnedWriteHalf,
}

impl Visit {
    /// Connects to the door at `socket` and asks
    ///
    /// # Errors
    ///
    /// A message naming the socket when the dog cannot be reached.
    pub async fn knock(socket: &Path, ask: &Ask) -> Result<Self, String> {
        let stream = UnixStream::connect(socket).await.map_err(|e| {
            format!(
                "cannot reach the dog at {}: {e}. Is the adopted kelpie running? `shep enable kelpie` runs it",
                socket.display()
            )
        })?;
        let (read, mut write) = stream.into_split();
        let mut line = serde_json::to_vec(ask).expect("an ask serializes to JSON");
        line.push(b'\n');
        write
            .write_all(&line)
            .await
            .map_err(|e| format!("cannot ask the dog: {e}"))?;
        Ok(Self {
            answers: BufReader::new(read).lines(),
            _ask: write,
        })
    }

    /// The dog's next answer, or `None` once it has closed the connection
    ///
    /// # Errors
    ///
    /// A message when the answer cannot be read or is not one.
    pub async fn answer(&mut self) -> Result<Option<Answer>, String> {
        let line = self
            .answers
            .next_line()
            .await
            .map_err(|e| format!("cannot hear the dog: {e}"))?;
        line.map(|line| {
            serde_json::from_str(&line).map_err(|e| format!("the dog answered {line:?} ({e})"))
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_door_is_beside_the_book() {
        let socket = |kelpie_home: Option<&str>, home: Option<&str>| {
            socket_from(kelpie_home.map(Into::into), home.map(Into::into))
        };
        assert_eq!(
            socket(Some("/k"), Some("/home/me")),
            Ok(PathBuf::from("/k/dog/lease.sock"))
        );
        assert_eq!(
            socket(None, Some("/home/me")),
            Ok(PathBuf::from("/home/me/.kelpie/dog/lease.sock"))
        );
        assert!(socket(None, None).is_err());
    }

    #[test]
    fn the_wire_format_is_pinned() {
        let ask = Ask {
            take: "cargo-test".into(),
            pid: 42,
            what: "cargo test".into(),
        };
        assert_eq!(
            serde_json::to_value(&ask).unwrap(),
            json!({ "take": "cargo-test", "pid": 42, "what": "cargo test" })
        );
        assert_eq!(
            serde_json::to_value([
                Answer::Queued(2),
                Answer::Granted,
                Answer::Error("no".into())
            ])
            .unwrap(),
            json!([{ "queued": 2 }, "granted", { "error": "no" }])
        );
    }
}
