//! What a wake reads before it launches: the end of a red run's log for each
//! work item parked on a `StillRed` ruling, written into the project manager's
//! folder before it wakes, since it runs no `gh`, and the source's answer for
//! each standing line whose ticket end the board cannot answer
//!
//! The logs are read as a worker's red prompt reads them, with `failed_run_logs`.
//! The folder is emptied first, so a log never outlives its ruling. The forge
//! and the ticket source are asked with the runner's lock let go:
//! [`wake_off_lock`] runs them as a [`LogsJob`], and the files and the standing
//! notes are written under it from what they found.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::ports::{FailedLogs, Forge, ForgeError, Issue, Tickets};
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
        let ends = self.pm_stand_reads();
        if heads.is_empty() && runs.is_empty() && ends.is_empty() {
            return None;
        }
        Some(LogsJob {
            forge: Arc::clone(&self.ports.forge),
            tickets: Arc::clone(&self.ports.tickets),
            remote: self.remote.clone(),
            heads,
            runs,
            ends,
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
}

impl Logs {
    // What the read found of ticket `ticket`, or None when it read none
    pub(super) fn found(&self, ticket: TicketId) -> Option<&Result<Issue, ForgeError>> {
        self.issues.get(&ticket)
    }
}

impl LogsJob {
    // In the order the wake would have asked: each parked item's, then each red workflow's, then each standing end
    fn run(self) -> Logs {
        let Self {
            forge,
            tickets,
            remote,
            heads,
            runs,
            ends,
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
        Logs {
            failed,
            tails,
            issues,
        }
    }
}

fn short(head: &str) -> &str {
    head.get(..7).unwrap_or(head)
}
