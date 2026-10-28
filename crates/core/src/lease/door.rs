//! The dog's door: the socket a command asks for a counted lease at
//!
//! A command connects to `$SHEP_HOME/kelpie/dog/lease.sock`, or the socket
//! `KELPIE_LEASE_SOCKET` names (a named pipe on Windows, see [`ipc`]), and sends
//! one [`Ask`] line. The dog answers with [`Answer`] lines: queued, then
//! granted. The command holds the lease for as long as it keeps the
//! connection open, so one that ends or dies gives its place back. A
//! worker's sandbox may connect to this socket and to no shep socket.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::home;
use crate::ipc::{self, ClientStream};

/// The variable naming the door, which a runner sets for its workers
pub const SOCKET_VAR: &str = "KELPIE_LEASE_SOCKET";

/// The door's name in the dog's folder
const SOCKET: &str = "lease.sock";

/// The door: `KELPIE_LEASE_SOCKET`, or `lease.sock` in the dog's folder
/// under the shepherd's home
///
/// Never under `KELPIE_HOME`: shep starts the adopted dog without it, so
/// a runner and its workers would look somewhere the dog is not.
///
/// # Errors
///
/// A message when the shepherd's home cannot be worked out.
pub fn socket() -> Result<PathBuf, String> {
    let named = std::env::var_os(SOCKET_VAR).filter(|n| !n.is_empty());
    named.map_or_else(|| Ok(default(&home::shep_home()?)), |n| Ok(n.into()))
}

/// The lease door's socket at `shep_home`, or the default and a message saying why
///
/// The dog binds it and a runner lets its workers reach it. It is
/// `KELPIE_LEASE_SOCKET`, unless that is relative or under the shepherd's
/// home outside kelpie's. The path goes into the worker's sandbox, so it must
/// never be shep's own socket. The comparison resolves `..`, symlinks and a
/// volume's case.
pub fn checked_socket(shep_home: &Path) -> (PathBuf, Option<String>) {
    let named = std::env::var_os(SOCKET_VAR).filter(|n| !n.is_empty());
    checked(named.map(PathBuf::from), shep_home)
}

fn default(shep_home: &Path) -> PathBuf {
    home::dog_folder(shep_home).join(SOCKET)
}

fn checked(named: Option<PathBuf>, shep_home: &Path) -> (PathBuf, Option<String>) {
    let default = default(shep_home);
    let Some(named) = named else {
        return (default, None);
    };
    let (real, shep, kelpie) = (
        resolved(&named),
        resolved(shep_home),
        resolved(&home::under(shep_home)),
    );
    let shepherds = real.starts_with(&shep) && !real.starts_with(&kelpie);
    if named.is_absolute() && !shepherds {
        return (named, None);
    }
    let why = format!(
        "{SOCKET_VAR} names {}, which is relative or the shepherd's own: workers get {}",
        named.display(),
        default.display()
    );
    (default, Some(why))
}

// Where `path` leads: `..` applied after the symlinks before it, and each
// part that exists spelled as the volume does, so a case-insensitive one
// cannot be asked for shep's socket by another name. What does not exist
// yet is kept as written.
fn resolved(path: &Path) -> PathBuf {
    let mut real = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                real.pop();
            }
            Component::CurDir => {}
            part => {
                real.push(part);
                if let Ok(there) = real.canonicalize() {
                    real = there;
                }
            }
        }
    }
    real
}

/// The longest line either end of the door's socket handles as one: the dog
/// cuts an ask short here, and an e2e run's output is cut into lines no
/// longer
pub const LONGEST_LINE: u64 = 64 * 1024;

/// The lease an ask names to run a project's end-to-end tests on its host
pub const E2E: &str = "e2e";

/// What an ask names to reach the login's forge hold and pace, which no
/// connection holds: the dog answers one line and closes
pub const FORGE: &str = "forge";

/// What the dog says, first, when the project that asked for [`E2E`] has no
/// host to run it on
pub const NO_E2E_HOST: &str = "this project has no e2e host";

/// What an e2e run is `Done` with when it ran past the dog's ceiling and was
/// ended, as `timeout` exits
pub const TIMED_OUT: i32 = 124;

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
    /// Whether its command already runs under a grant from a dog that went
    /// away, so the dog seats it at once rather than counting it twice.
    /// The dog takes it on trust, as it does the rest of the ask.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub running: bool,
    /// The folder an `e2e` ask runs from, which the dog resolves to the
    /// project whose worktree holds it. The ask never names a host, a command
    /// or a results folder: the dog takes those from the project's own
    /// settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    /// What a `forge` ask wants of the login's forge hold and pace
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<ForgeAsk>,
}

