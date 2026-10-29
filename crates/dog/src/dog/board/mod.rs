//! The agent board: every project's finished items, for the `pick` and `fit` asks
//!
//! Every [`LOAD_TICK`] the dog looks at each project's usage ledger in
//! kelpie's home and rereads one whose length or last write changed. A
//! `pick` ask ranks the implementers and reviewers it names with
//! `agent_board::pick`, each one's wait read from the
//! [`availability`](super::availability): the dog knows no project's day
//! start or kickoff hours, so a plan waits only on its 5-hour window. A
//! `fit` ask reads every project's records at once, and the dog keeps what
//! it has answered in its own marks file beside its book.

mod fit;
mod pick;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use tokio::time::MissedTickBehavior;

use super::availability::LOAD_TICK;
use crate::agents::{Agent, Agents, Role};
use crate::home::ProjectName;
use crate::settings::AgentName;
use crate::usage::{self, FinishedLine, Line};

pub(super) use fit::{load_marks, visit as fit_visit};
pub(super) use pick::visit;

/// Each agent and kind the dog answered a fit finding for, with the agent's
/// items of that kind when it did
pub(super) type Marks = BTreeMap<(AgentName, String), u32>;

/// The agents the board ranks, implementers and reviewers, and the finished items of every project
#[derive(Debug)]
pub struct Board {
    ranked: Arc<BTreeMap<AgentName, Agent>>,
    ledgers: BTreeMap<ProjectName, Ledger>,
    marks: Marks,
}

#[derive(Debug)]
struct Ledger {
    stamp: Stamp,
    finished: Arc<[FinishedLine]>,
}

// A ledger's length and last write, which change with every line it gains.
type Stamp = (u64, Option<SystemTime>);

/// What a pick, a `fit` ask or `status` reads of the board, copied out of its lock
#[derive(Debug, Clone, Default)]
pub struct View {
    ranked: Arc<BTreeMap<AgentName, Agent>>,
    ledgers: Vec<Arc<[FinishedLine]>>,
    marks: Marks,
}

impl View {
    // The agents the board ranks that are implementers
    fn implementers(&self) -> impl Iterator<Item = (&AgentName, &Agent)> {
        (self.ranked.iter()).filter(|(_, agent)| agent.role == Role::Implementer)
    }
}

impl Board {
    /// The implementers and reviewers among `agents`, with no ledger read yet
    pub(super) fn new(agents: &Agents) -> Self {
        let ranked = (agents.iter())
            .filter(|(_, agent)| ranks(agent.role))
            .map(|(name, agent)| (name.clone(), agent.clone()))
            .collect();
        Self {
            ranked: Arc::new(ranked),
            ledgers: BTreeMap::new(),
            marks: Marks::new(),
        }
    }

    /// What it holds now, for a pick, a `fit` ask or `status` to read without the lock
    pub(super) fn view(&self) -> View {
        View {
            ranked: Arc::clone(&self.ranked),
            ledgers: self
                .ledgers
                .values()
                .map(|l| Arc::clone(&l.finished))
                .collect(),
            marks: self.marks.clone(),
        }
    }
}

// Whether the board ranks an agent of `role`: an implementer builds an item
// and a reviewer reads one.
fn ranks(role: Role) -> bool {
    matches!(role, Role::Implementer | Role::Reviewer)
}

/// Rereads each project's ledger under `kelpie_home` that changed, and says how many it read
///
/// A project is a folder whose name is a project's and that holds a ledger.
/// The reads run outside the lock. A ledger that cannot be looked at or read
/// keeps its last lines, and only one that is gone leaves the board.
pub(super) fn refresh(board: &Mutex<Board>, kelpie_home: &Path) -> usize {
    let Some(found) = ledgers(kelpie_home) else {
        return 0;
    };
    let changed: Vec<_> = {
        let board = lock(board);
        (found.iter())
            .filter_map(|(project, seen)| Some((project, seen.as_ref()?)))
            .filter(|(project, (_, stamp))| {
                (board.ledgers.get(*project)).is_none_or(|ledger| ledger.stamp != *stamp)
            })
            .map(|(project, (path, stamp))| (project.clone(), path.clone(), *stamp))
            .collect()
    };
    let read: Vec<_> = (changed.into_iter())
        .filter_map(|(project, path, stamp)| {
            let finished = (usage::read(&path).ok()?.into_iter())
                .filter_map(|line| match line {
                    Line::Finished(finished) => Some(finished),
                    Line::Call(_) => None,
                })
                .collect();
            Some((project, Ledger { stamp, finished }))
        })
        .collect();
    let count = read.len();
    let mut board = lock(board);
    (board.ledgers).retain(|project, _| found.contains_key(project));
    board.ledgers.extend(read);
    count
}

// Each project with a ledger under `kelpie_home`: its path and stamp, or
// None when it cannot be looked at. None when the home cannot be listed.
// Only a plain file is read, so a FIFO left in its place cannot stall the dog.
fn ledgers(kelpie_home: &Path) -> Option<BTreeMap<ProjectName, Option<(PathBuf, Stamp)>>> {
    let mut found = BTreeMap::new();
    for entry in fs::read_dir(kelpie_home).ok()? {
        let entry = entry.ok()?;
        let Some(project) =
            (entry.file_name().to_str()).and_then(|n| ProjectName::try_from(n).ok())
        else {
            continue;
        };
        let path = entry.path().join(usage::FILE);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {
                let stamp = (meta.len(), meta.modified().ok());
                found.insert(project, Some((path, stamp)));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => _ = found.insert(project, None),
        }
    }
    Some(found)
}

/// The board of `agents`' implementers and reviewers, refreshed from the ledgers under `kelpie_home` from now on
pub(super) fn begin(agents: &Agents, kelpie_home: PathBuf) -> Arc<Mutex<Board>> {
    let mut board = Board::new(agents);
    load_marks(&mut board, &kelpie_home);
    let board = Arc::new(Mutex::new(board));
    tokio::spawn(keep(Arc::clone(&board), Arc::new(kelpie_home)));
    board
}

async fn keep(board: Arc<Mutex<Board>>, kelpie_home: Arc<PathBuf>) {
    let mut ticks = tokio::time::interval(LOAD_TICK);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let (board, home) = (Arc::clone(&board), Arc::clone(&kelpie_home));
        let _ = tokio::task::spawn_blocking(move || refresh(&board, &home)).await;
    }
}

/// The board, under its lock
///
/// Nothing holding it can leave it half changed, so a poisoned lock is fine.
pub(super) fn lock(board: &Mutex<Board>) -> MutexGuard<'_, Board> {
    board.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
pub(super) mod tests;
