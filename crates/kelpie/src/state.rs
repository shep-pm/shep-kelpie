//! The project's state file
//!
//! What a runner must remember across a restart: whether the project is
//! running, its work item, pending rulings and leases held. Each save goes
//! to a temporary file that is synced and then renamed over the old one, so
//! a runner killed mid-write leaves the previous state whole.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pacer::DayStart;
use crate::ports::Timestamp;
use crate::work_item::{Known, Phase, Review, WorkItem};

/// The state file's format version
const VERSION: u32 = 1;

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
    /// The work item in flight
    pub work_item: Option<WorkItem>,
    /// Rulings waiting on the maintainer, oldest first
    pub rulings: Vec<Ruling>,
    /// The id of the last ruling raised, so no id is ever given twice
    #[serde(default)]
    pub last_ruling: u64,
    /// Issues whose work items kelpie finished, which the board never takes again
    #[serde(default)]
    pub finished: Vec<u64>,
    /// The forge's ids of the reviews a rework was started or refused on,
    /// which never start one again
    #[serde(default)]
    pub reworked: Vec<String>,
    /// Leases this project holds
    pub leases: Vec<LeaseHeld>,
    /// What the week had spent when today began, once usage has been read
    #[serde(default)]
    pub pacing: Option<DayStart>,
}

impl ProjectState {
    /// A paused project with nothing in flight
    pub fn new(since: Timestamp) -> Self {
        Self {
            version: VERSION,
            run: RunState::Paused,
            since,
            work_item: None,
            rulings: Vec::new(),
            last_ruling: 0,
            finished: Vec::new(),
            reworked: Vec::new(),
            leases: Vec::new(),
            pacing: None,
        }
    }
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
                "state file {} is version {found}, and this kelpie reads version {VERSION}",
                path.display()
            ),
            Self::Write { path, kind } => {
                write!(f, "cannot write state file {}: {kind}", path.display())
            }
        }
    }
}