impl Ask {
    /// An ask for the counted lease `take` from process `pid`, running
    /// `what`, with no folder and no forge ask, from a command not yet under
    /// a grant
    pub fn new(take: impl Into<String>, pid: u32, what: impl Into<String>) -> Self {
        Self {
            take: take.into(),
            pid,
            what: what.into(),
            running: false,
            worktree: None,
            forge: None,
        }
    }
}

/// What a runner asks of the login's forge hold and pace, in a [`FORGE`] ask
// wire format: changing this is a breaking change to the door
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeAsk {
    /// Whether a call of `project`'s may go: answered [`Answer::Go`] when it
    /// may, which counts it, [`Answer::Wait`] or [`Answer::Held`]
    Pass {
        /// The project whose runner would make the call
        project: String,
    },
    /// Until when the forge is held: [`Answer::Held`], or [`Answer::Go`] when it is not
    Held,
    /// The forge said its limit is used up until this time, in seconds since
    /// the epoch: answered [`Answer::Held`] with the hold now standing
    Hold(u64),
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
    /// A forge call may go, or no forge hold stands
    Go,
    /// A forge call waits this many seconds, then asks again
    Wait(u64),
    /// The forge is held until this time, in seconds since the epoch
    Held(u64),
    /// A line of an e2e run's output, as it arrives
    Output(String),
    /// An e2e run is over, with the exit code of its command, and the dog
    /// closes the connection
    Done(i32),
    /// Refused, saying why, and the dog closes the connection
    Error(String),
}

/// One command's connection to the door, holding its place while open
#[derive(Debug)]
pub struct Visit {
    // Kept open: closing it is how the dog hears the lease given back.
    stream: BufReader<ClientStream>,
}

impl Visit {
    /// Connects to the door at `socket` and asks
    ///
    /// # Errors
    ///
    /// A message naming the socket when the dog cannot be reached.
    pub async fn knock(socket: &Path, ask: &Ask) -> Result<Self, String> {
        let stream = ipc::connect(socket)
            .await
            .map_err(|e| format!("cannot reach the dog at {}: {e}", socket.display()))?;
        Self::over(stream, ask).await
    }

    /// Asks over `stream`, which is already connected to the dog
    ///
    /// # Errors
    ///
    /// A message when the ask cannot be sent.
    pub async fn over(mut stream: ClientStream, ask: &Ask) -> Result<Self, String> {
        let mut line = serde_json::to_vec(ask).expect("an ask serializes to JSON");
        line.push(b'\n');
        stream
            .write_all(&line)
            .await
            .map_err(|e| format!("cannot ask the dog: {e}"))?;
        Ok(Self {
            stream: BufReader::new(stream),
        })
    }

    /// The dog's next answer, or `None` once it has closed the connection
    ///
    /// # Errors
    ///
    /// A message when the answer cannot be read or is not one.
    pub async fn answer(&mut self) -> Result<Option<Answer>, String> {
        let mut line = String::new();
        let read = self
            .stream
            .read_line(&mut line)
            .await
            .map_err(|e| format!("cannot hear the dog: {e}"))?;
        if read == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        serde_json::from_str(line)
            .map(Some)
            .map_err(|e| format!("the dog answered {line:?} ({e})"))
    }

    /// Lets processes started from now on inherit the connection, or not
    ///
    /// A command started while it is inheritable holds the lease with its
    /// whole process tree, however the process that asked ends.
    ///
    /// # Errors
    ///
    /// A message when the connection's flags cannot be changed, and always on
    /// Windows, where a pipe's handle is not passed to a command yet.
    pub fn inheritable(&self, inherit: bool) -> Result<(), String> {
        #[cfg(unix)]
        {
            use rustix::io::{FdFlags, fcntl_setfd};
            let flags = if inherit {
                FdFlags::empty()
            } else {
                FdFlags::CLOEXEC
            };
            fcntl_setfd(self.stream.get_ref(), flags)
                .map_err(|e| format!("cannot pass the lease to the command: {e}"))
        }
        #[cfg(windows)]
        {
            let _ = inherit;
            Err("cannot pass the lease to the command: Windows does not inherit a pipe yet".into())
        }
    }

