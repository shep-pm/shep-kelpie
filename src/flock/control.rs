//! `shep kelpie start`, `pause` and `status`: the runner's own triggers,
//! sent over the shepherd's socket
//!
//! `start` first starts the dog's sheep and the runner's when they are not
//! running, then waits for the runner to answer, since a runner just
//! started reads its settings before it opens its channel.

use std::path::Path;
use std::time::Duration;

use shep_client::shep_core::protocol::request::{ActionOutcome, ProcessInfo, Response};
use shep_client::shep_core::protocol::{Request, SelectorSpec};
use shep_client::shep_core::status::ProcStatus;
use shep_client::{Client, TRIGGER_DEADLINE};

use super::{Found, flock, kelpie_sheep, resume, tables};
use crate::dog;
use crate::runner::ProjectName;
use crate::settings::Settings;
use crate::shepherd;

/// How long `start` waits for a runner it started to answer
const STARTING: Duration = Duration::from_secs(30);

/// How often it asks while it waits
const ASK_AGAIN: Duration = Duration::from_millis(500);

/// The project whose settings name `root` as its checkout
///
/// # Errors
///
/// A message when no project, or more than one, runs from `root`.
pub async fn project_here(
    client: &Client,
    root: &Path,
    home: &Path,
) -> Result<ProjectName, String> {
    let here: Vec<String> = tables(client)
        .await?
        .into_iter()
        .filter(|(sheep, table)| {
            Settings::from_table(table, sheep, home, home).is_ok_and(|s| s.repo == root)
        })
        .map(|(sheep, _)| sheep)
        .collect();
    match here.as_slice() {
        [one] => ProjectName::try_from(one.as_str()).map_err(|e| e.to_string()),
        [] => Err(format!(
            "no project runs from {}: `shep kelpie add` sets one up",
            root.display()
        )),
        more => Err(format!(
            "{} run from {}, so name the one you mean",
            more.join(", "),
            root.display()
        )),
    }
}

/// Starts `project`'s runner and its dog when they are down, then sends it `start`
///
/// # Errors
///
/// A message when the project has no kelpie runner, or it does not answer
/// in time. Only kelpie's own sheep are ever started.
pub async fn start(client: &Client, project: &ProjectName) -> Result<Vec<String>, String> {
    let rows = flock(client).await?;
    let runner = kelpie_runner(client, &rows, project).await?;
    let adopted = rows
        .iter()
        .any(|r| r.name == dog::OLD_NAME && r.dog.is_some());
    let dog = match kelpie_sheep(client, &rows, dog::NAME, &["dog"]).await? {
        Some(dog) => Some(dog),
        None if adopted => {
            return Err(format!(
                "kelpie is adopted and enabled, and this flock has no `{}`: run `shep disable \
                 {}`, then `shep kelpie add`",
                dog::NAME,
                dog::OLD_NAME
            ));
        }
        None => kelpie_sheep(client, &rows, dog::OLD_NAME, &["dog"]).await?,
    };
    for found in dog.iter().chain([&runner]) {
        if found.row.status != ProcStatus::Online {
            resume(client, &found.row.name).await?;
        }
    }
    let runner = project.as_str();
    let waited = tokio::time::Instant::now();
    loop {
        match trigger(client, runner, "start").await? {
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
    match trigger(client, runner, "pause").await? {
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
        let line = match trigger(client, &sheep, "status").await? {
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

// `project`'s runner, refusing a name that is not one: a sheep with no
// kelpie table, or one kelpie did not start as `runner <project>`.
async fn kelpie_runner(
    client: &Client,
    rows: &[ProcessInfo],
    project: &ProjectName,
) -> Result<Found, String> {
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
pub(crate) enum Answered {
    /// The runner's own answer, which is always a JSON object
    Runner(String),
    /// A plain-text answer: shep-channel's `unknown action`, from a runner
    /// that has opened its channel and not yet taken its actions
    Starting,
    /// No answer: the sheep is not running, or has no such name
    Down,
}

// What `sheep` made of `action`, under `shep trigger`'s own budget.
pub(crate) async fn trigger(
    client: &Client,
    sheep: &str,
    action: &str,
) -> Result<Answered, String> {
    let request = Request::Trigger {
        selector: SelectorSpec::Name(sheep.to_owned()),
        action: action.to_owned(),
        params: None,
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
