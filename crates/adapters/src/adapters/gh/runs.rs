//! The logs of the CI runs that failed, read for a worker whose token cannot
//!
//! A worker on a repo kelpie's App covers holds a token that pushes and does
//! nothing else, so `gh run view` is refused it. Kelpie reads the failed
//! runs' logs as the login `gh` acts as and puts the end of each in the
//! worker's prompt.

use serde::Deserialize;

use super::{gh, unreadable};
use crate::ports::{BranchRun, FailedLogs, ForgeError, Rerun, Timestamp, Workflow, WorkflowState};
use crate::settings::ForgeSlug;

/// How many failed runs are read
const RUNS: usize = 3;

/// How much of the end of each log is kept, in bytes: where a failure says why
const LOG_BYTES: usize = 6000;

pub(super) fn failed_run_logs(
    repo: &ForgeSlug,
    head: &str,
    merge_group: bool,
) -> Result<FailedLogs, ForgeError> {
    let listed = list(repo)?;
    let failed = failed_runs(&listed, head, merge_group)?;
    let mut text = String::new();
    for id in failed.iter().copied().take(RUNS) {
        text.push_str(&format!("--- run {id}\n{}\n", run_log_tail(repo, id)?));
    }
    Ok(FailedLogs {
        text,
        omitted: failed.len().saturating_sub(RUNS),
    })
}

pub(super) fn run_log_tail(repo: &ForgeSlug, run: u64) -> Result<String, ForgeError> {
    let id = run.to_string();
    let log = gh(&["run", "view", &id, "--repo", repo.as_str(), "--log-failed"])?;
    Ok(failure_tail(&String::from_utf8_lossy(&log), LOG_BYTES))
}

/// The latest completed run of each workflow and event among the 50 newest completed runs on `branch`
pub(super) fn branch_runs(repo: &ForgeSlug, branch: &str) -> Result<Vec<BranchRun>, ForgeError> {
    let listed = gh(&[
        "run",
        "list",
        "--repo",
        repo.as_str(),
        "--branch",
        branch,
        "--status",
        "completed",
        "--limit",
        "50",
        "--json",
        "databaseId,workflowName,conclusion,headSha,event,createdAt",
    ])?;
    latest_runs(&listed)
}

/// How many workflows are asked for: a list this long may have been cut off
const WORKFLOWS: usize = 1000;

/// Every workflow of `repo`, the disabled ones too
pub(super) fn workflows(repo: &ForgeSlug) -> Result<Vec<Workflow>, ForgeError> {
    let limit = WORKFLOWS.to_string();
    let listed = gh(&[
        "workflow",
        "list",
        "--repo",
        repo.as_str(),
        "--all",
        "--limit",
        &limit,
        "--json",
        "name,path,state",
    ])?;
    let workflows = parse_workflows(&listed)?;
    if workflows.len() >= WORKFLOWS {
        return Err(ForgeError::Failed(format!(
            "{} workflows listed, which may not be all of them",
            workflows.len()
        )));
    }
    Ok(workflows)
}

// `state` is the field GitHub documents for a workflow, in "List repository
// workflows"; a value outside its documented ones is a list kelpie cannot read
fn parse_workflows(listed: &[u8]) -> Result<Vec<Workflow>, ForgeError> {
    #[derive(Deserialize)]
    struct Listed {
        name: String,
        path: String,
        state: String,
    }
    let workflows: Vec<Listed> = serde_json::from_slice(listed).map_err(|_| unreadable(listed))?;
    workflows
        .into_iter()
        .map(|w| {
            let state = match w.state.as_str() {
                "active" => WorkflowState::Active,
                "deleted" => WorkflowState::Deleted,
                "disabled_manually" => WorkflowState::DisabledManually,
                "disabled_inactivity" => WorkflowState::DisabledInactivity,
                "disabled_fork" => WorkflowState::DisabledFork,
                _ => return Err(unreadable(listed)),
            };
            Ok(Workflow {
                name: w.name,
                path: w.path,
                state,
            })
        })
        .collect()
}

