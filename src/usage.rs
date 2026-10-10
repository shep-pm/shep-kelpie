//! The usage ledger: every model call a project makes, in the control
//! room's units
//!
//! `<kelpie home>/<project>/usage.jsonl` holds one JSON object per line,
//! appended and never rewritten: a `call` line as each call ends, the
//! worker's turns, every reviewer's sessions and local rounds, the project
//! manager's wakes and compactions and the issue writer's runs, and a
//! `finished` line as a work item finishes. A call's line keeps its four
//! token counts beside its units, so another weighting can be applied
//! later. `shep kelpie usage` reads it, and nothing else does.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ports::{Cost, Role, SessionId, Timestamp, Usage};
use crate::work_item::{QwenTally, RoleTally, Tally};

pub mod baselines;
mod draft;
mod import;
mod report;

pub use draft::{Draft, Spent, ended};
pub use import::{Imported, import};
pub use report::report;

/// The ledger's file name in a project's folder
pub const FILE: &str = "usage.jsonl";

/// A call's tokens in the control room's units, rounded to the nearest
///
/// Weighted by cache pricing, as the design log measured the control room:
/// uncached input 1, a one-hour cache write 2, a five-minute one 1.25, a
/// cache read 0.1, output 5.
pub fn units(usage: Usage) -> u64 {
    let five_minutes = usage.cache_write_5m.min(usage.cache_write);
    let hundredths = u128::from(usage.input) * 100
        + u128::from(usage.cache_write - five_minutes) * 200
        + u128::from(five_minutes) * 125
        + u128::from(usage.cache_read) * 10
        + u128::from(usage.output) * 500;
    u64::try_from((hundredths + 50) / 100).unwrap_or(u64::MAX)
}

/// One line of the ledger
// wire format: changing this is a breaking change to the usage ledger
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "line", rename_all = "kebab-case")]
pub enum Line {
    /// A model call ended
    Call(CallLine),
    /// A work item finished
    Finished(FinishedLine),
}

/// What a call was
// wire format: changing this is a breaking change to the usage ledger
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallKind {
    /// A worker's turn
    Turn,
    /// A reviewer's session, a first or second look
    Review,
    /// A local reviewer's round, on the GPU or an endpoint
    LocalRound,
    /// The project manager's wake
    Wake,
    /// `/compact` on the project manager's session
    Compact,
    /// The issue writer's run
    Issues,
}

/// How a call ended
// wire format: changing this is a breaking change to the usage ledger
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ended {
    /// It answered
    Answered,
    /// It came back, with an answer kelpie could not read
    Unreadable,
    /// The harness failed, and reported no usage
    Failed,
    /// It ran past its ceiling and was ended
    TimedOut,
    /// It was ended because the runner was stopping
    Stopped,
}

/// One account's usage as the pacer last read it
// wire format: changing this is a breaking change to the usage ledger
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacerLine {
    /// When it was read
    pub at: Timestamp,
    /// Percent of the 5-hour window used
    pub session_pct: u32,
    /// When the 5-hour window resets
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_resets_at: Option<Timestamp>,
    /// Percent of the weekly window used
    pub week_pct: u32,
    /// When the weekly window resets, so two readings tell whether a reset
    /// fell between them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub week_resets_at: Option<Timestamp>,
}

