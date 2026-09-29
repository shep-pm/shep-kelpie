//! The project's state file
//!
//! What a runner must remember across a restart: whether the project is
//! running, its open work items, pending rulings and leases held. Each save goes
//! to a temporary file that is synced and then renamed over the old one, so
//! a runner killed mid-write leaves the previous state whole.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pacer::DayStart;
use crate::ports::{Finding, Timestamp};
use crate::work_item::{Known, Phase, Review, Turn, WorkItem};

/// The state file's format version
const VERSION: u32 = 2;

/// The format before a project could have more than one work item open,
/// which this kelpie still reads
const ONE_ITEM: u32 = 1;

/// Everything a project's runner keeps across a restart
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectState {
    version: u32,
    /// Whether the project is running or paused
    pub run: RunState,
    /// When `run` last changed
    pub since: Timestamp,
    /// The open work items, oldest first
    #[serde(default)]
    pub work_items: Vec<WorkItem>,
    /// The work item a version 1 file kept, which loads into `work_items`
    #[serde(default, skip_serializing)]
    work_item: Option<WorkItem>,
    /// Rulings waiting on the maintainer, oldest first
    pub rulings: Vec<Ruling>,
    /// The id of the last ruling raised, so no id is ever given twice
    #[serde(default)]
    pub last_ruling: u64,
    /// Issues whose work items kelpie finished, which the board never takes again
    #[serde(default)]
    pub finished: Vec<u64>,
    /// The forge's ids of the reviews that started a rework or were refused
    /// one. None of them starts another.
    #[serde(default)]
    pub reworked: Vec<String>,
    /// Pull requests adopted and waiting for a free slot, oldest first
    #[serde(default)]
    pub adopted: Vec<Waiting>,
    /// Leases this project holds
    pub leases: Vec<LeaseHeld>,
    /// What the week had spent when today began, once usage has been read
    #[serde(default)]
    pub pacing: Option<DayStart>,
    /// Automatic merges not yet sent, oldest first
    #[serde(default)]
    pub notices: Vec<Notice>,
}

impl ProjectState {
    /// A paused project with nothing in flight
    pub fn new(since: Timestamp) -> Self {
        Self {
            version: VERSION,
            run: RunState::Paused,
            since,
            work_items: Vec::new(),
            work_item: None,
            rulings: Vec::new(),
            last_ruling: 0,
            finished: Vec::new(),
            reworked: Vec::new(),
            adopted: Vec::new(),
            leases: Vec::new(),
            pacing: None,
            notices: Vec::new(),
        }
    }

    /// The open work items' issues, oldest first
    pub fn open_issues(&self) -> Vec<u64> {
        self.work_items.iter().map(|item| item.issue).collect()
    }

    /// The open work item for `issue`
    pub fn item(&self, issue: u64) -> Option<&WorkItem> {
        self.work_items.iter().find(|item| item.issue == issue)
    }

    /// The open work item for `issue`, to change
    pub fn item_mut(&mut self, issue: u64) -> Option<&mut WorkItem> {
        self.work_items.iter_mut().find(|item| item.issue == issue)
    }

    // A version 1 file held one work item, and every ruling in it was that
    // item's.
    fn one_item_moved(mut self) -> Self {
        let Some(item) = self.work_item.take() else {
            return self;
        };
        for ruling in &mut self.rulings {
            ruling.issue.get_or_insert(item.issue);
        }
        self.work_items = vec![item];
        self
    }
}

/// A pull request adopted and waiting its turn
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Waiting {
    /// The pull request
    pub pull_request: u64,
    /// Whether `ready-for-agent` adopted it, so taking the label off takes it back
    pub by_label: bool,
}

/// Whether a project takes work
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    /// Dispatching and working
    Running,
    /// Taking no new turns
    Paused,
}

/// A decision only the maintainer makes
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ruling {
    /// What the answering trigger names
    pub id: u64,
    /// The issue of the work item it parks. None only in an older file
    /// with no work item open.
    #[serde(default)]
    pub issue: Option<u64>,
    /// The question, as the maintainer reads it
    pub question: String,
    /// The pull request it is about, once there is one
    pub pull_request: Option<u64>,
    /// What raised it, which decides what a yes does
    pub kind: RulingKind,
    /// Whether it reached the maintainer's webhook. One saved before
    /// webhooks existed is posted once.
    #[serde(default)]
    pub alerted: bool,
    /// Whether it reached a running relay, which is told if it is settled
    /// any other way
    #[serde(default)]
    pub relayed: bool,
}

/// A merge kelpie made under `auto`, told to the maintainer after it lands
///
/// Not a ruling: it has no id and takes no answer. It goes once the
/// webhook post lands.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notice {
    /// The work item's issue
    pub issue: u64,
    /// The pull request merged
    pub pull_request: u64,
    /// The head it merged at
    pub head: String,
    /// Whether kelpie's shots of that head failed, so none were on the pull request
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shots_failed: bool,
}

