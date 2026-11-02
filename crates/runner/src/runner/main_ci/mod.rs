//! Which workflows are red on `main`
//!
//! The runner reads the latest completed run of each workflow on `main` at
//! most once a [`MAIN_CI_POLL`](super::looks::MAIN_CI_POLL), and keeps the
//! workflows whose latest run failed in the project's state, for `status` and
//! the board to show. A workflow stays red until a run of it succeeds: a run
//! that was cancelled or skipped says nothing about the code, so it changes
//! nothing. A workflow the forge no longer runs, because its file is gone or
//! it is disabled, drops out whatever its last run was. The forge is asked with the runner's lock
//! let go (see [`read`]).

use serde::Serialize;

use self::read::Done;
use super::Runner;
use crate::ports::{BranchRun, ForgeError, Timestamp};
use crate::state::{MainNotice, Notice, RedWorkflow};
use crate::ticket::TicketId;

/// A workflow red on `main`, as `status` shows it
#[derive(Debug, Serialize)]
pub struct RedStatus {
    /// The workflow's name
    pub workflow: String,
    /// The forge's id for the first red run kelpie read since it last read the
    /// workflow green
    pub first_run: u64,
    /// The forge's id for its latest red run
    pub run: u64,
    /// The commit the latest red run ran on
    pub head: String,
    /// When `first_run` was created
    pub since: Timestamp,
    /// How long it has been red, in seconds
    pub red_for: u64,
    /// The latest red run's page on the forge
    pub url: String,
    /// What is being done about the latest red head
    pub doing: String,
}

impl Runner {
    // What to do about the workflows this read found red, and the notices to post. A new
    // red head is rerun once, then told once when red again or where the forge refuses a
    // rerun, and each of `cleared` that was told is told green. `done` holds the forge's
    // answers, and a workflow with none waits for the next read.
    fn act_on_reds(
        failing: &[String],
        red: &mut [RedWorkflow],
        cleared: &[Cleared],
        done: &mut Done,
        now: Timestamp,
    ) -> Vec<Notice> {
        let mut notices = Vec::new();
        for workflow in red.iter_mut().filter(|r| failing.contains(&r.workflow)) {
            let Some(mut answers) = done.remove(&workflow.run) else {
                continue;
            };
            if workflow.rerun_done() {
                workflow.again = true;
            } else {
                match answers.rerun.take() {
                    None | Some(Err(ForgeError::Held(_) | ForgeError::Limited { .. })) => continue,
                    Some(Ok(())) => {
                        workflow.reran = Some(workflow.run);
                        workflow.reran_head = Some(workflow.head.clone());
                        continue;
                    }
                    Some(Err(e)) => {
                        eprintln!("cannot rerun {} on main: {e}", workflow.workflow);
                        workflow.reran_head = Some(workflow.head.clone());
                        workflow.again = true;
                    }
                }
            }
            if workflow.noticed {
                continue;
            }
            let Some(log) = answers.log else {
                continue;
            };
            workflow.noticed = true;
            notices.push(Notice::Main(MainNotice {
                workflow: workflow.workflow.clone(),
                run: workflow.run,
                red_for: now.0.saturating_sub(workflow.since.0),
                log: log_end(&log),
                green: false,
            }));
        }
        for Cleared { was, by } in cleared.iter().filter(|c| c.was.noticed) {
            notices.push(Notice::Main(MainNotice {
                workflow: was.workflow.clone(),
                run: *by,
                red_for: now.0.saturating_sub(was.since.0),
                log: String::new(),
                green: true,
            }));
        }
        notices
    }

    /// The workflows red on `main` as `status` shows them, in the order they went red
    pub(super) fn red_status(&self, now: Timestamp) -> Vec<RedStatus> {
        (self.state.red.iter())
            .map(|red| RedStatus {
                workflow: red.workflow.clone(),
                first_run: red.first_run,
                run: red.run,
                head: red.head.clone(),
                since: red.since,
                red_for: now.0.saturating_sub(red.since.0),
                doing: red.doing(),
                url: format!(
                    "https://github.com/{}/actions/runs/{}",
                    self.remote.as_str(),
                    red.run
                ),
            })
            .collect()
    }
}

