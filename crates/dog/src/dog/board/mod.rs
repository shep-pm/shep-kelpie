//! The agent board: every project's finished items and API calls, for the `pick` and `fit` asks
//!
//! Every [`LOAD_TICK`] the dog rereads each project's usage ledger in
//! kelpie's home that changed, keeping its finished items and the priced
//! calls of agents that bill an API key. A `pick` ask ranks the agents it
//! names with `agent_board::pick`, each one's wait read from the
//! [`availability`](super::availability): the dog knows no project's day
//! start or kickoff hours, so a plan waits only on its 5-hour window. A
//! `fit` ask reads every project's records at once, and the dog keeps what
//! it has answered in its own marks file beside its book.

mod fit;
mod pick;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use tokio::time::MissedTickBehavior;

use super::availability::LOAD_TICK;
use crate::agents::{Agent, Agents, Role};
use crate::home::ProjectName;
use crate::ports::Timestamp;
use crate::settings::AgentName;
use crate::usage::{self, FinishedLine, Line};
use crate::webhook::ApiCredit;

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
    credit: Option<ApiCredit>,
    // Every agent whose file bills an API key, whatever its role
    api_agents: Arc<BTreeSet<String>>,
}

#[derive(Debug)]
struct Ledger {
    stamp: Stamp,
    finished: Arc<[FinishedLine]>,
    api_calls: Arc<[ApiCall]>,
}

// A priced call of an API agent
#[derive(Debug, Clone, Copy)]
struct ApiCall {
    at: Timestamp,
    cost_usd: f64,
}

// A ledger's length and last write, which change with every line it gains.
type Stamp = (u64, Option<SystemTime>);

/// What a pick, a `fit` ask or `status` reads of the board, copied out of its lock
#[derive(Debug, Clone, Default)]
pub struct View {
    ranked: Arc<BTreeMap<AgentName, Agent>>,
    ledgers: Vec<Arc<[FinishedLine]>>,
    marks: Marks,
    credit: Option<ApiCredit>,
    api_agents: Arc<BTreeSet<String>>,
    api_calls: Vec<Arc<[ApiCall]>>,
}

impl View {
    /// The credit that caps API spend, if one is set
    pub fn credit(&self) -> Option<ApiCredit> {
        self.credit
    }

    /// What the API agents' priced calls cost in the credit's window at `now`, across
    /// every project's ledger; 0 when no credit is set
    pub fn api_spent(&self, now: Timestamp) -> f64 {
        let Some(credit) = self.credit else {
            return 0.0;
        };
        let (start, end) = credit.window(now);
        (self.api_calls.iter().flat_map(|calls| calls.iter()))
            .filter(|call| start <= call.at && call.at < end)
            .map(|call| call.cost_usd)
            .sum()
    }

    // Whether `name` is an agent whose file bills an API key
    fn bills_api(&self, name: &str) -> bool {
        self.api_agents.contains(name)
    }

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
        let api_agents = (agents.iter())
            .filter(|(_, agent)| {
                (agent.runs.session()).is_some_and(|(model, _)| model.harness.bills_api())
            })
            .map(|(name, _)| name.to_string())
            .collect();
        Self {
            ranked: Arc::new(ranked),
            ledgers: BTreeMap::new(),
            marks: Marks::new(),
            credit: None,
            api_agents: Arc::new(api_agents),
        }
    }

    /// The same board capping API spend at `credit`
    pub(super) fn credited(self, credit: Option<ApiCredit>) -> Self {
        Self { credit, ..self }
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
            credit: self.credit,
            api_agents: Arc::clone(&self.api_agents),
            api_calls: (self.ledgers.values())
                .map(|l| Arc::clone(&l.api_calls))
                .collect(),
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
    let (changed, api_agents): (Vec<_>, _) = {
        let board = lock(board);
        let changed = (found.iter())
            .filter_map(|(project, seen)| Some((project, seen.as_ref()?)))
            .filter(|(project, (_, stamp))| {
                (board.ledgers.get(*project)).is_none_or(|ledger| ledger.stamp != *stamp)
            })
            .map(|(project, (path, stamp))| (project.clone(), path.clone(), *stamp))
            .collect();
        (changed, Arc::clone(&board.api_agents))
    };
    let read: Vec<_> = (changed.into_iter())
        .filter_map(|(project, path, stamp)| {
            let (mut finished, mut api_calls) = (Vec::new(), Vec::new());
            for line in usage::read(&path).ok()? {
                match line {
                    Line::Finished(line) => finished.push(line),
                    Line::Call(call) => api_calls.extend(api_call(&call, &api_agents)),
                }
            }
            Some((
                project,
                Ledger {
                    stamp,
                    finished: finished.into(),
                    api_calls: api_calls.into(),
                },
            ))
        })
        .collect();
    let count = read.len();
    let mut board = lock(board);
    (board.ledgers).retain(|project, _| found.contains_key(project));
    board.ledgers.extend(read);
    count
}

// The call, if an API agent made it and it has a price
fn api_call(call: &usage::CallLine, api_agents: &BTreeSet<String>) -> Option<ApiCall> {
    let agent = call.agent.as_deref()?;
    Some(ApiCall {
        at: call.at,
        cost_usd: api_agents.contains(agent).then_some(call.cost_usd)??,
    })
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

/// The board of `agents`' implementers and reviewers, capping API spend at `credit`, refreshed from the ledgers under `kelpie_home` from now on
pub(super) fn begin(
    agents: &Agents,
    kelpie_home: PathBuf,
    credit: Option<ApiCredit>,
) -> Arc<Mutex<Board>> {
    let mut board = Board::new(agents).credited(credit);
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
