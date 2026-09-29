//! Saving the state file, and reading it back
//!
//! Each save goes to a temporary file that is synced and then renamed over
//! the old one, so a runner killed mid-write leaves the previous state whole.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{ProjectState, VERSION};

/// Why the state file cannot be read or written
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// Reading the file failed
    Read {
        /// The file
        path: PathBuf,
        /// What reading it failed with
        kind: io::ErrorKind,
    },
    /// The file is not a state file this version of kelpie wrote
    Malformed {
        /// The file
        path: PathBuf,
        /// The parser's message
        message: String,
    },
    /// The file carries a format version this kelpie does not read
    Version {
        /// The file
        path: PathBuf,
        /// The version it carries
        found: u32,
    },
    /// Writing the file failed, and the previous state still stands
    Write {
        /// The file
        path: PathBuf,
        /// What writing it failed with
        kind: io::ErrorKind,
    },
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, kind } => {
                write!(f, "cannot read state file {}: {kind}", path.display())
            }
            Self::Malformed { path, message } => {
                write!(f, "state file {} is malformed: {message}", path.display())
            }
            Self::Version { path, found } => write!(
                f,
                "state file {} is version {found}, and this kelpie reads version {VERSION}",
                path.display()
            ),
            Self::Write { path, kind } => {
                write!(f, "cannot write state file {}: {kind}", path.display())
            }
        }
    }
}

impl core::error::Error for StateError {}

/// Where a project's state lives on disk
#[derive(Debug, Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    /// A store for the state file at `path`
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Reads the state, or `None` when no state has been saved yet
    ///
    /// # Errors
    ///
    /// [`StateError`] when the file exists and cannot be read or understood.
    pub fn load(&self) -> Result<Option<ProjectState>, StateError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(self.error_read(e)),
        };

        // Only the version first, so a newer format reports as one.
        #[derive(Deserialize)]
        struct Versioned {
            version: u32,
        }
        let malformed = |e: serde_json::Error| StateError::Malformed {
            path: self.path.clone(),
            message: e.to_string(),
        };
        let found = serde_json::from_str::<Versioned>(&text)
            .map_err(malformed)?
            .version;
        if found != VERSION {
            return Err(StateError::Version {
                path: self.path.clone(),
                found,
            });
        }
        serde_json::from_str(&text).map(Some).map_err(malformed)
    }

    /// Replaces the saved state with `state`, atomically
    ///
    /// # Errors
    ///
    /// [`StateError::Write`] when any step fails. The previous state stands.
    pub fn save(&self, state: &ProjectState) -> Result<(), StateError> {
        let mut bytes = serde_json::to_vec_pretty(state).expect("state serializes to JSON");
        bytes.push(b'\n');
        write_atomically(&self.path, &bytes).map_err(|e| StateError::Write {
            path: self.path.clone(),
            kind: e.kind(),
        })
    }

    fn error_read(&self, e: io::Error) -> StateError {
        StateError::Read {
            path: self.path.clone(),
            kind: e.kind(),
        }
    }
}

pub(crate) fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    path.with_file_name(name)
}

// The rename is the commit point. Syncing the file first means the name
// never points at unwritten bytes; syncing the folder keeps the rename.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = temporary_path(path);
    let mut file = File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    let folder = path.parent().filter(|p| !p.as_os_str().is_empty());
    File::open(folder.unwrap_or(Path::new(".")))?.sync_all()
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::ports::Timestamp;
    use crate::state::{Ruling, RulingKind};

    const WRITER_DIR: &str = "KELPIE_TEST_WRITER_DIR";

    fn store_in(dir: &Path) -> StateStore {
        StateStore::new(dir.join("state.json"))
    }

    // Large, so a kill is likely to land inside a write, and uniform, so a
    // mix of two saves is visible.
    fn big_state(n: u64) -> ProjectState {
        let mut state = ProjectState::new(Timestamp(n));
        state.rulings = (0..20_000)
            .map(|id| Ruling {
                id,
                question: format!("question {n}"),
                pull_request: Some(n),
                kind: RulingKind::Closed,
                alerted: false,
                relayed: false,
            })
            .collect();
        state
    }

    fn assert_whole(state: &ProjectState) {
        let n = state.since.0;
        assert_eq!(state, &big_state(n), "state {n} was saved in part");
    }

    #[test]
    fn no_file_loads_as_no_state() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(store_in(dir.path()).load().unwrap(), None);
    }

    #[test]
    fn a_torn_temporary_file_leaves_the_saved_state_readable() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.save(&big_state(1)).unwrap();
        let whole = serde_json::to_vec(&big_state(2)).unwrap();
        fs::write(temporary_path(&store.path), &whole[..whole.len() / 2]).unwrap();

        assert_eq!(store.load().unwrap(), Some(big_state(1)));
        store.save(&big_state(3)).unwrap();
        assert_eq!(store.load().unwrap(), Some(big_state(3)));
    }

    #[test]
    fn a_malformed_file_names_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        fs::write(dir.path().join("state.json"), "{\"version\": 1, \"run\": ").unwrap();
        let err = store.load().unwrap_err();
        assert!(matches!(&err, StateError::Malformed { path, .. } if path == &store.path));
        assert!(err.to_string().contains("state.json is malformed"));
    }

    #[test]
    fn a_newer_format_is_reported_as_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        fs::write(
            dir.path().join("state.json"),
            r#"{"version": 2, "shape": "new"}"#,
        )
        .unwrap();
        assert_eq!(
            store.load().unwrap_err(),
            StateError::Version {
                path: store.path.clone(),
                found: 2
            }
        );
    }

    #[test]
    fn a_write_that_cannot_happen_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("missing/state.json"));
        let err = store.save(&ProjectState::new(Timestamp(1))).unwrap_err();
        assert!(matches!(
            err,
            StateError::Write {
                kind: io::ErrorKind::NotFound,
                ..
            }
        ));
    }

    #[test]
    #[ignore = "a child process of a_runner_killed_mid_write_leaves_the_previous_state_readable"]
    fn writer_child() {
        let Ok(dir) = std::env::var(WRITER_DIR) else {
            return;
        };
        let store = store_in(Path::new(&dir));
        let mut out = io::stdout().lock();
        for n in 0..=u64::MAX {
            store.save(&big_state(n)).unwrap();
            writeln!(out, "saved {n}").unwrap();
            out.flush().unwrap();
        }
    }

    #[test]
    fn a_runner_killed_mid_write_leaves_the_previous_state_readable() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let temporary = temporary_path(&store.path);
        let mut torn = 0;
        for _ in 0..8 {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "state::store::tests::writer_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env(WRITER_DIR, dir.path())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            // The reader stays open until the kill, or the child would die on
            // a closed pipe between two saves.
            let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
            let line = lines.find(|line| line.as_ref().is_ok_and(|l| l.starts_with("saved ")));
            line.expect("the writer stopped before its first save")
                .unwrap();

            // Kill once the next save has bytes in its temporary file. A save
            // that writes the state file in place never gets there.
            let deadline = Instant::now() + Duration::from_secs(10);
            while !fs::metadata(&temporary).is_ok_and(|m| m.len() > 0) {
                assert!(Instant::now() < deadline, "no save wrote a temporary file");
                std::hint::spin_loop();
            }
            child.kill().unwrap();
            child.wait().unwrap();
            drop(lines);
            torn += usize::from(temporary.exists());

            assert_whole(&store.load().unwrap().expect("a saved state"));
        }
        assert!(
            torn > 0,
            "no kill landed inside a write, so nothing was tested"
        );
    }
}
