//! What a wake reads before it launches: a red run's log end and each parked
//! item's issue, pull request and talk, written into the project manager's
//! folder, since it runs no `gh`, and the source's answer for each standing
//! line whose ticket end the board cannot answer
//!
//! The folder is emptied first, so a log never outlives its ruling. The forge
//! and the ticket source are asked with the runner's lock let go:
//! [`wake_off_lock`] runs them as a [`LogsJob`], and the files and the standing
//! notes are written under it from what they found.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::case::ended_red;
use crate::ports::{
    FailedLogs, Forge, ForgeError, HeadRun, Issue, LogPart, PullRequest, Talk, Tickets,
};
use crate::runner::Runner;
use crate::runner::trigger::lock;
use crate::settings::ForgeSlug;
use crate::state::{RulingKind, Stuck};
use crate::ticket::TicketId;
use crate::work_item::Phase;

/// The folder in the project manager's that holds the logs
const CI: &str = "ci";

impl Runner {
    /// Writes `ci/<issue>.log` in `folder` for each work item parked on a
    /// `StillRed` ruling, and returns what the board says of each
    ///
    /// A log that could not be read or written is noted there and in the log.
    pub(super) fn pm_ci_logs(&mut self, folder: &Path) -> BTreeMap<TicketId, String> {
        let ci = folder.join(CI);
        let cleared = match std::fs::remove_dir_all(&ci) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
        let mut said = BTreeMap::new();
        for (ticket, head) in self.still_red() {
            let file = format!("{CI}/{ticket}.log");
            let kept = (self.pm.logs.as_ref()).and_then(|logs| logs.failed.get(&head).cloned());
            let read = kept
                .unwrap_or_else(|| self.ports.forge.failed_run_logs(&self.remote, &head, false))
                .map(|logs| logs.text)
                .map_err(|e| format!("kelpie could not read it ({e})"))
                .and_then(|logs| match logs.trim().is_empty() {
                    true => Err(format!("kelpie found no failed run of {}", short(&head))),
                    false => Ok(logs),
                });
            let written = match (&cleared, read) {
                (Err(e), _) => Err(format!(
                    "cannot clear {}: {}, so any log left there is stale",
                    ci.display(),
                    e.kind()
                )),
                (Ok(()), Err(why)) => Err(why),
                (Ok(()), Ok(logs)) => std::fs::create_dir_all(&ci)
                    .and_then(|()| std::fs::write(folder.join(&file), logs))
                    .map_err(|e| format!("cannot write it: {}", e.kind())),
            };
            let line = match written {
                Ok(()) => format!("CI log `{file}`"),
                Err(why) => {
                    self.notes.push(format!(
                        "no CI log of {} for the project manager: {why}",
                        ticket.shown()
                    ));
                    format!("no CI log: {why}")
                }
            };
            said.insert(ticket, line);
        }
        said
    }
}

impl Runner {
    /// Writes `ci/main-<run>.log` in `folder` for each workflow red on `main`,
    /// and returns what the board says of each, for the section that names them
    ///
    /// Run after [`Runner::pm_ci_logs`], which empties the folder. A log that
    /// could not be read or written is noted there and in the log.
    pub(super) fn pm_main_logs(&mut self, folder: &Path) -> Vec<String> {
        let reds: Vec<(String, u64)> = (self.state.red.iter())
            .map(|r| (r.workflow.clone(), r.run))
            .collect();
        let mut said = Vec::new();
        for (workflow, run) in reds {
            let file = format!("{CI}/main-{run}.log");
            let kept = (self.pm.logs.as_ref()).and_then(|logs| logs.tails.get(&run).cloned());
            let read = kept
                .unwrap_or_else(|| self.ports.forge.run_log_tail(&self.remote, run))
                .map_err(|e| format!("kelpie could not read it ({e})"))
                .and_then(|log| match log.trim().is_empty() {
                    true => Err(format!("kelpie found no failed log of run {run}")),
                    false => Ok(log),
                });
            let written = read.and_then(|log| {
                std::fs::create_dir_all(folder.join(CI))
                    .and_then(|()| std::fs::write(folder.join(&file), log))
                    .map_err(|e| format!("cannot write it: {}", e.kind()))
            });
            let line = match written {
                Ok(()) => format!("CI log `{file}`"),
                Err(why) => {
                    self.notes.push(format!(
                        "no CI log of {workflow} on main for the project manager: {why}"
                    ));
                    format!("no CI log: {why}")
                }
            };
            said.push(format!("main, {workflow}: {line}"));
        }
        said
    }
}

