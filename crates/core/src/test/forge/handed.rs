//! The fake forge's pull requests kelpie opens for a worker, and the CI logs
//! it reads for one

use std::collections::HashMap;

use super::FakeForge;
use crate::ports::{BranchRun, FailedLogs, PullRequestState, Rerun, Workflow, WorkflowState};

/// A pull request on the fake forge. Its head is its branch on the rig's
/// origin, so a push the runner makes moves it.
#[derive(Debug, Clone)]
pub struct FakePullRequest {
    pub base: String,
    pub branch: String,
    pub state: PullRequestState,
    pub draft: bool,
    pub from_fork: bool,
    // Where the fork is cloned from, and whether its owner lets the
    // maintainers edit it
    pub fork: Option<(String, bool)>,
    // Whether an organization owns the fork
    pub fork_owned_by_organization: bool,
    pub author: String,
    pub author_is_bot: bool,
}

/// What the fake forge's failed CI runs say, and the reruns asked of them
#[derive(Debug, Default)]
pub struct CiRuns {
    log: String,
    // How many failed runs the log leaves out, as `failed_run_logs` reports it
    log_omitted: usize,
    // How many of the log's runs a job's timeout cut short, as `failed_run_logs` reports it
    log_timed_out: usize,
    log_error: Option<String>,
    reruns: Vec<String>,
    rerun_error: Option<String>,
    run_reruns: Vec<(u64, Rerun)>,
    // The runs still in progress, which the forge refuses to rerun
    running: Vec<u64>,
    // The latest run of each workflow, by branch
    branch_runs: HashMap<String, Vec<BranchRun>>,
    // The workflow names any scripted runs carried, in the order they came
    run_workflows: Vec<String>,
    // Why listing a branch's runs fails
    branch_runs_error: Option<String>,
    // How many times a branch's runs were listed
    branch_runs_reads: usize,
    // The repo's workflows, or none scripted: every name a branch's runs
    // have carried, all active
    workflows: Option<Vec<Workflow>>,
    // Why listing the workflows fails
    workflows_error: Option<String>,
    // How many times the workflows were listed
    workflows_reads: usize,
    // The end of each run's failed log, by run id
    run_logs: HashMap<u64, String>,
}

impl CiRuns {
    /// The logs, or why reading them fails
    pub fn log(&self) -> Result<FailedLogs, String> {
        match &self.log_error {
            Some(error) => Err(error.clone()),
            None => Ok(FailedLogs {
                text: self.log.clone(),
                omitted: self.log_omitted,
                timed_out: self.log_timed_out,
            }),
        }
    }

    /// Records a rerun of `head`, or says why it fails
    pub fn rerun(&mut self, head: &str) -> Result<(), String> {
        match &self.rerun_error {
            Some(error) => Err(error.clone()),
            None => {
                self.reruns.push(head.to_owned());
                Ok(())
            }
        }
    }
}

impl CiRuns {
    /// Records a rerun of run `run`, or says why it fails
    pub fn rerun_of_run(&mut self, run: u64, rerun: Rerun) -> Result<(), String> {
        match &self.rerun_error {
            Some(error) => Err(error.clone()),
            None if self.running.contains(&run) => Err(format!("run {run} is still in progress")),
            None => {
                self.run_reruns.push((run, rerun));
                Ok(())
            }
        }
    }
}

impl FakeForge {
    /// The runs whose failed CI jobs kelpie reran by id, in order
    pub fn run_reruns(&self) -> Vec<u64> {
        let reruns = self.ci.lock().unwrap().run_reruns.clone();
        (reruns.into_iter())
            .filter(|(_, rerun)| *rerun == Rerun::Failed)
            .map(|(run, _)| run)
            .collect()
    }

    /// The runs kelpie reran whole by id, in order
    pub fn whole_run_reruns(&self) -> Vec<u64> {
        let reruns = self.ci.lock().unwrap().run_reruns.clone();
        (reruns.into_iter())
            .filter(|(_, rerun)| *rerun == Rerun::All)
            .map(|(run, _)| run)
            .collect()
    }

    /// Makes the forge refuse to rerun run `run`, as it does a run still in progress
    pub fn set_run_in_progress(&self, run: u64) {
        self.ci.lock().unwrap().running.push(run);
    }

    /// Makes the failed CI runs' logs say `log`
    pub fn set_ci_log(&self, log: &str) {
        let mut ci = self.ci.lock().unwrap();
        (ci.log, ci.log_error) = (log.to_owned(), None);
        (ci.log_omitted, ci.log_timed_out) = (0, 0);
    }

    /// Makes the failed CI runs' logs say `log`, with `omitted` failed runs kelpie
    /// did not read
    pub fn set_ci_log_omitted(&self, log: &str, omitted: usize) {
        let mut ci = self.ci.lock().unwrap();
        (ci.log, ci.log_error) = (log.to_owned(), None);
        (ci.log_omitted, ci.log_timed_out) = (omitted, 0);
    }