/// What raised a ruling. A no's note, or an answer, always goes to the worker.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RulingKind {
    /// CI is green on this head of a current branch. A yes merges it.
    Merge {
        /// The head the question is about
        head: String,
        /// Whether kelpie's shots of that head failed, so none are on the pull request
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        shots_failed: bool,
    },
    /// Kelpie could not rebase the branch onto `main`. A yes looks again.
    Rebase {
        /// Why
        reason: String,
    },
    /// CI failed again on a head whose red run the worker already had. A yes looks again.
    StillRed {
        /// The head
        head: String,
        /// The checks that failed
        checks: Vec<String>,
    },
    /// The forge refused an automatic merge again after a catch-up. A yes
    /// looks again.
    MergeRefused {
        /// The head the second refusal was about
        head: String,
        /// The forge's reason
        reason: String,
    },
    /// Someone closed the pull request without merging it. A yes drops the
    /// work item and keeps its branch on the forge.
    Closed,
    /// The qwen-review loop passed its round guard without settling. A yes
    /// lets it past the guard for the rest of this work item.
    ReviewGuard {
        /// The review, at the round the guard stopped it on
        review: Review,
    },
    /// The worker's fix turn for held findings ended with nothing pushed. A
    /// yes sends it the same findings again.
    FixNotPushed {
        /// The round whose findings hold, still fixing
        #[serde(flatten)]
        fix: Fix,
        /// The fix turn a yes starts
        prompt: String,
    },
    /// CodeRabbit's rounds reached their cap with findings the judge held.
    /// A yes sends the worker those findings and lifts the cap for the rest
    /// of this work item.
    #[serde(rename = "coderabbit-cap")]
    CodeRabbitCap {
        /// Rounds run
        rounds: u32,
        /// Findings the judge held
        held: u32,
        /// The fix turn a yes starts
        prompt: String,
        /// The head the findings are on, which the fix must move. None in
        /// an older state file, whose fix is not checked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<String>,
    },
    /// CodeRabbit never reviewed this head after a summon. A yes looks at
    /// CI again, and summons again once it is green.
    #[serde(rename = "coderabbit-silent")]
    CodeRabbitSilent {
        /// The head the summon was for
        head: String,
    },
    /// A merged pull request left confirmed findings unfixed. A yes files
    /// each as an issue on the project, and a no drops them.
    FollowUp {
        /// The findings, at the judge's severity
        findings: Vec<Finding>,
        /// Why the forge would not take them, when it has refused for hours
        /// and a yes tries again. None when the ruling comes before filing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refused: Option<String>,
    },
    /// The worker ended its turn on a question. The answer is its next turn.
    Question {
        /// The question, verbatim from the worker's question block
        asked: String,
        /// Where the qwen-review loop stood when the question interrupted
        /// it, so the answer resumes the right place instead of the
        /// ordinary rule (a known pull request goes straight to CI)
        resume: Resume,
    },
    /// A worker's turn ran past its ceiling and kelpie stopped it, keeping
    /// its session. A yes resumes it; a no stops the work item.
    TurnTimeout {
        /// The phase the turn ran in, which a yes resumes. None in an
        /// older state file, which resumes under Implement.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<Phase>,
    },
    /// A worker's turn could not run, or its call failed. A yes retries the
    /// step that failed; a no stops the work item.
    TurnFailed {
        /// Why it failed
        reason: String,
        /// The phase the turn ran in, which a yes goes back to
        phase: Phase,
        /// The turn as it stood before it failed, which a yes puts back
        retry: Turn,
    },
    /// The pull request changes Claude Code's own files, which run outside
    /// the sandbox. A yes accepts them at this head; a no stops the work item.
    ClaudeFiles {
        /// The head that changes them
        head: String,
        /// The files it changes
        files: Vec<String>,
        /// The phase the gate was in, which a yes goes back to
        phase: Phase,
    },
    /// The pull request's labels or ready state changed outside kelpie. A
    /// yes accepts the change and kelpie carries on watching it.
    ForeignChange {
        /// What changed, named plainly enough to answer from a phone
        description: String,
        /// The labels and ready state kelpie adopts as its own on a yes
        known: Known,
    },
}

/// The round a fix that pushed nothing was for
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Fix {
    /// A qwen-review round: the review, still fixing it
    Review(Review),
    /// A CodeRabbit round
    #[serde(rename = "coderabbit")]
    CodeRabbit {
        /// Its number
        round: u32,
        /// The head its findings are on, which the fix must move
        head: String,
    },
}

/// Where the qwen-review loop stood when a worker's question interrupted
/// it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Resume {
    /// No pull request existed yet; answering it changes nothing
    Nothing,
    /// A pull request existed, but the loop had not run its first round;
    /// once answered, start it
    ReviewFirst,
    /// The loop had already reached this round and stage; once answered,
    /// resume exactly there
    Review(Review),
    /// A CodeRabbit round's fix turn from `head`; once answered, the fix
    /// ends back in that round, which checks it moved the head
    #[serde(rename = "coderabbit-fix")]
    CodeRabbitFix {
        /// The head the findings are on
        head: String,
    },
}

/// A lease the dog granted this project
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseHeld {
    /// What the lease is for
    pub resource: Resource,
    /// The issue of the work item it was taken for
    #[serde(default)]
    pub issue: Option<u64>,
    /// When it was granted
    pub since: Timestamp,
}

/// A shared resource kelpie leases out
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Resource {
    /// The GPU the qwen-review loop runs on
    Gpu,
    /// The CodeRabbit review window
    Coderabbit,
}

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
                "state file {} is version {found}, and this kelpie reads versions {ONE_ITEM} and {VERSION}",
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
        if found != VERSION && found != ONE_ITEM {
            return Err(StateError::Version {
                path: self.path.clone(),
                found,
            });
        }
        let state: ProjectState = serde_json::from_str(&text).map_err(malformed)?;
        Ok(Some(ProjectState {
            version: VERSION,
            ..state.one_item_moved()
        }))
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
mod tests;
