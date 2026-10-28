//! GitHub, through the `gh` command line

mod access;
mod app;
mod attachment;
mod board;
mod conditional;
mod contents;
mod failed;
mod issues;
mod pulls;
mod queue;
mod rate_limit;
mod review;
pub mod review_bot;
mod rules;
mod runs;
mod suites;
mod talk;
mod thread;
mod tickets;

pub use app::GhApps;
pub use conditional::Etags;

use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;

use crate::board::OpenPullRequest;
use crate::ports::{
    BranchRun, Checks, CommentMarks, FailedLogs, FolderEntry, Forge, ForgeError, InstallationToken,
    Issue, NewLabel, OpenPull, PullRequest, PullRequestState, QueueStanding, Queued, RateLimits,
    Rerun, Reviewed, Talk, ThreadComment, Timestamp, Visibility, Workflow,
};
use crate::review_bot::{Activity, Login};
use crate::settings::ForgeSlug;
use crate::ticket::TicketId;

/// GitHub, through the `gh` command line
///
/// The REST reads that repeat keep their answers in `etags`, so a clone
/// shares them and a `Gh::default()` starts with none.
#[derive(Debug, Clone, Default)]
pub struct Gh {
    etags: Etags,
}

impl Gh {
    /// `gh`, keeping the answers of its repeated reads in `etags`
    pub fn new(etags: Etags) -> Self {
        Self { etags }
    }
}

impl Forge for Gh {
    fn visibility(&self, repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        parse_visibility(&gh(&[
            "repo",
            "view",
            repo.as_str(),
            "--json",
            "visibility",
        ])?)
    }

    fn default_branch(&self, repo: &ForgeSlug) -> Result<String, ForgeError> {
        parse_default_branch(&gh(&[
            "repo",
            "view",
            repo.as_str(),
            "--json",
            "defaultBranchRef",
        ])?)
    }

    fn requires_up_to_date(&self, repo: &ForgeSlug, base: &str) -> Result<bool, ForgeError> {
        rules::requires_up_to_date(repo, base)
    }

    fn branch_exists(&self, repo: &ForgeSlug, branch: &str) -> Result<bool, ForgeError> {
        contents::branch_exists(repo, branch)
    }

    fn folder(&self, repo: &ForgeSlug, path: &str) -> Result<Option<Vec<FolderEntry>>, ForgeError> {
        contents::folder(repo, path)
    }

    fn file_text(&self, repo: &ForgeSlug, path: &str) -> Result<String, ForgeError> {
        contents::file_text(repo, path)
    }

    fn repo_labels(&self, repo: &ForgeSlug) -> Result<Vec<String>, ForgeError> {
        let stdout = gh(&[
            "label",
            "list",
            "--repo",
            repo.as_str(),
            "--json",
            "name",
            "--limit",
            "1000",
        ])?;
        parse_label_names(&stdout)
    }