impl Runner {
    // Each work item parked on a `StillRed` ruling, with the head that stayed red
    fn still_red(&self) -> Vec<(TicketId, String)> {
        (self.state.work_items.iter())
            .filter_map(|item| {
                let Phase::Ruling { id } = item.phase else {
                    return None;
                };
                match &self.state.rulings.iter().find(|r| r.id == id)?.kind {
                    RulingKind::Stuck(Stuck::StillRed { head, .. }) => {
                        Some((item.issue, head.clone()))
                    }
                    _ => None,
                }
            })
            .collect()
    }

    // The reads a wake needs before it launches, or none when no item is parked on red,
    // no workflow is red, and no standing line names a ticket the board cannot answer
    pub(super) fn pm_logs_job(&self) -> Option<LogsJob> {
        let heads: Vec<String> = self.still_red().into_iter().map(|(_, head)| head).collect();
        let runs: Vec<u64> = self.state.red.iter().map(|r| r.run).collect();
        let parked = self.parked_items().into_iter().filter(|p| !p.contribution);
        let parked: Vec<_> = parked.collect();
        let mut ends = self.pm_stand_reads();
        ends.extend(parked.iter().map(|p| p.ticket));
        ends.sort_unstable();
        ends.dedup();
        let mut pulls: Vec<u64> = parked.iter().filter_map(|p| p.pull).collect();
        pulls.sort_unstable();
        pulls.dedup();
        let open: BTreeSet<TicketId> = parked.iter().map(|p| p.ticket).collect();
        let kept = (self.pm.run_logs.keys())
            .filter(|(ticket, ..)| open.contains(ticket))
            .map(|&(_, run, attempt)| (run, attempt))
            .collect();
        if heads.is_empty() && runs.is_empty() && ends.is_empty() && pulls.is_empty() {
            return None;
        }
        Some(LogsJob {
            forge: Arc::clone(&self.ports.forge),
            tickets: Arc::clone(&self.ports.tickets),
            remote: self.remote.clone(),
            heads,
            runs,
            ends,
            pulls,
            kept,
        })
    }
}

/// Wakes the project manager, reading its CI logs and standing tickets with the runner's lock let go
///
/// The first try goes as far as the launch and, finding reads to make, hands them back
/// instead. They run with the lock let go, and the second try writes the files and the
/// standing notes from them and launches. Says whether a call started.
pub(in crate::runner) fn wake_off_lock(runner: &Mutex<Runner>) -> bool {
    let job = match lock(runner).wake_pm_or_reads() {
        Woke::Did(woke) => return woke,
        Woke::Reads(job) => job,
    };
    let read = job.run();
    let mut held = lock(runner);
    held.pm.logs = Some(read);
    // The drain, the pace and what the board shows are all checked again.
    let woke = matches!(held.wake_pm_or_reads(), Woke::Did(true));
    held.pm.logs = None;
    woke
}

/// What a wake's first try came to
pub(super) enum Woke {
    /// It ran as far as it goes, and a call started or did not
    Did(bool),
    /// What it would read before it launches is to be read first
    Reads(Box<LogsJob>),
}

/// What a wake reads before it launches, without the runner's lock
pub(super) struct LogsJob {
    forge: Arc<dyn Forge>,
    tickets: Arc<dyn Tickets>,
    remote: ForgeSlug,
    heads: Vec<String>,
    runs: Vec<u64>,
    // Each standing line's ticket end the board cannot answer
    ends: Vec<TicketId>,
    // Each parked item's pull request, whose state and talk its case holds
    pulls: Vec<u64>,
    // Each run attempt whose log a case already holds, which is not read again
    kept: BTreeSet<(u64, u32)>,
}

