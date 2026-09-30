//! `shep kelpie start`, `pause` and `status`: the runner's own triggers,
//! sent over the shepherd's socket
//!
//! `start` first checks the adopted kelpie holds the leases, starts the
//! runner's sheep when it is not running, then waits for the runner to
//! answer, since a runner just started reads its settings before it opens
//! its channel.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use shep_client::shep_core::protocol::request::{ActionOutcome, ProcessInfo, Response};
use shep_client::shep_core::protocol::{Request, SelectorSpec};
use shep_client::shep_core::status::ProcStatus;
use shep_client::{Client, TRIGGER_DEADLINE};

use super::{flock, kelpie_sheep, resume, tables};
use crate::runner::ProjectName;
use crate::settings::Settings;
use crate::shepherd;

/// How long `start` waits for a runner it started to answer
const STARTING: Duration = Duration::from_secs(30);

/// How often it asks while it waits
const ASK_AGAIN: Duration = Duration::from_millis(500);

/// The project whose repo holds `folder`: its checkout, or a worktree of it
///
/// # Errors
///
/// A message when no project, or more than one, has its repo there, listing
/// the projects so one can be named with `-p`.
pub async fn project_here(
    client: &Client,
    folder: &Path,
    home: &Path,
) -> Result<ProjectName, String> {
    let roots = checkout_roots(folder);
    let tables = tables(client).await?;
    let here: Vec<&String> = tables
        .iter()
        .filter(|(sheep, table)| {
            Settings::from_table(table, sheep, home, home).is_ok_and(|s| {
                let repo = s.repo.canonicalize().unwrap_or(s.repo);
                roots.contains(&repo)
            })
        })
        .map(|(sheep, _)| sheep)
        .collect();
    let projects: Vec<&str> = tables.keys().map(String::as_str).collect();
    match here.as_slice() {
        [one] => ProjectName::try_from(one.as_str()).map_err(|e| e.to_string()),
        [] if projects.is_empty() => {
            Err("no projects: `shep kelpie add` in a checkout sets one up".into())
        }
        [] => Err(format!(
            "no project's repo holds {}, so name one with `-p <project>`: {}",
            folder.display(),
            projects.join(", ")
        )),
        more => Err(format!(
            "{} all have their repo at {}, so name one with `-p <project>`",
            more.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            folder.display()
        )),
    }
}

// The checkouts `folder` belongs to: its own top folder, and for a worktree
// the checkout it was made from. None when it is in no git checkout.
fn checkout_roots(folder: &Path) -> Vec<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(folder)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output.map(|o| (o.status.success(), o.stdout)) else {
        return Vec::new();
    };
    let (true, stdout) = output else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&stdout);
    let mut lines = text.lines().map(PathBuf::from);
    let (Some(top), common) = (lines.next(), lines.next()) else {
        return Vec::new();
    };
    let main = common
        .filter(|c| c.ends_with(".git"))
        .and_then(|c| c.parent().map(Path::to_owned));
    [Some(top), main]
        .into_iter()
        .flatten()
        .map(|root| root.canonicalize().unwrap_or(root))
        .collect()
}

/// Starts `project`'s runner when it is down, then sends it `start`
///
/// # Errors
///
/// A message when the adopted kelpie is not running as the dog with its
/// channel, the project has no kelpie runner, or it does not answer in
/// time. Only kelpie's own runners are ever started.
pub async fn start(client: &Client, project: &ProjectName) -> Result<Vec<String>, String> {
    let rows = flock(client).await?;
    let runner = kelpie_runner(client, &rows, project).await?;
    // A runner with no dog is never granted a lease.
    if let Some(down) = super::dog_down(&rows) {
        return Err(down);
    }
    if runner.status != ProcStatus::Online {
        resume(client, &runner.name).await?;
    }
    let runner = project.as_str();
    let waited = tokio::time::Instant::now();
    loop {
        match trigger(client, runner, "start", None).await? {
            Answered::Runner(body) => return Ok(vec![body]),
            _ if waited.elapsed() < STARTING => tokio::time::sleep(ASK_AGAIN).await,
            _ => {
                return Err(format!(
                    "{runner}'s runner did not answer in {}s: `shep bleats {runner}` says why",
                    STARTING.as_secs()
                ));
            }
        }
    }
}