impl std::error::Error for StateError {}

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
    use crate::test::a_work_item;

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
    fn a_saved_state_loads_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let mut state = ProjectState::new(Timestamp(1_790_000_000));
        state.run = RunState::Running;
        state.work_item = Some(a_work_item());
        state.rulings.push(Ruling {
            id: 1,
            question: "merge #43?".into(),
            pull_request: Some(43),
            kind: RulingKind::Merge {
                head: "c0ffee".into(),
            },
            alerted: true,
        });
        state.last_ruling = 1;
        state.leases.push(LeaseHeld {
            resource: Resource::Gpu,
            since: Timestamp(1_790_000_100),
        });
        state.pacing = Some(DayStart {
            week_resets_at: Timestamp(1_790_500_000),
            day: 2,
            week_used_pct: 31,
        });
        store.save(&state).unwrap();
        assert_eq!(store.load().unwrap(), Some(state));
    }

    #[test]
    fn a_file_saved_before_pacing_loads_with_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let old =
            r#"{"version":1,"run":"running","since":7,"work_item":null,"rulings":[],"leases":[]}"#;
        fs::write(dir.path().join("state.json"), old).unwrap();
        let state = store.load().unwrap().unwrap();
        assert_eq!((state.run, state.pacing), (RunState::Running, None));
    }

    #[test]
    fn the_file_format_is_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let mut state = ProjectState::new(Timestamp(7));
        state.leases.push(LeaseHeld {
            resource: Resource::Coderabbit,
            since: Timestamp(8),
        });
        let ruling = |id, kind| Ruling {
            id,
            question: "q".into(),
            pull_request: Some(30),
            kind,
            alerted: id.is_multiple_of(2),
        };
        state.rulings = vec![
            ruling(
                1,
                RulingKind::Merge {
                    head: "c0ffee".into(),
                },
            ),
            ruling(
                2,
                RulingKind::Rebase {
                    reason: "conflicts".into(),
                },
            ),
            ruling(
                3,
                RulingKind::StillRed {
                    head: "bad".into(),
                    checks: vec!["lint".into()],
                },
            ),
            ruling(4, RulingKind::Closed),
            ruling(
                5,
                RulingKind::Question {
                    asked: "Which name?".into(),
                    resume: Resume::Nothing,
                },
            ),
            ruling(6, RulingKind::TurnTimeout { phase: None }),
            ruling(
                7,
                RulingKind::ForeignChange {
                    description: "the `bug` label was added".into(),
                    known: Known {
                        labels: vec!["bug".into()],
                        ready: false,
                    },
                },
            ),
        ];
        state.last_ruling = 7;
        state.finished = vec![22, 30];
        state.reworked = vec!["PRR_1".into()];
        state.pacing = Some(DayStart {
            week_resets_at: Timestamp(9),
            day: 1,
            week_used_pct: 10,
        });
        store.save(&state).unwrap();
        let text = fs::read_to_string(dir.path().join("state.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let pinned = |id: u64, kind| {
            serde_json::json!({
                "id": id,
                "question": "q",
                "pull_request": 30,
                "kind": kind,
                "alerted": id.is_multiple_of(2),
            })
        };
        assert_eq!(
            value,
            serde_json::json!({
                "version": 1,
                "run": "paused",
                "since": 7,
                "work_item": null,
                "rulings": [
                    pinned(1, serde_json::json!({ "kind": "merge", "head": "c0ffee" })),
                    pinned(2, serde_json::json!({ "kind": "rebase", "reason": "conflicts" })),
                    pinned(3, serde_json::json!({ "kind": "still-red", "head": "bad", "checks": ["lint"] })),
                    pinned(4, serde_json::json!({ "kind": "closed" })),
                    pinned(
                        5,
                        serde_json::json!({
                            "kind": "question",
                            "asked": "Which name?",
                            "resume": { "state": "nothing" },
                        }),
                    ),
                    pinned(6, serde_json::json!({ "kind": "turn-timeout" })),
                    pinned(7, serde_json::json!({
                        "kind": "foreign-change",
                        "description": "the `bug` label was added",
                        "known": { "labels": ["bug"], "ready": false },
                    })),
                ],
                "last_ruling": 7,
                "finished": [22, 30],
                "reworked": ["PRR_1"],
                "leases": [{ "resource": "coderabbit", "since": 8 }],
                "pacing": { "week_resets_at": 9, "day": 1, "week_used_pct": 10 },
            })
        );
    }

    #[test]
    fn the_coderabbit_rulings_are_pinned() {
        let cap = RulingKind::CodeRabbitCap {
            rounds: 2,
            held: 1,
            prompt: "fix".into(),
            head: Some("c0ffee".into()),
        };
        let silent = RulingKind::CodeRabbitSilent {
            head: "c0ffee".into(),
        };
        let unpushed = RulingKind::FixNotPushed {
            fix: Fix::CodeRabbit {
                round: 3,
                head: "c0ffee".into(),
            },
            prompt: "again".into(),
        };
        for kind in [&cap, &silent, &unpushed] {
            let saved = serde_json::to_value(kind).unwrap();
            assert_eq!(&serde_json::from_value::<RulingKind>(saved).unwrap(), kind);
        }
        assert_eq!(
            serde_json::to_value([&cap, &silent, &unpushed]).unwrap(),
            serde_json::json!([
                {
                    "kind": "coderabbit-cap",
                    "rounds": 2,
                    "held": 1,
                    "prompt": "fix",
                    "head": "c0ffee",
                },
                { "kind": "coderabbit-silent", "head": "c0ffee" },
                {
                    "kind": "fix-not-pushed",
                    "coderabbit": { "round": 3, "head": "c0ffee" },
                    "prompt": "again",
                },
            ])
        );
        let saved_before_the_head: RulingKind = serde_json::from_value(serde_json::json!(
            { "kind": "coderabbit-cap", "rounds": 2, "held": 1, "prompt": "fix" }
        ))
        .unwrap();
        assert_eq!(
            saved_before_the_head,
            RulingKind::CodeRabbitCap {
                rounds: 2,
                held: 1,
                prompt: "fix".into(),
                head: None,
            }
        );
    }

    #[test]
    fn a_timeout_keeps_the_phase_its_turn_ran_in() {
        let kind = RulingKind::TurnTimeout {
            phase: Some(Phase::Implement),
        };
        let saved = serde_json::to_value(&kind).unwrap();
        assert_eq!(
            saved,
            serde_json::json!({ "kind": "turn-timeout", "phase": { "state": "implement" } })
        );
        assert_eq!(serde_json::from_value::<RulingKind>(saved).unwrap(), kind);
    }

    #[test]
    fn a_question_during_a_coderabbit_fix_is_pinned() {
        let resume = Resume::CodeRabbitFix {
            head: "c0ffee".into(),
        };
        let saved = serde_json::to_value(&resume).unwrap();
        assert_eq!(
            saved,
            serde_json::json!({ "state": "coderabbit-fix", "head": "c0ffee" })
        );
        assert_eq!(serde_json::from_value::<Resume>(saved).unwrap(), resume);
    }

    #[test]
    fn a_qwen_fix_not_pushed_keeps_its_wire_shape() {
        let saved = serde_json::json!({
            "kind": "fix-not-pushed",
            "review": {
                "round": 1,
                "consecutive_clean": 0,
                "guard_cleared": false,
                "stage": { "stage": "fixing", "clean": false, "head": "c0ffee" },
            },
            "prompt": "again",
        });
        let kind: RulingKind = serde_json::from_value(saved.clone()).unwrap();
        let RulingKind::FixNotPushed {
            fix: Fix::Review(review),
            ..
        } = &kind
        else {
            panic!("read as {kind:?}");
        };
        assert_eq!(review.round, 1);
        assert_eq!(serde_json::to_value(&kind).unwrap(), saved);
        let stray = serde_json::json!({
            "kind": "fix-not-pushed",
            "coderabbit": { "round": 3, "head": "c0ffee" },
            "prompt": "again",
            "extra": true,
        });
        assert!(serde_json::from_value::<RulingKind>(stray).is_err());
        let misspelt = serde_json::json!({
            "kind": "fix-not-pushed",
            "coderabbit": { "round": 3, "head": "c0ffee", "heade": "c0ffee" },
            "prompt": "again",
        });
        assert!(serde_json::from_value::<RulingKind>(misspelt).is_err());
    }

    #[test]
    fn a_state_saved_before_ruling_ids_were_counted_starts_counting_at_zero() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        fs::write(
            dir.path().join("state.json"),
            r#"{"version":1,"run":"running","since":3,"work_item":null,"rulings":[],"leases":[]}"#,
        )
        .unwrap();
        let state = store.load().unwrap().unwrap();
        assert_eq!((state.last_ruling, state.finished), (0, vec![]));
        assert!(state.reworked.is_empty());
    }

    #[test]
    fn a_ruling_saved_before_webhooks_is_posted_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let old = r#"{"version":1,"run":"running","since":7,"work_item":null,
            "rulings":[{"id":1,"question":"q","pull_request":3,"kind":{"kind":"closed"}}],
            "leases":[]}"#;
        fs::write(dir.path().join("state.json"), old).unwrap();
        assert!(!store.load().unwrap().unwrap().rulings[0].alerted);
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
                    "state::tests::writer_child",
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
