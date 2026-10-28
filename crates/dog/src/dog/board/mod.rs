//! The agent board: every project's finished items, for the `pick` ask
//!
//! Every [`LOAD_TICK`] the dog looks at each project's usage ledger in
//! kelpie's home and rereads one whose length or last write changed. A
//! `pick` ask ranks the implementers it names with `agent_board::pick`, each
//! one's wait read from the [`availability`](super::availability). The dog
//! knows no project's day start or kickoff hours, so a plan waits only on its
//! 5-hour window, and the runner's pacer still holds at dispatch.

mod pick;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use tokio::time::MissedTickBehavior;

use super::availability::LOAD_TICK;
use crate::agents::{Agent, Agents, Role};
use crate::home::ProjectName;
use crate::settings::AgentName;
use crate::usage::{self, FinishedLine, Line};

pub(super) use pick::visit;

/// The implementers the board ranks, and the finished items of every project
#[derive(Debug)]
pub struct Board {
    implementers: Arc<BTreeMap<AgentName, Agent>>,
    ledgers: BTreeMap<ProjectName, Ledger>,
}

#[derive(Debug)]
struct Ledger {
    stamp: Stamp,
    finished: Arc<[FinishedLine]>,
}

// A ledger's length and last write, which change with every line it gains.
type Stamp = (u64, Option<SystemTime>);

/// What a pick or `status` reads of the board, copied out of its lock
#[derive(Debug, Clone, Default)]
pub struct View {
    implementers: Arc<BTreeMap<AgentName, Agent>>,
    ledgers: Vec<Arc<[FinishedLine]>>,
}

impl Board {
    /// The implementers among `agents`, with no ledger read yet
    pub(super) fn new(agents: &Agents) -> Self {
        let implementers = (agents.iter())
            .filter(|(_, agent)| agent.role == Role::Implementer)
            .map(|(name, agent)| (name.clone(), agent.clone()))
            .collect();
        Self {
            implementers: Arc::new(implementers),
            ledgers: BTreeMap::new(),
        }
    }

    /// What it holds now, for a pick to read without the lock
    pub(super) fn view(&self) -> View {
        View {
            implementers: Arc::clone(&self.implementers),
            ledgers: self
                .ledgers
                .values()
                .map(|l| Arc::clone(&l.finished))
                .collect(),
        }
    }
}

/// Rereads each project's ledger under `kelpie_home` that changed, and says how many it read
///
/// A project is a folder whose name is a project's and that holds a ledger.
/// The reads run outside the lock. A ledger that cannot be read keeps its
/// last lines, and the next refresh tries it again.
pub(super) fn refresh(board: &Mutex<Board>, kelpie_home: &Path) -> usize {
    let found = ledgers(kelpie_home);
    let changed: Vec<_> = {
        let board = lock(board);
        (found.iter())
            .filter(|(project, _, stamp)| {
                (board.ledgers.get(project)).is_none_or(|ledger| ledger.stamp != *stamp)
            })
            .cloned()
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
    (board.ledgers).retain(|project, _| found.iter().any(|(there, ..)| there == project));
    board.ledgers.extend(read);
    count
}

// Each project's ledger under `kelpie_home`, with its stamp. Only a plain
// file is read, so a FIFO left in its place cannot stall the dog.
fn ledgers(kelpie_home: &Path) -> Vec<(ProjectName, PathBuf, Stamp)> {
    let Ok(entries) = fs::read_dir(kelpie_home) else {
        return Vec::new();
    };
    (entries.flatten())
        .filter_map(|entry| {
            let project = ProjectName::try_from(entry.file_name().to_str()?).ok()?;
            let path = entry.path().join(usage::FILE);
            let meta = fs::symlink_metadata(&path)
                .ok()
                .filter(fs::Metadata::is_file)?;
            Some((project, path, (meta.len(), meta.modified().ok())))
        })
        .collect()
}

/// The board of `agents`' implementers, refreshed from the ledgers under `kelpie_home` from now on
pub(super) fn begin(agents: &Agents, kelpie_home: PathBuf) -> Arc<Mutex<Board>> {
    let board = Arc::new(Mutex::new(Board::new(agents)));
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