    /// Makes the failed CI runs' logs say `log`, `timed_out` of the runs having
    /// been cut short by a job's timeout
    pub fn set_ci_log_timed_out(&self, log: &str, timed_out: usize) {
        let mut ci = self.ci.lock().unwrap();
        (ci.log, ci.log_error) = (log.to_owned(), None);
        (ci.log_omitted, ci.log_timed_out) = (0, timed_out);
    }

    /// Makes reading the failed CI runs' logs fail with `error`, verbatim
    pub fn set_ci_log_error(&self, error: &str) {
        self.ci.lock().unwrap().log_error = Some(error.to_owned());
    }

    /// The heads whose failed CI jobs kelpie reran, in order
    pub fn reruns(&self) -> Vec<String> {
        self.ci.lock().unwrap().reruns.clone()
    }

    /// Makes rerunning failed CI jobs fail with `error`, verbatim
    pub fn set_rerun_error(&self, error: &str) {
        self.ci.lock().unwrap().rerun_error = Some(error.to_owned());
    }

    /// Makes `branch` list `runs`, the latest completed run of each of its workflows
    pub fn set_branch_runs(&self, branch: &str, runs: Vec<BranchRun>) {
        let mut ci = self.ci.lock().unwrap();
        for run in &runs {
            if !ci.run_workflows.contains(&run.workflow) {
                ci.run_workflows.push(run.workflow.clone());
            }
        }
        ci.branch_runs.insert(branch.to_owned(), runs);
    }

    /// Makes listing a branch's runs fail with `error`, verbatim
    pub fn set_branch_runs_error(&self, error: &str) {
        self.ci.lock().unwrap().branch_runs_error = Some(error.to_owned());
    }

    /// How many times a branch's runs were listed, failed lists included
    pub fn branch_runs_reads(&self) -> usize {
        self.ci.lock().unwrap().branch_runs_reads
    }

    /// Makes the repo list `workflows`
    pub fn set_workflows(&self, workflows: Vec<Workflow>) {
        self.ci.lock().unwrap().workflows = Some(workflows);
    }

    /// Makes listing the workflows fail with `error`, verbatim
    pub fn set_workflows_error(&self, error: &str) {
        self.ci.lock().unwrap().workflows_error = Some(error.to_owned());
    }

    /// How many times the workflows were listed, failed lists included
    pub fn workflows_reads(&self) -> usize {
        self.ci.lock().unwrap().workflows_reads
    }

    /// Makes the failed log of run `run` end in `tail`
    pub fn set_run_log_tail(&self, run: u64, tail: &str) {
        self.ci
            .lock()
            .unwrap()
            .run_logs
            .insert(run, tail.to_owned());
    }

    /// The runs `branch` lists: none for a branch no test scripted
    pub fn branch_runs_of(&self, branch: &str) -> Result<Vec<BranchRun>, String> {
        let mut ci = self.ci.lock().unwrap();
        ci.branch_runs_reads += 1;
        match &ci.branch_runs_error {
            Some(error) => Err(error.clone()),
            None => Ok(ci.branch_runs.get(branch).cloned().unwrap_or_default()),
        }
    }

    /// The workflows listed: those a test scripted, or else one active
    /// workflow for each name any scripted run carried
    pub fn workflows_listed(&self) -> Result<Vec<Workflow>, String> {
        let mut ci = self.ci.lock().unwrap();
        ci.workflows_reads += 1;
        if let Some(error) = &ci.workflows_error {
            return Err(error.clone());
        }
        if let Some(workflows) = &ci.workflows {
            return Ok(workflows.clone());
        }
        Ok(ci
            .run_workflows
            .iter()
            .map(|name| Workflow {
                name: name.clone(),
                path: format!(".github/workflows/{name}.yml"),
                state: WorkflowState::Active,
            })
            .collect())
    }

    /// The end of run `run`'s failed log, or why none can be read
    pub fn run_log_tail_of(&self, run: u64) -> Result<String, String> {
        let ci = self.ci.lock().unwrap();
        ci.run_logs
            .get(&run)
            .cloned()
            .ok_or_else(|| format!("no log for run {run}"))
    }

    /// Makes pull request `number` read with `title`
    pub fn set_title(&self, number: u64, title: &str) {
        self.titles.lock().unwrap().insert(number, title.to_owned());
    }

    /// The title and body pull request `number` was opened or edited with
    pub fn pull_request_text(&self, number: u64) -> Option<(String, String)> {
        self.texts.lock().unwrap().get(&number).cloned()
    }

    /// The title and body kelpie gave pull request `number` when it opened or
    /// edited it, or none when it did neither
    pub fn text_of(&self, number: u64) -> Option<(String, String)> {
        self.texts.lock().unwrap().get(&number).cloned()
    }
}