/// Sends `project`'s runner `pause`
///
/// # Errors
///
/// A message when the project has no kelpie runner, or it is not running.
pub async fn pause(client: &Client, project: &ProjectName) -> Result<Vec<String>, String> {
    kelpie_runner(client, &flock(client).await?, project).await?;
    let runner = project.as_str();
    match trigger(client, runner, "pause", None).await? {
        Answered::Runner(body) => Ok(vec![body]),
        Answered::Starting => Err(format!(
            "{runner}'s runner is starting: ask again in a moment"
        )),
        Answered::Down => Err(format!("{runner}'s runner is not running")),
    }
}

/// Every project's status, one line each: its name and its runner's answer
///
/// # Errors
///
/// A message when the shepherd cannot list the projects.
pub async fn status(client: &Client) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    for sheep in tables(client).await?.into_keys() {
        let line = match trigger(client, &sheep, "status", None).await? {
            Answered::Runner(body) => format!("{sheep}: {body}"),
            Answered::Starting => format!("{sheep}: starting"),
            Answered::Down => format!("{sheep}: not running"),
        };
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push("no projects: `shep kelpie add` in a checkout sets one up".into());
    }
    Ok(lines)
}

/// Sends `project`'s runner `action` with `params`, as `shep trigger` would
///
/// # Errors
///
/// A message when the project has no kelpie runner, it is not running, or
/// it refused the trigger, with its reason.
pub async fn send(
    client: &Client,
    project: &ProjectName,
    action: &str,
    params: Option<&str>,
) -> Result<Vec<String>, String> {
    kelpie_runner(client, &flock(client).await?, project).await?;
    let runner = project.as_str();
    match trigger(client, runner, action, params).await? {
        Answered::Runner(body) => match refusal(&body) {
            Some(why) => Err(why),
            None => Ok(vec![body]),
        },
        Answered::Starting => Err(format!(
            "{runner}'s runner is starting: ask again in a moment"
        )),
        Answered::Down => Err(format!("{runner}'s runner is not running")),
    }
}

// The `error` a runner's answer carries, if any.
fn refusal(body: &str) -> Option<String> {
    let answer: serde_json::Value = serde_json::from_str(body).ok()?;
    match answer.get("error")? {
        serde_json::Value::String(why) => Some(why.clone()),
        serde_json::Value::Null => None,
        why => Some(why.to_string()),
    }
}

// `project`'s runner, refusing a name that is not one: a sheep with no
// kelpie table, or one kelpie did not start as `runner <project>`.
async fn kelpie_runner(
    client: &Client,
    rows: &[ProcessInfo],
    project: &ProjectName,
) -> Result<ProcessInfo, String> {
    let name = project.as_str();
    let not_one =
        || format!("no kelpie runner named {name} in this flock: `shep kelpie add` sets one up");
    if !tables(client).await?.contains_key(name) {
        return Err(not_one());
    }
    match kelpie_sheep(client, rows, name, &["runner", name]).await {
        Ok(Some(found)) => Ok(found),
        Ok(None) | Err(_) => Err(not_one()),
    }
}

/// What a runner's sheep made of a trigger
enum Answered {
    /// The runner's own answer, which is always a JSON object
    Runner(String),
    /// A plain-text answer: shep-channel's `unknown action`, from a runner
    /// that has opened its channel and not yet taken its actions
    Starting,
    /// No answer: the sheep is not running, or has no such name
    Down,
}

// What `sheep` made of `action`, under `shep trigger`'s own budget.
async fn trigger(
    client: &Client,
    sheep: &str,
    action: &str,
    params: Option<&str>,
) -> Result<Answered, String> {
    let request = Request::Trigger {
        selector: SelectorSpec::Name(sheep.to_owned()),
        action: action.to_owned(),
        params: params.map(str::to_owned),
    };
    let rows = match client
        .request_with_deadline(request, Some(TRIGGER_DEADLINE))
        .await
    {
        Ok(Response::Triggered(rows)) => rows,
        Err(e) if shepherd::names_no_sheep(&e) => return Ok(Answered::Down),
        Ok(other) => return Err(format!("the shepherd answered {other:?}")),
        Err(e) => return Err(format!("cannot send {sheep} `{action}`: {e}")),
    };
    Ok(match rows.into_iter().next().map(|row| row.outcome) {
        Some(ActionOutcome::Replied { body })
            if serde_json::from_str::<serde_json::Map<_, _>>(&body).is_ok() =>
        {
            Answered::Runner(body)
        }
        Some(ActionOutcome::Replied { .. }) => Answered::Starting,
        _ => Answered::Down,
    })
}

#[cfg(test)]
mod tests;
