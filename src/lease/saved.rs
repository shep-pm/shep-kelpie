//! The dog's lease book on disk, so a restart keeps it
//!
//! The dog saves the whole book after every change, with the same atomic
//! write as a runner's state file: who holds and waits for each kind, each
//! review window's quota, summons and refusal, and each runner run's
//! totals. On start it loads the file and checks every runner in it
//! against the live flock, so a holder whose sheep now has a different pid
//! is reclaimed. A missing file, or one from a build that wrote another
//! format, loads as an empty book.

use std::fmt;
use std::fs;
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Epoch, Holder, LeaseKind};
use crate::ports::Timestamp;
use crate::runner::ProjectName;
use crate::state::write_atomically;

/// The book file's format version
const VERSION: u32 = 1;

/// Everything the dog keeps across a restart
// wire format: changing this is a breaking change to the book file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedBook {
    version: u32,
    /// Every kind anyone has asked for or that has a window, by kind
    #[serde(default)]
    pub leases: Vec<SavedLease>,
    /// Each runner's current run and the totals it has raised, by sheep
    #[serde(default)]
    pub runs: Vec<SavedRun>,
    /// Runs already replaced, whose late metrics change nothing
    #[serde(default)]
    pub retired: Vec<SavedRunId>,
}

impl SavedBook {
    /// A book with these leases, runs and retired runs
    pub fn new(leases: Vec<SavedLease>, runs: Vec<SavedRun>, retired: Vec<SavedRunId>) -> Self {
        Self {
            version: VERSION,
            leases,
            runs,
            retired,
        }
    }
}

/// One kind's lease
// wire format: changing this is a breaking change to the book file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedLease {
    /// What it is for
    pub kind: LeaseKind,
    /// Who holds it and since when, if anyone
    pub held: Option<SavedHeld>,
    /// Who waits, next first
    pub queue: Vec<SavedHolder>,
    /// Its review window, for a kind that has one
    pub window: Option<SavedWindow>,
}

/// A held lease's holder and when it was granted
// wire format: changing this is a breaking change to the book file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedHeld {
    /// Who holds it
    pub holder: SavedHolder,
    /// Since when
    pub since: Timestamp,
}

/// A holder with its epoch, which `status` leaves out
// wire format: changing this is a breaking change to the book file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum SavedHolder {
    /// The maintainer
    Maintainer,
    /// One run of a project's runner
    Runner {
        /// The project
        project: ProjectName,
        /// Which run
        epoch: Epoch,
    },
}

impl From<&Holder> for SavedHolder {
    fn from(holder: &Holder) -> Self {
        match holder {
            Holder::Maintainer => Self::Maintainer,
            Holder::Runner { project, epoch } => Self::Runner {
                project: project.clone(),
                epoch: *epoch,
            },
        }
    }
}

impl From<SavedHolder> for Holder {
    fn from(holder: SavedHolder) -> Self {
        match holder {
            SavedHolder::Maintainer => Self::Maintainer,
            SavedHolder::Runner { project, epoch } => Self::Runner { project, epoch },
        }
    }
}

/// A review window's state
// wire format: changing this is a breaking change to the book file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedWindow {
    /// Summons an hour
    pub quota: NonZeroU32,
    /// When the footer that stated the quota was posted, once one was read
    pub quota_at: Option<Timestamp>,
    /// Accepted summons, oldest first
    pub summons: Vec<Timestamp>,
    /// The latest refusal, if one still counts
    pub refusal: Option<SavedRefusal>,
}

/// A refusal and when the dog heard it
// wire format: changing this is a breaking change to the book file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedRefusal {
    /// When the dog heard it
    pub heard: Timestamp,
    /// When it quoted the window opening
    pub opens: Timestamp,
}

/// One runner's current run
// wire format: changing this is a breaking change to the book file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedRun {
    /// The runner's sheep
    pub sheep: String,
    /// Which run
    pub epoch: Epoch,
    /// Its totals, by kind
    pub totals: Vec<SavedTotals>,
}

/// One run's two totals for one kind
// wire format: changing this is a breaking change to the book file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedTotals {
    /// The kind
    pub kind: LeaseKind,
    /// How many times it has asked
    pub want: u64,
    /// How many times it has given back or withdrawn
    #[serde(rename = "return")]
    pub give_back: u64,
}

/// A run already replaced
// wire format: changing this is a breaking change to the book file
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedRunId {
    /// The runner's sheep
    pub sheep: String,
    /// Which run
    pub epoch: Epoch,
}

/// Why the book file cannot be read or written
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookFileError {
    /// Reading the file failed
    Read {
        /// The file
        path: PathBuf,
        /// What reading it failed with
        kind: io::ErrorKind,
    },
    /// The file carries this format's version and is not a book
    Malformed {
        /// The file
        path: PathBuf,
        /// The parser's message
        message: String,
    },
    /// Writing the file failed, and the previous book still stands
    Write {
        /// The file
        path: PathBuf,
        /// What writing it failed with
        kind: io::ErrorKind,
    },
}

impl fmt::Display for BookFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, kind } => {
                write!(f, "cannot read book file {}: {kind}", path.display())
            }
            Self::Malformed { path, message } => {
                write!(f, "book file {} is malformed: {message}", path.display())
            }
            Self::Write { path, kind } => {
                write!(f, "cannot write book file {}: {kind}", path.display())
            }
        }
    }
}

impl core::error::Error for BookFileError {}

