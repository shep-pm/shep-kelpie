//! Stand-ins for the ports and the helpers the tests of kelpie's crates share
//!
//! Each names only this crate and the leaves below it. The rig that wires them
//! into a runner is in the root, which also holds the stand-ins that name an adapter.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::ports::{
    Clock, Cost, Meter, MeterError, Role, SessionId, Timestamp, Usage, Utilization, Window,
};
use crate::work_item::{CallRecord, Known, Phase, Seconds, TimingPhase, Timings, Turn, WorkItem};
use forge::{At, Gate};

mod agent_board;
mod alerts;
mod bridge;
mod classifier;
mod coderabbit;
mod codex;
mod cubic;
mod dev_server;
mod disk;
mod e2e_runner;
mod endpoint;
mod forge;
mod git;
mod hold;
mod home;
mod leased;
mod memory;
mod pages;
mod samples;
mod sandbox;
mod shepherd;
mod test_runner;

pub use agent_board::FakeAgentBoard;
pub use alerts::FakeAlerts;
pub use bridge::FakeBridges;
pub use classifier::{FakeClassifier, SeenAsk};
pub use coderabbit::FakeCodeRabbit;
pub use dev_server::FakeDevServers;
pub use disk::{FakeDisk, GB};
pub use e2e_runner::FakeE2e;
pub use endpoint::{Answer, StandInEndpoint, Take, unreachable_url};
pub use forge::{AppComments, FakeForge, HeldCall, LOGIN_TOKEN, VIEWER};
pub use git::{FakeGit, Synced};
pub use hold::Hold;
pub use home::Home;
pub use leased::LeaseReads;
pub use memory::Memory;
pub use pages::{FakePages, clean as clean_visit};
pub use samples::RecordedSink;
pub use sandbox::OpenSandbox;
#[cfg(test)]
pub use shep_kelpie_test_support::{
    CALL_PATIENCE, Fake, git, linked_worktree, short_tempdir, write_fake, write_in, write_script,
};
pub use shepherd::FakeShepherd;
pub use test_runner::FakeTests;

/// Where every fake clock starts, in seconds since the Unix epoch
pub const EPOCH: u64 = 1_790_000_000;

/// One day, in seconds
pub const DAY: u64 = 86_400;

/// Usage with `week` and `session` percent spent. The week began at [`EPOCH`]
/// and the session window resets five hours on.
pub fn utilization(week: u32, session: u32) -> Utilization {
    Utilization {
        session: Window {
            used_pct: session,
            resets_at: Timestamp(EPOCH + 5 * 3600),
        },
        week: Window {
            used_pct: week,
            resets_at: Timestamp(EPOCH + 7 * DAY),
        },
    }
}

/// [`a_work_item`] as an older file's loads: the comments on its pull request
/// are marked as seen
pub fn a_work_item_from_before_comments() -> WorkItem {
    let mut item = a_work_item();
    item.talked.quiet = true;
    item
}

/// A work item with one call, so every field of its format shows
pub fn a_work_item() -> WorkItem {
    WorkItem {
        pinned: true,
        turn: Turn::Running {
            since: Timestamp(9),
            prompt: None,
        },
        pull_request: Some(51),
        phase: Phase::Ci {
            head: Some("c0ffee".into()),
            since: Timestamp(11),
        },
        red_head: Some("bad".into()),
        conflict: Some(crate::work_item::Conflict {
            head: "c0ffee".into(),
            main: "a11ce".into(),
            turns: 1,
        }),
        known: Known {
            labels: vec!["review please".into()],
            ready: false,
            head: None,
        },
        timings: Some(Timings {
            created: Timestamp(5),
            since: Timestamp(12),
            seconds: Seconds::of(&[(TimingPhase::Worker, 4), (TimingPhase::Ci, 3)]),
            call: None,
            queued: false,
        }),
        counts: crate::work_item::Counts {
            review_rounds: 2,
            fix_turns: 1,
            rulings: 1,
            ci_fix_turns: 1,
            disputed: 0,
            deferred: 0,
            outcomes_counted: 0,
            worker_unreported: 0,
            reviewer_unreported: 0,
            ci_failures: crate::work_item::CiFailures::default(),
        },
        calls: vec![CallRecord {
            role: Role::Worker,
            at: Timestamp(10),
            session: SessionId("5e55".into()),
            usage: Usage {
                input: 1,
                cache_write: 2,
                cache_write_5m: 0,
                cache_read: 3,
                output: 4,
            },
            cost: Cost(5),
            session_cost: Cost(6),
            unpriced: false,
        }],
        ..WorkItem::new(
            ticket(42),
            "Add a thing",
            "kelpie/42",
            "/k/wt/shep/42".into(),
            "/k/targets/shep/42".into(),
            "opus-high".to_owned().try_into().unwrap(),
            SessionId("5e55".into()),
        )
    }
}