// The last lines of a failed log
fn log_end(log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    let end = lines[lines.len().saturating_sub(NOTICE_LOG_LINES)..].join("\n");
    let mut cut = end.len().saturating_sub(NOTICE_LOG_BYTES);
    while !end.is_char_boundary(cut) {
        cut += 1;
    }
    end[cut..].to_owned()
}

// Whether a run ended in a way that means the code is broken
fn is_red(conclusion: &str) -> bool {
    matches!(conclusion, "failure" | "timed_out" | "startup_failure")
}

// The newest run of each workflow among those a push, a schedule or a manual
// start made, in the order the workflows first appear. A pull request's run
// is not `main`'s: one from a fork's own `main` carries the branch's name.
fn on_main(runs: &[BranchRun]) -> Vec<&BranchRun> {
    let mut latest: Vec<&BranchRun> = Vec::new();
    for run in runs
        .iter()
        .filter(|r| !matches!(r.event.as_str(), "pull_request" | "pull_request_target"))
    {
        match latest.iter_mut().find(|r| r.workflow == run.workflow) {
            Some(kept) if kept.created < run.created => *kept = run,
            Some(_) => {}
            None => latest.push(run),
        }
    }
    latest
}

/// How much of the end of a failed log a notice carries: at most this many
/// lines, and at most this many bytes, which a webhook's message has room for
const NOTICE_LOG_LINES: usize = 20;
const NOTICE_LOG_BYTES: usize = 2000;

/// What a read of `main`'s runs found
struct Read {
    /// The red workflows after it
    red: Vec<RedWorkflow>,
    /// The workflows whose latest run it read was red
    failing: Vec<String>,
    /// The workflows a successful run took out of `red`
    cleared: Vec<Cleared>,
    /// The workflows whose fix work item had closed while they were still red,
    /// so they are the project manager's again
    reopened: Vec<String>,
}

/// A workflow that went green
struct Cleared {
    /// How it stood when it was red
    was: RedWorkflow,
    /// The run that cleared it
    by: u64,
}

// `red` holds the latest completed run of each workflow and event. A red
// run keeps or adds its workflow, a successful one drops it. A red
// workflow's run from before the one kelpie reran is not its latest. While
// the rerun's jobs go the run is not completed, and an older run shows. A fix
// work item stays a workflow's through every new head until it is one of
// `closed`, so one failure has one fix however many commits land on top of it.
fn still_red(red: &[RedWorkflow], runs: &[BranchRun], closed: &[TicketId]) -> Read {
    let mut next = red.to_vec();
    let mut failing = Vec::new();
    let mut cleared = Vec::new();
    let mut reopened = Vec::new();
    for red in next
        .iter_mut()
        .filter(|r| r.fix().is_some_and(|t| closed.contains(&t)))
    {
        red.handled = None;
        reopened.push(red.workflow.clone());
    }
    for run in on_main(runs) {
        let at = next.iter().position(|r| r.workflow == run.workflow);
        let older =
            |r: &RedWorkflow| run.created < r.since || r.reran.is_some_and(|rerun| run.id < rerun);
        if at.is_some_and(|at| older(&next[at])) {
            continue;
        }
        if !is_red(&run.conclusion) {
            if let (Some(at), "success") = (at, run.conclusion.as_str()) {
                cleared.push(Cleared {
                    was: next.remove(at),
                    by: run.id,
                });
            }
            continue;
        }
        failing.push(run.workflow.clone());
        if let Some(at) = at {
            let red = &mut next[at];
            if red.head != run.head {
                red.again = false;
                if red.fix().is_none() {
                    red.handled = None;
                }
            }
            red.run = run.id;
            red.head.clone_from(&run.head);
        } else {
            next.push(RedWorkflow {
                workflow: run.workflow.clone(),
                first_run: run.id,
                run: run.id,
                head: run.head.clone(),
                since: run.created,
                reran: None,
                reran_head: None,
                again: false,
                handled: None,
                noticed: false,
            });
        }
    }
    Read {
        red: next,
        failing,
        cleared,
        reopened,
    }
}

pub(super) use read::read_main_ci_off_lock;
#[cfg(test)]
mod notice_tests;
mod read;
#[cfg(test)]
mod tests;