    fn create_label(&self, repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError> {
        gh(&[
            "label",
            "create",
            label.name,
            "--repo",
            repo.as_str(),
            "--color",
            label.color,
            "--description",
            label.description,
        ])
        .map(drop)
    }

    fn can_push(&self, repo: &ForgeSlug) -> Result<bool, ForgeError> {
        access::can_push(repo)
    }

    fn last_bot_review(
        &self,
        repo: &ForgeSlug,
        login: Login<'_>,
    ) -> Result<Option<Timestamp>, ForgeError> {
        access::last_bot_review(repo, login)
    }

    fn open_pull_requests(&self, repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
        board::open_pull_requests(None, repo)
    }

    fn pull_request(&self, repo: &ForgeSlug, number: u64) -> Result<PullRequest, ForgeError> {
        pull_request(None, repo, number)
    }

    fn reviewed(&self, repo: &ForgeSlug, number: u64) -> Result<Reviewed, ForgeError> {
        review::reviewed(None, repo, number)
    }

    fn talk(&self, repo: &ForgeSlug, number: u64) -> Result<Talk, ForgeError> {
        talk::talk(repo, number)
    }

    fn viewer(&self) -> Result<String, ForgeError> {
        review::viewer()
    }

    fn login_token(&self) -> Result<InstallationToken, ForgeError> {
        review::login_token()
    }

    fn account_created(&self, login: &str) -> Result<Timestamp, ForgeError> {
        issues::account_created(login)
    }

    fn comments_since(
        &self,
        repo: &ForgeSlug,
        since: Timestamp,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        thread::comments_since(&self.etags, repo, since)
    }

    fn thread_replies_since(
        &self,
        repo: &ForgeSlug,
        number: u64,
        since: Timestamp,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        thread::thread_replies_since(repo, number, since)
    }

    fn can_write(&self, repo: &ForgeSlug, login: &str) -> Result<bool, ForgeError> {
        thread::can_write(repo, login)
    }

    fn comment_marks(&self, repo: &ForgeSlug, comment: u64) -> Result<CommentMarks, ForgeError> {
        thread::comment_marks(&self.etags, repo, comment)
    }

    fn comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<u64, ForgeError> {
        let path = format!("repos/{}/issues/{number}/comments", repo.as_str());
        let field = format!("body={body}");
        let args = [
            "api", "--method", "POST", &path, "-f", &field, "--jq", ".id",
        ];
        let out = gh(&args)?;
        let text = String::from_utf8_lossy(&out);
        text.trim()
            .parse()
            .map_err(|_| ForgeError::Unreadable(text.into_owned()))
    }

    fn open_pulls(&self, repo: &ForgeSlug) -> Result<Vec<OpenPull>, ForgeError> {
        issues::open_pulls(repo)
    }

    fn close_pull_request(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        issues::close_pull_request(repo, number)
    }

    fn create_pull_request(
        &self,
        repo: &ForgeSlug,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<u64, ForgeError> {
        pulls::create(repo, head, base, title, body)
    }

    fn edit_pull_request(
        &self,
        repo: &ForgeSlug,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        pulls::edit(repo, number, title, body)
    }

    fn failed_run_logs(
        &self,
        repo: &ForgeSlug,
        head: &str,
        merge_group: bool,
    ) -> Result<FailedLogs, ForgeError> {
        runs::failed_run_logs(repo, head, merge_group)
    }

    fn branch_runs(&self, repo: &ForgeSlug, branch: &str) -> Result<Vec<BranchRun>, ForgeError> {
        runs::branch_runs(repo, branch)
    }

    fn workflows(&self, repo: &ForgeSlug) -> Result<Vec<Workflow>, ForgeError> {
        runs::workflows(repo)
    }

    fn run_log_tail(&self, repo: &ForgeSlug, run: u64) -> Result<String, ForgeError> {
        runs::run_log_tail(repo, run)
    }

    fn rerun_failed(&self, repo: &ForgeSlug, head: &str) -> Result<(), ForgeError> {
        runs::rerun_failed(repo, head)
    }

    fn approve_workflow_runs(&self, repo: &ForgeSlug, head: &str) -> Result<(), ForgeError> {
        runs::approve_workflow_runs(repo, head)
    }

    fn rerun_run(&self, repo: &ForgeSlug, run: u64, rerun: Rerun) -> Result<(), ForgeError> {
        runs::rerun_run(repo, run, rerun)
    }

    fn mark_ready(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        let number = number.to_string();
        gh(&["pr", "ready", &number, "--repo", repo.as_str()]).map(drop)
    }

    fn set_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        review_bot::label(repo, number, label, on)
    }

    fn review_bot(
        &self,
        repo: &ForgeSlug,
        number: u64,
        login: Login<'_>,
    ) -> Result<Activity, ForgeError> {
        review_bot::activity(&self.etags, None, repo, number, login)
    }

    fn resolve_thread(&self, _repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        review_bot::resolve(thread)
    }

    fn reply_to_thread(
        &self,
        _repo: &ForgeSlug,
        thread: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        review_bot::reply(thread, body)
    }

    fn merge(&self, repo: &ForgeSlug, number: u64, head: &str) -> Result<(), ForgeError> {
        let number = number.to_string();
        gh(&merge_args(repo, &number, head)).map(drop)
    }

    fn merge_queue(&self, repo: &ForgeSlug, number: u64) -> Result<QueueStanding, ForgeError> {
        queue::standing(repo, number)
    }

    fn disable_auto_merge(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        let number = number.to_string();
        let args = [
            "pr",
            "merge",
            &number,
            "--repo",
            repo.as_str(),
            "--disable-auto",
        ];
        gh(&args).map(drop)
    }

    fn rate_limit(&self) -> Result<RateLimits, ForgeError> {
        rate_limit::rate_limit()
    }
}

// A merge commit, and only of the head the ruling was about. No
// `--delete-branch`: gh would also switch branches in its working folder.
fn merge_args<'a>(repo: &'a ForgeSlug, number: &'a str, head: &'a str) -> [&'a str; 8] {
    [
        "pr",
        "merge",
        number,
        "--repo",
        repo.as_str(),
        "--merge",
        "--match-head-commit",
        head,
    ]
}