/// The `[app.dogs.kelpie]` table of a runner's Flockfile entry, such as
/// `settings.example.toml`, as shep hands it to the runner
pub fn project_table(entry: &str) -> serde_json::Map<String, serde_json::Value> {
    let parsed: toml::Table = toml::from_str(entry).unwrap();
    match serde_json::to_value(&parsed["app"][0]["dogs"]["kelpie"]).unwrap() {
        serde_json::Value::Object(table) => table,
        other => panic!("the entry's kelpie table is {other}"),
    }
}

/// A path quoted and escaped as a TOML string value
pub fn toml_path(path: &std::path::Path) -> String {
    toml::Value::String(path.to_string_lossy().into_owned()).to_string()
}

#[test]
fn a_path_in_toml_reads_back_with_backslashes_and_quotes_intact() {
    use std::path::Path;

    for path in [
        r"C:\Users\runner\a repo",
        r#"C:\Users\runner\a "quoted" repo"#,
        "/home/runner/a repo",
    ] {
        let path = Path::new(path);
        let text = format!("[git]\ncheckout = {}\n", toml_path(path));
        let table: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(Path::new(table["git"]["checkout"].as_str().unwrap()), path);
    }
}

/// A runner entry like `settings.example.toml` with `tables` added to its
/// kelpie table, such as an older `[app.dogs.kelpie.review]`
pub fn with_tables(entry: &str, tables: &str) -> String {
    const PACING: &str = "\n[app.dogs.kelpie.pacing]\n";
    assert!(entry.contains(PACING), "the example's pacing table moved");
    entry.replace(PACING, &format!("\n{tables}{PACING}"))
}

/// A meter that reports what a test sets, and counts its reads
///
/// It starts with the account idle: nothing used in either window, the
/// week [`EPOCH`] begins and the session window five hours long.
#[derive(Debug, Clone)]
pub struct FakeMeter {
    reading: Arc<Mutex<Result<Utilization, MeterError>>>,
    reads: Arc<AtomicUsize>,
    hold: Arc<Mutex<Option<Gate>>>,
}

impl FakeMeter {
    /// A meter that reads no use
    pub fn idle() -> Self {
        Self {
            reading: Arc::new(Mutex::new(Ok(utilization(0, 0)))),
            reads: Arc::default(),
            hold: Arc::default(),
        }
    }

    /// Reports `usage` from now on
    pub fn set(&self, usage: Utilization) {
        *self.reading.lock().unwrap() = Ok(usage);
    }

    /// Fails every read with `error`, until a test sets a reading
    pub fn fail(&self, error: MeterError) {
        *self.reading.lock().unwrap() = Err(error);
    }

    /// How many times usage was read
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    /// Holds the next read open until the test drops what this returns
    pub fn hold_next_read(&self) -> HeldCall {
        let (gate, held) = Gate::open(At::Any);
        *self.hold.lock().unwrap() = Some(gate);
        held
    }
}

impl Meter for FakeMeter {
    fn read(&self, _now: Timestamp) -> Result<Utilization, MeterError> {
        let gate = self.hold.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.pass();
        }
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.reading.lock().unwrap().clone()
    }
}

/// A clock that moves only when a test moves it
#[derive(Debug, Clone)]
pub struct FakeClock(Arc<AtomicU64>);

impl FakeClock {
    /// A clock that stands at `seconds` past the epoch
    pub fn at(seconds: u64) -> Self {
        Self(Arc::new(AtomicU64::new(seconds)))
    }

    /// Moves the clock on by `seconds`
    pub fn advance(&self, seconds: u64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.0.load(Ordering::SeqCst))
    }

    fn wait(&self, seconds: u64) {
        self.advance(seconds);
    }
}

/// The ticket of GitHub issue `number`
pub fn ticket(number: u64) -> crate::ticket::TicketId {
    crate::ticket::TicketId::from_issue(number).expect("a test's issue is not zero")
}
