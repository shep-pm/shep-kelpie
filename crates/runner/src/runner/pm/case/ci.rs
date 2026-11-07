//! A case's CI part: every run on the pull request's head, then the whole log of each that failed
//!
//! A completed run's log does not change, so the runner keeps each one it read
//! in memory, by item and run, and a wake reads only the logs it lacks. The
//! list is read at every wake, since runs start and end. A green run's log is
//! not read: the project manager asks for it when it needs it.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::{cause, plain};
use crate::ports::{ForgeError, HEAD_RUNS_LIMIT, HeadRun, LogPart};
use crate::runner::Runner;
use crate::ticket::TicketId;

// How a run concludes when it failed: its log is read. A job `timeout-minutes`
// stops ends `cancelled`, and a run can also conclude `timed_out`.
const FAILED: [&str; 3] = ["failure", "timed_out", "cancelled"];

/// Whether `run` ended red, so its log is kept in the case
pub(in crate::runner::pm) fn ended_red(run: &HeadRun) -> bool {
    run.conclusion
        .as_deref()
        .is_some_and(|c| FAILED.contains(&c))
}

/// What one case read of CI: the run list, and the failed logs not yet kept
pub(super) struct CiReads {
    head: String,
    runs: Result<Vec<HeadRun>, ForgeError>,
    fresh: BTreeMap<u64, Result<String, ForgeError>>,
}

impl Runner {
    // What the wake's reads found of `head`'s runs, or a read now for what they did not
    pub(super) fn ci_reads(&self, ticket: TicketId, head: &str) -> CiReads {
        let found = self.pm.logs.as_ref();
        let runs = (found.and_then(|l| l.head_runs(head)).cloned())
            .unwrap_or_else(|| self.ports.forge.head_runs(&self.remote, head));
        let mut fresh = BTreeMap::new();
        for run in runs.iter().flatten().filter(|r| ended_red(r)) {
            if self.pm.run_logs.contains_key(&(ticket, run.run)) {
                continue;
            }
            let read = (found.and_then(|l| l.run_log(run.run)).cloned()).unwrap_or_else(|| {
                self.ports
                    .forge
                    .run_log(&self.remote, run.run, LogPart::Failed)
            });
            fresh.insert(run.run, read);
        }
        CiReads {
            head: head.to_owned(),
            runs,
            fresh,
        }
    }

    // Keeps each log `ci` read, so a later wake does not read it again. A log
    // that could not be read is asked for again.
    pub(super) fn keep_run_logs(&mut self, ticket: TicketId, ci: CiReads) {
        for (run, read) in ci.fresh {
            if let Ok(text) = read {
                self.pm.run_logs.insert((ticket, run), text);
            }
        }
    }

    // The `## CI` part, and `why` it could not be read, if it could not
    pub(super) fn ci_part(
        &self,
        out: &mut String,
        ticket: TicketId,
        ci: Option<&CiReads>,
    ) -> Option<String> {
        out.push_str("## CI\n\n");
        let Some(ci) = ci else {
            out.push_str("(the pull request's head is not known, so its runs are not asked for)\n");
            return None;
        };
        let runs = match &ci.runs {
            Ok(runs) => runs,
            Err(e) => {
                let why = format!("kelpie could not read it ({})", cause(e));
                let _ = writeln!(out, "({why})");
                return Some(why);
            }
        };
        let _ = write!(
            out,
            "Runs on head `{}`: {}.\n\n",
            plain(&ci.head),
            runs.len()
        );
        for run in runs {
            let ended = run
                .conclusion
                .as_deref()
                .map_or("still going".to_owned(), plain);
            let _ = writeln!(
                out,
                "- `{}`: {ended}, run {}",
                plain(&run.workflow),
                run.run
            );
        }
        if runs.len() >= HEAD_RUNS_LIMIT {
            let _ = write!(
                out,
                "\nGitHub lists at most {HEAD_RUNS_LIMIT} runs of one commit, and this came back \
                 with {}: older runs may be missing.\n",
                runs.len()
            );
        }
        for run in runs.iter().filter(|r| ended_red(r)) {
            let _ = write!(
                out,
                "\n### Log of `{}`, run {}\n\n",
                plain(&run.workflow),
                run.run
            );
            self.run_log_part(out, ticket, run.run, ci);
        }
        None
    }

    // One failed run's log, quoted, or what became of it
    fn run_log_part(&self, out: &mut String, ticket: TicketId, run: u64, ci: &CiReads) {
        let read = match (self.pm.run_logs.get(&(ticket, run)), ci.fresh.get(&run)) {
            (Some(kept), _) => Ok(kept),
            (None, Some(Ok(fresh))) => Ok(fresh),
            (None, Some(Err(e))) => Err(e),
            (None, None) => return,
        };
        match read {
            Ok(log) if log.trim().is_empty() => out.push_str("The log is empty.\n"),
            Ok(log) => {
                let _ = writeln!(out, "{}", self.data(log));
            }
            Err(e) if e.log_is_gone() => {
                let _ = writeln!(
                    out,
                    "The log of run {run} is gone: GitHub drops a log past its retention. \
                     (kelpie's read said: {}.)",
                    cause(e)
                );
            }
            Err(e) => {
                let _ = writeln!(
                    out,
                    "The log of run {run} could not be read just now ({}). It is asked for again \
                     at the next wake.",
                    cause(e)
                );
            }
        }
    }
}