fn issue(config: Option<&Path>, repo: &ForgeSlug, number: u64) -> Result<Issue, ForgeError> {
    let number = number.to_string();
    parse_issue(&gh_as(
        config,
        &[
            "issue",
            "view",
            &number,
            "--repo",
            repo.as_str(),
            "--json",
            "title,body,labels,state,parent,blockedBy",
        ],
    )?)
}

fn pull_request(
    config: Option<&Path>,
    repo: &ForgeSlug,
    number: u64,
) -> Result<PullRequest, ForgeError> {
    let number = number.to_string();
    let pr = parse_pull_request(&gh_as(
        config,
        &[
            "pr",
            "view",
            &number,
            "--repo",
            repo.as_str(),
            "--json",
            "state,isDraft,headRefOid,statusCheckRollup,labels",
        ],
    )?)?;
    // `gh pr view` lists no check suites, so they are asked for apart, and
    // only when the rollup would let the pull request merge.
    if matches!(pr.checks, Checks::Pending(_) | Checks::Failed(_)) {
        return Ok(pr);
    }
    let suites = suites::of_commit(config, repo, &pr.head)?;
    Ok(PullRequest {
        checks: suites.fold(pr.checks),
        ..pr
    })
}

fn gh(args: &[&str]) -> Result<Vec<u8>, ForgeError> {
    gh_as(None, args)
}

// The variables that hand `gh` a login ahead of its config folder's
const LOGIN_VARS: [&str; 3] = ["GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN"];

/// `gh` with `args`, as the login in the config folder `config` where there is
/// one, and as the maintainer's own otherwise
fn gh_as(config: Option<&Path>, args: &[&str]) -> Result<Vec<u8>, ForgeError> {
    let output = gh_command(config)
        .args(args)
        .output()
        .map_err(|e| ForgeError::Spawn(e.to_string()))?;
    if !output.status.success() {
        return Err(failed::failure(&String::from_utf8_lossy(&output.stderr)));
    }
    Ok(output.stdout)
}

// `gh` as the login in `config` where there is one, printing the API's
// debug lines, which hold the headers a limit's answer carries
fn gh_command(config: Option<&Path>) -> std::process::Command {
    let mut command = crate::spawn::command("gh");
    if let Some(config) = config {
        // A token in the environment would win over the folder's.
        for var in LOGIN_VARS {
            command.env_remove(var);
        }
        command
            .env("GH_CONFIG_DIR", config)
            .env("GH_PROMPT_DISABLED", "1");
    }
    command.env("GH_DEBUG", "api").stdin(Stdio::null());
    command
}

