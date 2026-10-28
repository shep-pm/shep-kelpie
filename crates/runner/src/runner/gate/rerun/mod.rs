//! A red run whose log names no file the branch changed is rerun once
//!
//! A flaky test outside the branch fails a run the worker cannot fix. Its
//! credential cannot rerun a job. Kelpie reads the failed runs' logs as the
//! login `gh` acts as. When none names a file the branch changed, kelpie
//! reruns its failed jobs, once, before it spends a turn. A head that fails
//! again goes to the worker with the log, saying so. Anything kelpie cannot
//! read or rerun leaves the red run to the worker. A still-red ruling offers
//! the same rerun as an option, for the maintainer or the project manager to
//! choose.

use super::super::ci_help::Logs;
use crate::runner::Runner;
use crate::runner::report::{Begin, StepReport};
use crate::state::StateError;
use crate::work_item::Phase;
use crate::worktree;

impl Runner {
    /// Whether a still-red ruling at `head` offers to rerun its failed jobs
    /// once more: their logs name no file the branch changed, and no run was
    /// ended by its timeout, which a rerun would end again
    pub(super) fn rerun_offered(&self, head: &str, logs: &Logs) -> bool {
        match logs {
            Logs::Read(logs) => logs.timed_out == 0 && self.unrelated(head, &logs.text),
            Logs::Refused(_) => false,
        }
    }

    // Whether `logs` name no file the branch changed at `head`
    fn unrelated(&self, head: &str, logs: &str) -> bool {
        let base = self
            .current()
            .map_or(self.settings.git.base(), |i| self.item_base(i));
        let Ok(changed) = worktree::changed_files(&self.settings.git.checkout, head, base) else {
            return false;
        };
        let related = logs.trim().is_empty() || changed.is_empty() || names_any(logs, &changed);
        !related
    }

    /// Reruns the failed jobs of `head`, instead of a fix turn, when its logs
    /// name no file the branch changed and it has not been rerun
    pub(super) fn rerun_if_unrelated(
        &mut self,
        number: u64,
        head: &str,
        checks: &[String],
        read: &Logs,
    ) -> Result<Option<Begin>, StateError> {
        let item = self.current().expect("CI runs on a work item");
        if item.reran.as_deref() == Some(head) {
            return Ok(None);
        }
        let issue = item.issue;
        let Logs::Read(logs) = read else {
            return Ok(None);
        };
        if !self.rerun_offered(head, read) {
            return Ok(None);
        }
        self.journal_outside_diff(issue, &logs.text);
        if self.ports.forge.rerun_failed(&self.remote, head).is_err() {
            return Ok(None);
        }
        // The checks read red for a moment after a rerun starts, so the
        // verdict waits out the same time a new head's checks do.
        let now = self.ports.clock.now();
        let reran = head.to_owned();
        self.update(|item| {
            item.reran = Some(reran);
            match &mut item.phase {
                Phase::Ci { since, .. } => *since = now,
                Phase::Review(review) => {
                    if let Some(ci) = review.ci.as_deref_mut() {
                        ci.since = now;
                    }
                }
                _ => {}
            }
        })?;
        Ok(Some(Begin::Report(StepReport::CiRerun {
            issue,
            pull_request: number,
            head: head.to_owned(),
            checks: checks.to_vec(),
        })))
    }
}

// Whether `log` mentions any of `files`, by the path the branch changes it at
fn names_any(log: &str, files: &[String]) -> bool {
    files.iter().any(|file| log.contains(file.as_str()))
}

#[cfg(test)]
mod tests;