/// Where the dog's book lives on disk
#[derive(Debug, Clone)]
pub struct BookFile {
    path: PathBuf,
}

impl BookFile {
    /// The book file at `path`
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Where it is
    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the book, or `None` when there is no file or it is in a format
    /// another build wrote
    ///
    /// # Errors
    ///
    /// [`BookFileError`] when the file exists and cannot be read, or claims
    /// this format and is not a book.
    pub fn load(&self) -> Result<Option<SavedBook>, BookFileError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(BookFileError::Read {
                    path: self.path.clone(),
                    kind: e.kind(),
                });
            }
        };
        // Only the version first: another build's file is no book at all.
        #[derive(Deserialize)]
        struct Versioned {
            version: Option<u32>,
        }
        let malformed = |e: serde_json::Error| BookFileError::Malformed {
            path: self.path.clone(),
            message: e.to_string(),
        };
        let versioned = serde_json::from_str::<Versioned>(&text).map_err(malformed)?;
        if versioned.version != Some(VERSION) {
            return Ok(None);
        }
        serde_json::from_str(&text).map(Some).map_err(malformed)
    }

    /// Replaces the saved book with `book`, atomically
    ///
    /// # Errors
    ///
    /// [`BookFileError::Write`] when any step fails. The previous book stands.
    pub fn save(&self, book: &SavedBook) -> Result<(), BookFileError> {
        let mut bytes = serde_json::to_vec_pretty(book).expect("the book serializes to JSON");
        bytes.push(b'\n');
        write_atomically(&self.path, &bytes).map_err(|e| BookFileError::Write {
            path: self.path.clone(),
            kind: e.kind(),
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn file_in(dir: &Path) -> BookFile {
        BookFile::new(dir.join("book.json"))
    }

    fn a_book() -> SavedBook {
        let koji = ProjectName::try_from("koji").unwrap();
        SavedBook::new(
            vec![SavedLease {
                kind: LeaseKind::coderabbit(),
                held: Some(SavedHeld {
                    holder: SavedHolder::Runner {
                        project: koji,
                        epoch: Epoch(101),
                    },
                    since: Timestamp(7),
                }),
                queue: vec![SavedHolder::Maintainer],
                window: Some(SavedWindow {
                    quota: NonZeroU32::new(10).unwrap(),
                    quota_at: Some(Timestamp(5)),
                    summons: vec![Timestamp(6)],
                    refusal: Some(SavedRefusal {
                        heard: Timestamp(3),
                        opens: Timestamp(4),
                    }),
                }),
            }],
            vec![SavedRun {
                sheep: "koji".into(),
                epoch: Epoch(101),
                totals: vec![SavedTotals {
                    kind: LeaseKind::coderabbit(),
                    want: 2,
                    give_back: 1,
                }],
            }],
            vec![SavedRunId {
                sheep: "koji".into(),
                epoch: Epoch(99),
            }],
        )
    }

    #[test]
    fn no_file_loads_as_no_book() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(file_in(dir.path()).load(), Ok(None));
    }

    #[test]
    fn a_saved_book_loads_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(dir.path());
        file.save(&a_book()).unwrap();
        assert_eq!(file.load(), Ok(Some(a_book())));
    }

    #[test]
    fn the_file_format_is_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(dir.path());
        file.save(&a_book()).unwrap();
        let text = fs::read_to_string(file.path()).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            json!({
                "version": 1,
                "leases": [{
                    "kind": "coderabbit",
                    "held": { "holder": { "runner": { "project": "koji", "epoch": 101 } }, "since": 7 },
                    "queue": ["maintainer"],
                    "window": {
                        "quota": 10,
                        "quota_at": 5,
                        "summons": [6],
                        "refusal": { "heard": 3, "opens": 4 },
                    },
                }],
                "runs": [{
                    "sheep": "koji",
                    "epoch": 101,
                    "totals": [{ "kind": "coderabbit", "want": 2, "return": 1 }],
                }],
                "retired": [{ "sheep": "koji", "epoch": 99 }],
            })
        );
    }

    #[test]
    fn a_file_another_build_wrote_loads_as_no_book() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(dir.path());
        for other in [
            r#"{"version": 2, "shape": "new"}"#,
            r#"{"leases": {"coderabbit": "koji"}}"#,
        ] {
            fs::write(file.path(), other).unwrap();
            assert_eq!(file.load(), Ok(None), "{other}");
        }
    }

    #[test]
    fn a_book_that_is_not_one_names_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_in(dir.path());
        for bad in [
            r#"{"version": 1, "leases": "#,
            r#"{"version": 1, "leases": [{"kind": "gpu", "held": null, "queue": [], "window": null}]}"#,
            r#"{"version": 1, "runs": [], "leases": [{"kind": "coderabbit", "held": null, "queue": [],
                "window": {"quota": 0, "quota_at": null, "summons": [], "refusal": null}}]}"#,
        ] {
            fs::write(file.path(), bad).unwrap();
            let err = file.load().unwrap_err();
            assert!(
                matches!(&err, BookFileError::Malformed { path, .. } if path == file.path()),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn a_write_that_cannot_happen_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = BookFile::new(dir.path().join("missing/book.json"));
        assert!(matches!(
            file.save(&SavedBook::new(Vec::new(), Vec::new(), Vec::new())),
            Err(BookFileError::Write {
                kind: io::ErrorKind::NotFound,
                ..
            })
        ));
    }
}
