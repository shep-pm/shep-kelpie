//! Reading `main`'s workflows, and acting on the red ones, with the runner's lock let go
//!
//! [`Runner::main_ci_job`] decides a read is due and copies what it needs, [`MainCiJob::run`]
//! reads, [`Runner::plan_acts`] works the red workflows out on the state it finds and lists the
//! reruns and log reads they need, [`Acts::run`] makes those calls, and [`Runner::finish_main_ci`]
//! works the red workflows out again and acts on them with the answers. A read the forge refuses
//! waits as any failed read does and leaves what the state holds; a state that cannot be saved is
//! read again at the next poll.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{Read, still_red};
use crate::ports::{BranchRun, Forge, ForgeError, Rerun, Tickets, Workflow, WorkflowState};
use crate::runner::Runner;
use crate::runner::trigger::lock;
use crate::settings::ForgeSlug;
use crate::state::RedWorkflow;
use crate::ticket::TicketId;

/// Reads `main`'s workflows when they are due and the forge is not held, with the runner's lock
/// let go, at the start of every pass, before any work item is stepped
///
/// A step checks that it drains just before it saves the turn it starts, and a forge read between
/// that check and the call is time enough for a `drain` to land past it.
pub(in crate::runner) fn read_main_ci_off_lock(runner: &Mutex<Runner>) {
    let job = lock(runner).main_ci_job();
    let Some(job) = job else {
        return;
    };
    let read = job.run();
    let acts = lock(runner).plan_acts(read);
    let Some(acts) = acts else {
        return;
    };
    let done = acts.run();
    lock(runner).finish_main_ci(acts.runs, done);
}

/// The calls the red workflows of a read need, and what they need them for
struct Acts {
    forge: Arc<dyn Forge>,
    repo: ForgeSlug,
    runs: Runs,
    plan: Vec<Step>,
}

/// What one failing workflow's latest run needs
struct Step {
    run: u64,
    /// Its failed jobs are to be rerun
    rerun: bool,
    /// It has not been told, so the end of its log is read where it is to be
    log: bool,
}

/// What the forge answered about one run
#[derive(Default)]
pub(super) struct Answers {
    /// The rerun, where one was asked
    pub rerun: Option<Result<(), ForgeError>>,
    /// The failed log, where one was read
    pub log: Option<String>,
}

/// The answers of a plan, by run
pub(super) type Done = HashMap<u64, Answers>;

impl Acts {
    fn run(&self) -> Done {
        let mut done = Done::new();
        for step in &self.plan {
            let mut answers = Answers::default();
            let mut read_log = step.log;
            if step.rerun {
                let rerun = self.forge.rerun_run(&self.repo, step.run, Rerun::Failed);
                // A rerun that went through, or waits, leaves the log for a later read
                read_log &= !matches!(
                    rerun,
                    Ok(()) | Err(ForgeError::Held(_) | ForgeError::Limited { .. })
                );
                answers.rerun = Some(rerun);
            }
            if read_log {
                answers.log = Some(self.log(step.run));
            }
            done.insert(step.run, answers);
        }
        done
    }

    // The failed log of run `run`, or none where it cannot be read
    fn log(&self, run: u64) -> String {
        self.forge
            .run_log_tail(&self.repo, run)
            .unwrap_or_else(|e| {
                eprintln!("cannot read the log of run {run} on main: {e}");
                String::new()
            })
    }
}

/// A read of `main`'s runs the runner decided was due, which needs no lock
struct MainCiJob {
    forge: Arc<dyn Forge>,
    tickets: Arc<dyn Tickets>,
    repo: ForgeSlug,
    branch: String,
    /// The red workflows as they stood when the read began
    red: Vec<RedWorkflow>,
}

/// What a read found
struct Runs {
    runs: Vec<BranchRun>,
    /// The forge's workflows, read only while one was red
    workflows: Option<Vec<Workflow>>,
    /// The fix work items of the red workflows that the source says are closed. One it
    /// could not read is left out, so it stays the workflow's.
    closed: Vec<TicketId>,
}

impl MainCiJob {
    // The latest runs on `main`, and the forge's workflows while something is red: a red
    // workflow stays listed when its latest run is older than the runs read, so the forge's list
    // is what drops one that can no longer run.
    fn run(self) -> Result<Runs, ForgeError> {
        let runs = self.forge.branch_runs(&self.repo, &self.branch)?;
        let workflows = match still_red(&self.red, &runs, &[]).red.is_empty() {
            true => None,
            false => Some(self.forge.workflows(&self.repo)?),
        };
        let closed = (self.red.iter().filter_map(RedWorkflow::fix))
            .filter(|&fix| self.tickets.issue(&self.repo, fix).is_ok_and(|i| !i.open))
            .collect();
        Ok(Runs {
            runs,
            workflows,
            closed,
        })
    }
}

impl Runner {
    // The read, when `main`'s workflows are due and the forge is not held
    fn main_ci_job(&self) -> Option<MainCiJob> {
        let now = self.ports.clock.now();
        if !self.looks.main_ci_due(now) || self.forge_hold.until(now).is_some() {
            return None;
        }
        Some(MainCiJob {
            forge: Arc::clone(&self.ports.forge),
            tickets: Arc::clone(&self.ports.tickets),
            repo: self.remote.clone(),
            branch: self.settings.git.base().to_owned(),
            red: self.state.red.clone(),
        })
    }

    // Records the read and lists the calls its red workflows need, none where the read failed
    fn plan_acts(&mut self, runs: Result<Runs, ForgeError>) -> Option<Acts> {
        let now = self.ports.clock.now();
        let read = runs
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|runs| self.red_after(runs));
        self.looks
            .main_ci_read(now, read.is_ok(), self.forge_hold.until(now));
        let (read, runs) = (read.ok()?, runs.ok()?);
        let plan = (read.red.iter())
            .filter(|r| read.failing.contains(&r.workflow))
            .map(|r| Step {
                run: r.run,
                rerun: !r.rerun_done(),
                log: !r.noticed,
            })
            .collect();
        Some(Acts {
            forge: Arc::clone(&self.ports.forge),
            repo: self.remote.clone(),
            runs,
            plan,
        })
    }

    // Works the red workflows out again on the state as it is now and acts on them with `done`
    fn finish_main_ci(&mut self, runs: Runs, mut done: Done) {
        let now = self.ports.clock.now();
        let Ok(read) = self.red_after(&runs) else {
            return;
        };
        let mut next = self.state.clone();
        next.red = read.red;
        let reopened = read.reopened;
        let notices =
            Runner::act_on_reds(&read.failing, &mut next.red, &read.cleared, &mut done, now);
        next.notices.extend(notices);
        if next.red == self.state.red {
            return;
        }
        match self.save(next) {
            Ok(()) => {
                self.pm_forget_reds(&reopened);
                self.board_changed();
            }
            Err(e) => eprintln!("cannot save the red workflows on main: {e}"),
        }
    }

    // The red workflows after `runs`, worked out on the state as it is now, which the forge's
    // list of workflows then thins.
    fn red_after(&self, runs: &Runs) -> Result<Read, ForgeError> {
        let mut read = still_red(&self.state.red, &runs.runs, &runs.closed);
        if read.red.is_empty() {
            return Ok(read);
        }
        let workflows = runs.workflows.as_ref().ok_or_else(|| {
            ForgeError::Failed("a workflow went red while the workflows were not read".into())
        })?;
        read.red.retain(|r| {
            (workflows.iter()).any(|w| w.name == r.workflow && w.state == WorkflowState::Active)
        });
        Ok(read)
    }
}