// The newest run of each workflow and event in `listed`, in the order they
// first appear; of two runs created in the same second the one listed first stays
fn latest_runs(listed: &[u8]) -> Result<Vec<BranchRun>, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Run {
        database_id: u64,
        workflow_name: String,
        conclusion: String,
        head_sha: String,
        event: String,
        created_at: String,
    }
    let runs: Vec<Run> = serde_json::from_slice(listed).map_err(|_| unreadable(listed))?;
    let mut latest: Vec<BranchRun> = Vec::new();
    for run in runs {
        let created = run
            .created_at
            .parse::<jiff::Timestamp>()
            .ok()
            .and_then(|at| u64::try_from(at.as_second()).ok())
            .map(Timestamp)
            .ok_or_else(|| unreadable(listed))?;
        let run = BranchRun {
            workflow: run.workflow_name,
            id: run.database_id,
            head: run.head_sha,
            event: run.event,
            conclusion: run.conclusion,
            created,
        };
        match (latest.iter_mut()).find(|r| r.workflow == run.workflow && r.event == run.event) {
            Some(kept) if kept.created < run.created => *kept = run,
            Some(_) => {}
            None => latest.push(run),
        }
    }
    Ok(latest)
}

/// Reruns the failed jobs of the failed runs of `head`
///
/// Refused where more runs failed than `failed_run_logs` reads, since the
/// unread logs may name a changed file and a run left red would make the
/// fix turn's "failed again" false. Every run is asked for, and the call
/// fails only when none was rerun: a run that did start is the rerun the
/// caller records, so a refusal of another run does not hide it.
pub(super) fn rerun_failed(repo: &ForgeSlug, head: &str) -> Result<(), ForgeError> {
    let listed = list(repo)?;
    let ids = failed_runs(&listed, head, false)?;
    if ids.is_empty() {
        return Err(ForgeError::Failed(format!("no failed run on {head}")));
    }
    if ids.len() > RUNS {
        return Err(ForgeError::Failed(format!(
            "{} runs failed on {head}, more than the {RUNS} whose logs kelpie reads",
            ids.len()
        )));
    }
    let mut first_error = None;
    let mut started = false;
    for id in ids {
        let id = id.to_string();
        match gh(&["run", "rerun", &id, "--repo", repo.as_str(), "--failed"]) {
            Ok(_) => started = true,
            Err(e) => first_error = first_error.or(Some(e)),
        }
    }
    match (started, first_error) {
        (false, Some(e)) => Err(e),
        _ => Ok(()),
    }
}

/// Reruns the jobs of run `run` that `rerun` names
pub(super) fn rerun_run(repo: &ForgeSlug, run: u64, rerun: Rerun) -> Result<(), ForgeError> {
    let id = run.to_string();
    gh(&rerun_args(repo, &id, rerun)).map(drop)
}

pub(super) fn rerun_args<'a>(repo: &'a ForgeSlug, id: &'a str, rerun: Rerun) -> Vec<&'a str> {
    let mut args = vec!["run", "rerun", id, "--repo", repo.as_str()];
    if rerun == Rerun::Failed {
        args.push("--failed");
    }
    args
}

/// Approves every run of `head` that waits for a maintainer, as the login `gh` acts as
///
/// Every run is asked for, and the first refusal is returned once all were
/// tried: a run approved is no longer waiting, so a retry finds only the
/// ones that failed.
pub(super) fn approve_workflow_runs(repo: &ForgeSlug, head: &str) -> Result<(), ForgeError> {
    let path = format!(
        "repos/{}/actions/runs?status=action_required&head_sha={head}&per_page=100",
        repo.as_str()
    );
    let mut first_error = None;
    for id in held_runs(&gh(&["api", &path])?, head)? {
        let approve = format!("repos/{}/actions/runs/{id}/approve", repo.as_str());
        if let Err(e) = gh(&["api", "--method", "POST", &approve]) {
            first_error = first_error.or(Some(e));
        }
    }
    first_error.map_or(Ok(()), Err)
}

// What GitHub calls a run that waits for approval. Its `status` query matches
// a run's status or its conclusion, so a held run may carry it as either.
const HELD: &str = "action_required";

// The runs of `head` that wait for approval, from the listing of a repo's
// runs. The forge was asked for that head and that status, and a run that is
// neither is dropped here too: approving runs the contributor's code.
fn held_runs(listed: &[u8], head: &str) -> Result<Vec<u64>, ForgeError> {
    #[derive(Deserialize)]
    struct Listing {
        workflow_runs: Vec<Run>,
    }
    #[derive(Deserialize)]
    struct Run {
        id: u64,
        head_sha: String,
        status: String,
        conclusion: Option<String>,
    }
    let listing: Listing = serde_json::from_slice(listed).map_err(|_| unreadable(listed))?;
    Ok(listing
        .workflow_runs
        .into_iter()
        .filter(|r| {
            r.head_sha == head && (r.status == HELD || r.conclusion.as_deref() == Some(HELD))
        })
        .map(|r| r.id)
        .collect())
}