fn unreadable(stdout: &[u8]) -> ForgeError {
    ForgeError::Unreadable(String::from_utf8_lossy(stdout).into())
}

fn parse_visibility(stdout: &[u8]) -> Result<Visibility, ForgeError> {
    #[derive(Deserialize)]
    struct View {
        visibility: String,
    }
    let view: View = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    match view.visibility.as_str() {
        "PUBLIC" => Ok(Visibility::Public),
        "PRIVATE" => Ok(Visibility::Private),
        "INTERNAL" => Ok(Visibility::Internal),
        _ => Err(unreadable(stdout)),
    }
}

fn parse_default_branch(stdout: &[u8]) -> Result<String, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct View {
        default_branch_ref: BranchRef,
    }
    #[derive(Deserialize)]
    struct BranchRef {
        name: String,
    }
    let view: View = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(view.default_branch_ref.name)
}

fn parse_label_names(stdout: &[u8]) -> Result<Vec<String>, ForgeError> {
    let labels: Vec<Label> = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(labels.into_iter().map(|l| l.name).collect())
}

fn parse_issue(stdout: &[u8]) -> Result<Issue, ForgeError> {
    #[derive(Deserialize)]
    struct View {
        title: String,
        body: Option<String>,
        labels: Vec<Label>,
        state: String,
        // Absent only from a view made without asking for them.
        #[serde(default)]
        parent: Option<Numbered>,
        #[serde(default, rename = "blockedBy")]
        blocked_by: Option<Nodes>,
    }
    #[derive(Deserialize)]
    struct Numbered {
        number: u64,
    }
    #[derive(Deserialize)]
    struct Nodes {
        nodes: Vec<Numbered>,
    }
    let view: View = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    let blocked_by = view.blocked_by.map_or_else(Vec::new, |b| b.nodes);
    Ok(Issue {
        title: view.title,
        body: view.body.unwrap_or_default(),
        labels: view.labels.into_iter().map(|l| l.name).collect(),
        open: view.state != "CLOSED",
        parent: view.parent.and_then(|p| TicketId::from_issue(p.number)),
        blocked_by: blocked_by
            .into_iter()
            .filter_map(|b| TicketId::from_issue(b.number))
            .collect(),
    })
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

fn pull_request_state(state: &str, stdout: &[u8]) -> Result<PullRequestState, ForgeError> {
    match state {
        "OPEN" => Ok(PullRequestState::Open),
        "MERGED" => Ok(PullRequestState::Merged),
        "CLOSED" => Ok(PullRequestState::Closed),
        _ => Err(unreadable(stdout)),
    }
}

fn parse_pull_request(stdout: &[u8]) -> Result<PullRequest, ForgeError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct View {
        state: String,
        is_draft: bool,
        head_ref_oid: String,
        status_check_rollup: Vec<Check>,
        #[serde(default)]
        labels: Vec<Label>,
    }
    let view: View = serde_json::from_slice(stdout).map_err(|_| unreadable(stdout))?;
    Ok(PullRequest {
        state: pull_request_state(&view.state, stdout)?,
        draft: view.is_draft,
        head: view.head_ref_oid,
        checks: checks(&view.status_check_rollup),
        labels: view.labels.into_iter().map(|l| l.name).collect(),
    })
}

// GitHub has two kinds of check: an Actions-style check run, and a commit
// status posted by an outside service such as CodeRabbit.
#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum Check {
    CheckRun {
        name: String,
        status: Option<String>,
        conclusion: Option<String>,
        #[serde(rename = "completedAt")]
        completed_at: Option<String>,
        #[serde(rename = "detailsUrl")]
        details_url: Option<String>,
        // Only in the board's listing, which counts each suite's check runs by it
        #[serde(rename = "checkSuite", default)]
        suite: Option<SuiteId>,
    },
    StatusContext {
        context: String,
        state: Option<String>,
    },
}