    /// Gives the lease back, whatever else still holds the connection
    ///
    /// Shutting the socket ends it for every process holding a copy, such
    /// as one the command left running, so the dog hears the end at once.
    pub async fn give_back(mut self) {
        let _ = self.stream.get_mut().shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_worker_never_gets_shep_s_socket_or_a_relative_one() {
        let base = std::env::temp_dir();
        let shep = base.join("me/.shep");
        let default = shep.join("kelpie").join("dog").join("lease.sock");
        assert_eq!(checked(None, &shep), (default.clone(), None));
        for fine in [base.join("k/door.sock"), shep.join("kelpie/dog/other.sock")] {
            assert_eq!(checked(Some(fine.clone()), &shep), (fine, None));
        }
        let shep_socket = shep.join("run/other.sock").display().to_string();
        for bad in [shep_socket.as_str(), "door.sock"] {
            let (door, why) = checked(Some(PathBuf::from(bad)), &shep);
            assert_eq!(door, default, "{bad}");
            assert!(why.unwrap().contains(bad));
        }
    }

    // The dog binds what the runner hands its workers, so both ask `checked`.
    #[cfg(unix)]
    #[test]
    fn the_dog_and_the_runner_resolve_one_door() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let shep = base.join("shep");
        std::fs::create_dir_all(shep.join("run")).unwrap();
        std::fs::create_dir_all(home::dog_folder(&shep)).unwrap();
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(shep.join("run"), outside.join("link")).unwrap();
        let default = default(&shep);
        let socket = home::shepherd_address(&shep);
        let inside = socket.strip_prefix(&shep).unwrap().to_owned();

        let mut bad = vec![
            PathBuf::from("door.sock"),
            socket.clone(),
            shep.join("kelpie/..").join(&inside),
            outside.join("link/shep.sock"),
            outside.join("link/../shep").join(&inside),
        ];
        // Only a case-insensitive volume finds the folder spelled this way.
        let shouting = shep.join("RUN/shep.sock");
        if shouting.parent().unwrap().exists() {
            bad.push(shouting);
        }
        for named in bad {
            let (door, why) = checked(Some(named.clone()), &shep);
            assert_eq!(door, default, "{}", named.display());
            assert!(why.unwrap().contains(&named.display().to_string()));
        }

        let fine = [
            outside.join("door.sock"),
            shep.join("kelpie/dog/other.sock"),
            shep.join("kelpie/../kelpie/dog/other.sock"),
        ];
        for named in fine {
            assert_eq!(checked(Some(named.clone()), &shep), (named, None));
        }
    }

    #[test]
    fn the_wire_format_is_pinned() {
        let ask = Ask::new("cargo-test", 42, "cargo test");
        assert_eq!(
            serde_json::to_value(&ask).unwrap(),
            json!({ "take": "cargo-test", "pid": 42, "what": "cargo test" })
        );
        let rejoining = Ask {
            running: true,
            ..ask.clone()
        };
        let wire = r#"{"take":"cargo-test","pid":42,"what":"cargo test"}"#;
        assert_eq!(serde_json::from_str::<Ask>(wire).unwrap(), ask);
        assert_eq!(
            serde_json::to_value(&rejoining).unwrap()["running"],
            json!(true)
        );
        let from = Ask {
            take: E2E.into(),
            worktree: Some(PathBuf::from("/k/koji/worktrees/7")),
            ..ask.clone()
        };
        assert_eq!(
            serde_json::to_value(&from).unwrap(),
            json!({
                "take": "e2e",
                "pid": 42,
                "what": "cargo test",
                "worktree": "/k/koji/worktrees/7"
            })
        );
        assert_eq!(
            serde_json::to_value([
                Answer::Queued(2),
                Answer::Granted,
                Answer::Error("no".into()),
                Answer::Output("ok 1".into()),
                Answer::Done(3),
                Answer::Go,
                Answer::Wait(4),
                Answer::Held(1_790_000_600),
            ])
            .unwrap(),
            json!([
                { "queued": 2 },
                "granted",
                { "error": "no" },
                { "output": "ok 1" },
                { "done": 3 },
                "go",
                { "wait": 4 },
                { "held": 1_790_000_600 }
            ])
        );
        let forge = Ask {
            take: FORGE.into(),
            forge: Some(ForgeAsk::Pass {
                project: "koji".into(),
            }),
            ..ask.clone()
        };
        assert_eq!(
            serde_json::to_value(&forge).unwrap()["forge"],
            json!({ "pass": { "project": "koji" } })
        );
        assert_eq!(
            serde_json::to_value([ForgeAsk::Held, ForgeAsk::Hold(7)]).unwrap(),
            json!(["held", { "hold": 7 }])
        );
        let read: Vec<Answer> = serde_json::from_str(
            r#"[{"queued":2},"granted",{"error":"no"},{"output":"ok 1"},{"done":3},"go",{"wait":4},{"held":1790000600}]"#,
        )
        .unwrap();
        assert_eq!(
            read,
            [
                Answer::Queued(2),
                Answer::Granted,
                Answer::Error("no".into()),
                Answer::Output("ok 1".into()),
                Answer::Done(3),
                Answer::Go,
                Answer::Wait(4),
                Answer::Held(1_790_000_600),
            ]
        );
    }
}