fn list(repo: &ForgeSlug) -> Result<Vec<u8>, ForgeError> {
    gh(&[
        "run",
        "list",
        "--repo",
        repo.as_str(),
        "--limit",
        "50",
        "--json",
        "databaseId,conclusion,headSha,event",
    ])
}

// Every failed run of `head`, or of the merge queue, newest first
fn failed_runs(listed: &[u8], head: &str, merge_group: bool) -> Result<Vec<u64>, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Run {
        database_id: u64,
        conclusion: String,
        head_sha: String,
        event: String,
    }
    let runs: Vec<Run> = serde_json::from_slice(listed).map_err(|_| unreadable(listed))?;
    Ok(runs
        .into_iter()
        .filter(|r| r.conclusion == "failure")
        .filter(|r| match merge_group {
            true => r.event == "merge_group",
            false => r.head_sha == head,
        })
        .map(|r| r.database_id)
        .collect())
}

// The last `max` bytes of `log` once cargo's progress lines, GitHub's
// timestamps and terminal colour codes are gone, so the end is the failure and
// not the build before it
fn failure_tail(log: &str, max: usize) -> String {
    let kept: Vec<String> = log
        .lines()
        .filter_map(|line| {
            // `gh run view --log-failed` puts the job and step before the text
            let mut parts = line.splitn(3, '\t');
            let (names, text) = match (parts.next(), parts.next(), parts.next()) {
                (Some(job), Some(step), Some(text)) => (format!("{job}\t{step}\t"), text),
                _ => (String::new(), line),
            };
            let text = strip_colour(strip_timestamp(text));
            (!is_progress(&text)).then(|| format!("{names}{text}"))
        })
        .collect();
    tail(&kept.join("\n"), max).to_owned()
}

// The verbs cargo prints as it builds, with the width it right-aligns them to
const PROGRESS_WIDTH: usize = 12;
const PROGRESS: [&str; 5] = [
    "Compiling",
    "Downloaded",
    "Downloading",
    "Checking",
    "Fresh",
];

// A line cargo prints as it builds: its verb right-aligned to 12 columns,
// then a crate and its version, or the `crates` a download counts. A test's
// own line that starts with the same word is not indented so, and stays.
fn is_progress(text: &str) -> bool {
    let word = text.trim_start_matches(' ');
    let indent = text.len() - word.len();
    PROGRESS.iter().any(|verb| {
        indent + verb.len() == PROGRESS_WIDTH
            && word
                .strip_prefix(verb)
                .and_then(|rest| rest.strip_prefix(' '))
                .is_some_and(|rest| {
                    rest.split(' ').any(|w| {
                        w == "crates"
                            || w.strip_prefix('v')
                                .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
                    })
                })
    })
}

// `text` without the `2026-10-13T10:00:00.0000000Z ` GitHub puts before it,
// and the byte order mark the first line of a log starts with
fn strip_timestamp(text: &str) -> &str {
    let text = text.trim_start_matches('\u{feff}');
    match text.split_once(' ') {
        Some((stamp, rest)) if is_timestamp(stamp) => rest,
        _ => text,
    }
}

// `2026-10-13T10:00:00.0000000Z`
fn is_timestamp(word: &str) -> bool {
    let b = word.as_bytes();
    b.len() >= 20
        && b.ends_with(b"Z")
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[10] == b'T'
        && b[11..b.len() - 1]
            .iter()
            .all(|c| c.is_ascii_digit() || matches!(c, b':' | b'.'))
}

// `text` without its ANSI escape sequences, such as the colour cargo writes
// under `CARGO_TERM_COLOR=always`
fn strip_colour(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.next_if_eq(&'[').is_some() {
            // A sequence ends at its first byte from `@` to `~`
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

// The last `max` bytes of `log`, from the start of a line where one is near
fn tail(log: &str, max: usize) -> &str {
    if log.len() <= max {
        return log.trim_end();
    }
    let mut from = log.len() - max;
    while !log.is_char_boundary(from) {
        from += 1;
    }
    let cut = &log[from..];
    cut.split_once('\n')
        .map_or(cut, |(_, rest)| rest)
        .trim_end()
}

#[cfg(test)]
mod tests;