/// What a [`LogsJob`] read, errors as the inline call returns them
#[derive(Debug, Default)]
pub(super) struct Logs {
    // By head, as `failed_run_logs` of a pull request's runs read them
    failed: BTreeMap<String, Result<FailedLogs, ForgeError>>,
    // By run, the end of its failed log
    tails: BTreeMap<u64, Result<String, ForgeError>>,
    // By ticket, whether the source still holds it open
    issues: BTreeMap<TicketId, Result<Issue, ForgeError>>,
    // By number, a pull request as the forge holds it
    pulls: BTreeMap<u64, Result<PullRequest, ForgeError>>,
    // By number, what people wrote on a pull request
    talks: BTreeMap<u64, Result<Talk, ForgeError>>,
    // By head, every run on it
    head_runs: BTreeMap<String, Result<Vec<HeadRun>, ForgeError>>,
    // By run, its failed log in whole, for each failed run no case held yet
    run_logs: BTreeMap<(u64, u32), Result<String, ForgeError>>,
}

impl Logs {
    // What the read found of ticket `ticket`, or None when it read none
    pub(super) fn found(&self, ticket: TicketId) -> Option<&Result<Issue, ForgeError>> {
        self.issues.get(&ticket)
    }

    // What the read found of pull request `number`, or None when it read none
    pub(super) fn pull(&self, number: u64) -> Option<&Result<PullRequest, ForgeError>> {
        self.pulls.get(&number)
    }

    // What the read found of the runs on `head`, or None when it read none
    pub(super) fn head_runs(&self, head: &str) -> Option<&Result<Vec<HeadRun>, ForgeError>> {
        self.head_runs.get(head)
    }

    // What the read found of attempt `attempt` of run `run`'s log, or None when it read none
    pub(super) fn run_log(&self, run: u64, attempt: u32) -> Option<&Result<String, ForgeError>> {
        self.run_logs.get(&(run, attempt))
    }

    // What the read found of the talk on pull request `number`, or None when it read none
    pub(super) fn talk(&self, number: u64) -> Option<&Result<Talk, ForgeError>> {
        self.talks.get(&number)
    }
}

impl LogsJob {
    // In the order the wake would have asked: each parked item's, then each red workflow's, then each standing end, then each pull request
    fn run(self) -> Logs {
        let Self {
            forge,
            tickets,
            remote,
            heads,
            runs,
            ends,
            pulls,
            kept,
        } = self;
        // A `StillRed` ruling is raised only on a pull request's red run
        // (`ci_failed`), never on a merge queue removal, so not `merge_group`.
        let failed = (heads.into_iter())
            .map(|head| {
                let read = forge.failed_run_logs(&remote, &head, false);
                (head, read)
            })
            .collect();
        let tails = (runs.into_iter())
            .map(|run| (run, forge.run_log_tail(&remote, run)))
            .collect();
        let issues = (ends.into_iter())
            .map(|ticket| {
                let read = tickets.issue(&remote, ticket);
                (ticket, read)
            })
            .collect();
        let talks = (pulls.iter())
            .map(|&number| (number, forge.talk(&remote, number)))
            .collect();
        let pulls: BTreeMap<u64, _> = (pulls.into_iter())
            .map(|number| (number, forge.pull_request(&remote, number)))
            .collect();
        // Each head is read for its runs once, and each failed run's log once
        // however many cases name it, and not at all where a case holds it
        let heads: BTreeSet<&str> = (pulls.values().flatten())
            .map(|pull| pull.head.as_str())
            .collect();
        let mut head_runs = BTreeMap::new();
        let mut run_logs = BTreeMap::new();
        for head in heads {
            let listed = forge.head_runs(&remote, head);
            for run in listed.iter().flatten().filter(|r| ended_red(r)) {
                let key = (run.run, run.attempt);
                if !kept.contains(&key) && !run_logs.contains_key(&key) {
                    let read = forge.run_log(&remote, run.run, LogPart::Failed);
                    run_logs.insert(key, read);
                }
            }
            head_runs.insert(head.to_owned(), listed);
        }
        Logs {
            failed,
            tails,
            issues,
            pulls,
            talks,
            head_runs,
            run_logs,
        }
    }
}

fn short(head: &str) -> &str {
    head.get(..7).unwrap_or(head)
}