/// A model call, as it ended
// wire format: changing this is a breaking change to the usage ledger
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallLine {
    /// When it ended
    pub at: Timestamp,
    /// The issue of the work item it was for, if any
    pub issue: Option<u64>,
    /// The work item's pull request, once it had one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<u64>,
    /// The role it was made for
    pub role: Role,
    /// What it was
    pub kind: CallKind,
    /// The agent it ran on, unknown on an imported line
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The agent's harness, or `local` for a local round
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// The model, where kelpie names one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// How hard the model thought, where kelpie sets it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The session it ran in; none for a local round
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The tokens it used, as its harness reported them
    pub usage: Usage,
    /// Those tokens in the control room's units
    pub units: u64,
    /// What it cost, in US dollars, when its harness reported a cost
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Whether its harness reported no cost
    #[serde(default, skip_serializing_if = "is_false")]
    pub unpriced: bool,
    /// What its session had cost by its end, in US dollars, when reported
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_cost_usd: Option<f64>,
    /// Seconds from its start to its end, unknown on an imported line
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
    /// How it ended
    pub ended: Ended,
    /// Seconds a local round ran on the GPU, its queue left out
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_seconds: Option<u64>,
    /// Each account's last reading by the pacer before it ended, by name
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pacer: BTreeMap<String, PacerLine>,
    /// Whether it was read from a runner's log, not written as it ended
    #[serde(default, skip_serializing_if = "is_false")]
    pub imported: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

impl CallLine {
    /// Whether it ran in `session` and ended within `slack` seconds of `at`
    fn same_call(&self, session: &str, at: Timestamp, slack: u64) -> bool {
        self.session.as_deref() == Some(session) && self.at.0.abs_diff(at.0) <= slack
    }
}

/// What one role spent on a finished work item
// wire format: changing this is a breaking change to the usage ledger
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RoleLine {
    /// How many calls it made
    pub calls: u32,
    /// The tokens they used, all together
    pub tokens: Usage,
    /// Those tokens in the control room's units
    pub units: u64,
    /// What the calls that reported a cost cost, in US dollars
    pub cost_usd: f64,
    /// Calls whose harness reported no cost
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unpriced_calls: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl From<RoleTally> for RoleLine {
    fn from(role: RoleTally) -> Self {
        Self {
            calls: role.calls,
            tokens: role.tokens,
            units: role.units,
            cost_usd: role.cost.usd(),
            unpriced_calls: role.unpriced_calls,
        }
    }
}

/// A finished work item, merged, closed with no change or dropped, and what
/// it spent
// wire format: changing this is a breaking change to the usage ledger
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinishedLine {
    /// When it finished
    pub at: Timestamp,
    /// The issue it resolved
    pub issue: u64,
    /// The issue's title
    pub title: String,
    /// Its pull request, if it had one
    pub pull_request: Option<u64>,
    /// Whether the pull request merged
    pub merged: bool,
    /// Whether its issue was closed with no change, so it ended with no
    /// pull request
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
    /// Seconds from its creation to `at`
    pub wall: u64,
    /// Every role's units together
    pub units: u64,
    /// Every role's priced calls together, in US dollars
    pub cost_usd: f64,
    /// The worker's turns
    pub worker: RoleLine,
    /// The reviewers' sessions
    pub reviewer: RoleLine,
    /// Its local rounds and their seconds
    pub qwen: QwenTally,
    /// Reviewers' rounds that came back with an answer
    pub review_rounds: u32,
    /// Worker turns sent to fix a round's held findings
    pub fix_turns: u32,
    /// Rulings raised for it
    pub rulings: u32,
}

impl FinishedLine {
    /// The line for a work item that finished as `record` says, spending `tally`
    pub fn of(record: &crate::state::Finished, tally: &Tally) -> Self {
        Self {
            at: record.at,
            issue: record.issue,
            title: record.title.clone(),
            pull_request: record.pull_request,
            merged: record.merged,
            closed: record.closed,
            wall: record.wall,
            units: tally.units(),
            cost_usd: tally.cost().usd(),
            worker: tally.worker.into(),
            reviewer: tally.reviewer.into(),
            qwen: tally.qwen,
            review_rounds: tally.counts.review_rounds,
            fix_turns: tally.counts.fix_turns,
            rulings: tally.counts.rulings,
        }
    }
}

/// A project's ledger, which a runner appends to as its calls end
#[derive(Debug)]
pub struct Ledger {
    path: PathBuf,
    // Each session's cost as its last line carried it, read from the file
    // the first time one is asked for
    costs: Option<BTreeMap<String, Cost>>,
    // Whether the last append failed, so a run of failures is told once
    failing: bool,
}

impl Ledger {
    /// The ledger in the project folder `folder`
    pub fn in_folder(folder: &Path) -> Self {
        Self {
            path: folder.join(FILE),
            costs: None,
            failing: false,
        }
    }

    /// Appends `line`. A failure is told on stderr, once until an append
    /// works again, and goes no further: no call or step fails on it.
    pub fn append(&mut self, line: &Line) {
        match append_to(&self.path, line) {
            // Only a line that is in the file moves its session's cost on,
            // so the next line's cost takes in a lost one's.
            Ok(()) => {
                self.failing = false;
                if let (Line::Call(call), Some(costs)) = (line, self.costs.as_mut())
                    && let (Some(session), Some(cost)) = (&call.session, call.session_cost_usd)
                    && let Some(cost) = Cost::from_usd(cost)
                {
                    costs.insert(session.clone(), cost);
                }
            }
            Err(e) if !self.failing => {
                self.failing = true;
                eprintln!(
                    "cannot add to the usage ledger {}, so calls go unrecorded there \
                     until it can be written: {e}",
                    self.path.display()
                );
            }
            Err(_) => {}
        }
    }

    /// Where the ledger is
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What `session` had cost by its last line, or `None` with no line
    /// that says
    pub fn session_cost(&mut self, session: &SessionId) -> Option<Cost> {
        let path = &self.path;
        if self.costs.is_none() {
            // Not kept on an error, so the next call reads the file again.
            let lines = read(path).ok()?;
            let mut costs = BTreeMap::new();
            for line in lines {
                if let Line::Call(call) = line
                    && let (Some(session), Some(cost)) = (call.session, call.session_cost_usd)
                    && let Some(cost) = Cost::from_usd(cost)
                {
                    costs.insert(session, cost);
                }
            }
            self.costs = Some(costs);
        }
        self.costs.as_ref()?.get(&session.0).copied()
    }
}

/// Appends `line` to the ledger at `path`, making its folder if need be
///
/// The line goes in one write to a file opened for appending, so a runner
/// and an issue writer adding at once never interleave within a line. A
/// line a crash left torn is ended first, so it costs only itself.
///
/// # Errors
///
/// The [`io::Error`] that opening, reading or writing the file failed with.
pub fn append_to(path: &Path, line: &Line) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(line).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    let mut file = (OpenOptions::new().create(true).read(true).append(true)).open(path)?;
    let length = file.metadata()?.len();
    if length > 0 {
        let mut last = [0u8];
        file.seek(SeekFrom::Start(length - 1))?;
        file.read_exact(&mut last)?;
        if last != *b"\n" {
            bytes.insert(0, b'\n');
        }
    }
    file.write_all(&bytes)
}

/// Every line of the ledger at `path` that reads as one, oldest first: a
/// line torn by a crash, even mid-character, or of a kind a newer kelpie
/// wrote, is passed over
///
/// # Errors
///
/// The [`io::Error`] reading the file failed with, apart from its absence,
/// which reads as no lines.
pub fn read(path: &Path) -> io::Result<Vec<Line>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(bytes
        .split(|&b| b == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect())
}

#[cfg(test)]
mod tests;