#[derive(Deserialize)]
struct SuiteId {
    id: String,
}

enum Outcome {
    Pending,
    Passed,
    Skipped,
    Failed,
}

// What GitHub writes as the `completedAt` of a check run that has not finished
const NOT_COMPLETED: &str = "0001-01-01T00:00:00Z";

impl Check {
    fn outcome(&self) -> (&str, Outcome) {
        match self {
            Self::CheckRun {
                name, conclusion, ..
            } => {
                let outcome = if self.finished() {
                    match conclusion.as_deref() {
                        Some("SKIPPED") => Outcome::Skipped,
                        Some("SUCCESS" | "NEUTRAL") => Outcome::Passed,
                        _ => Outcome::Failed,
                    }
                } else {
                    Outcome::Pending
                };
                (name, outcome)
            }
            Self::StatusContext { context, state } => {
                let outcome = match state.as_deref() {
                    Some("SUCCESS") => Outcome::Passed,
                    Some("FAILURE" | "ERROR") => Outcome::Failed,
                    _ => Outcome::Pending,
                };
                (context, outcome)
            }
        }
    }

    // A check run is finished when its status says so, or when it has a time
    // it completed at and a conclusion: GitHub sometimes leaves the status at
    // `IN_PROGRESS` or `QUEUED` after the run ended.
    fn finished(&self) -> bool {
        match self {
            Self::CheckRun {
                status,
                conclusion,
                completed_at,
                ..
            } => {
                status.as_deref() == Some("COMPLETED")
                    || (conclusion.as_deref().is_some_and(|c| !c.is_empty())
                        && (completed_at.as_deref())
                            .is_some_and(|at| !at.is_empty() && at != NOT_COMPLETED))
            }
            Self::StatusContext { .. } => false,
        }
    }
}

impl Check {
    // The check run that is still queued or in progress, which the gate
    // reruns past a cap if its workflow run finished: GitHub leaves a job of
    // a finished run in either state for good, and a commit status has no run.
    fn queued(&self) -> Option<Queued> {
        match self {
            Self::CheckRun {
                name,
                status: Some(status),
                details_url,
                ..
            } if matches!(status.as_str(), "QUEUED" | "IN_PROGRESS") && !self.finished() => {
                Some(Queued {
                    name: name.clone(),
                    run: details_url.as_deref().and_then(run_of),
                    running: status == "IN_PROGRESS",
                })
            }
            _ => None,
        }
    }
}

impl Check {
    // The check suite a check run names, where the query asked for it
    fn suite(&self) -> Option<&str> {
        match self {
            Self::CheckRun {
                suite: Some(suite), ..
            } => Some(&suite.id),
            _ => None,
        }
    }
}

// The workflow run a check run's link, `.../actions/runs/<id>/job/<id>`, names
fn run_of(details_url: &str) -> Option<u64> {
    let (_, tail) = details_url.split_once("/actions/runs/")?;
    tail.split('/').next()?.parse().ok()
}

// The whole run is read before a verdict, so a red run names every failure.
fn checks(rollup: &[Check]) -> Checks {
    if rollup.is_empty() {
        return Checks::None;
    }
    let mut failed = Vec::new();
    let mut ran = false;
    let mut pending = false;
    for check in rollup {
        match check.outcome() {
            (_, Outcome::Pending) => pending = true,
            (name, Outcome::Failed) => failed.push(name.to_owned()),
            (_, Outcome::Passed) => ran = true,
            (_, Outcome::Skipped) => {}
        }
    }
    if pending {
        return Checks::Pending(rollup.iter().filter_map(Check::queued).collect());
    }
    if failed.is_empty() {
        if ran { Checks::Passed } else { Checks::Skipped }
    } else {
        Checks::Failed(failed)
    }
}

#[cfg(test)]
mod tests;
