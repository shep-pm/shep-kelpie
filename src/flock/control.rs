//! `shep kelpie start`, `pause`, `status` and the triggers the other verbs
//! send, over the shepherd's socket
//!
//! A project runs while its runner's sheep does: shep's own state is the
//! run state, so `shep start <project>` runs it too. `start` first checks
//! the adopted kelpie holds the leases, starts the runner's sheep when it is
//! not running, then waits for the runner to answer, since a runner just
//! started reads its settings before it opens its channel. `pause` drains
//! the runner, waits for its calls and any merge to end, then stops its
//! sheep. `rule` leaves the answer for a stopped runner to act on when it
//! starts.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use shep_client::shep_core::protocol::request::{ActionOutcome, ProcessInfo, Response};
use shep_client::shep_core::protocol::{Request, SelectorSpec};
use shep_client::shep_core::status::ProcStatus;
use shep_client::{Client, TRIGGER_DEADLINE};

use super::{flock, halt, kelpie_sheep, resume, tables};
use crate::runner::{ProjectName, leave_answer};
use crate::settings::Settings;
use crate::shepherd;
use crate::upgrade::drain::{self, Purpose};
use crate::upgrade::restart::{Interrupt, Patience, interrupted, wait_out_merges};

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
                let repo = s.git.checkout.canonicalize().unwrap_or(s.git.checkout);
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
    let output = crate::spawn::command("git")
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

/// Starts `project`'s runner when it is down, then waits for it to answer
/// `status`, which it answers
///
/// A runner still on a build from before the run state went, whose
/// `status` says it is paused, is sent that build's own `start`.
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
        match trigger(client, runner, "status", None).await? {
            Answered::Runner(body) if !paused(&body) => return Ok(vec![body]),
            Answered::Runner(_) => {
                return match trigger(client, runner, "start", None).await? {
                    Answered::Runner(body) => Ok(vec![body]),
                    other => Err(unanswered(runner, other)),
                };
            }
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

// Whether a runner's `status` says it is paused, as only a build from
// before the run state was the sheep's can.
fn paused(body: &str) -> bool {
    let status = serde_json::from_str::<serde_json::Value>(body);
    status.is_ok_and(|status| status["run"] == "paused")
}

/// Lets `project`'s runner end what it has running, then stops its sheep
///
/// The runner is drained, so it starts no new call, and waited on until no
/// call runs and no merge is in flight, as `upgrade` waits before a restart
/// and bounded by `patience` the same way. Each line of what it waits on
/// goes to `say`. A session the maintainer attached is their own process,
/// not the runner's: it goes on, and its work item stays held until it ends.
///
/// # Errors
///
/// A message when the project has no kelpie runner, it does not come up
/// from starting, the wait runs out or `interrupt` comes first, or the
/// shepherd refuses the stop. The runner is then sent `undrain` and left
/// running. Once the stop is sent, `interrupt` is no longer heard.
pub async fn pause(
    client: &Client,
    project: &ProjectName,
    patience: Patience,
    interrupt: Interrupt,
    say: &mut dyn FnMut(String),
) -> Result<Vec<String>, String> {
    let runner = kelpie_runner(client, &flock(client).await?, project).await?;
    let name = project.as_str();
    if !matches!(runner.status, ProcStatus::Online | ProcStatus::Starting) {
        return Ok(vec![format!(
            "{name}'s runner is {}: `shep kelpie start {name}` runs it",
            runner.status
        )]);
    }
    let drained = async {
        come_up(client, name, patience, &mut *say).await?;
        drain::drain(client, name, patience, Purpose::Pause, &mut *say).await?;
        wait_out_merges(client, &[name], patience, Purpose::Pause, &mut *say).await?;
        Ok(match trigger(client, name, "status", None).await? {
            Answered::Runner(body) => Some(attached(&body)),
            Answered::Down => None,
            _ => Some(Vec::new()),
        })
    };
    let drained = tokio::select! {
        drained = drained => drained,
        by = interrupted(interrupt) => Err(format!("stopped by {by} while `{name}` was pausing")),
    };
    let attached = match drained {
        Ok(Some(attached)) => attached,
        // Gone while it was waited on, as a stop from elsewhere or a crash leaves it.
        Ok(None) if !up(client, name).await? => {
            return Ok(vec![format!(
                "{name}'s runner stopped while it was being paused: `shep kelpie start {name}` \
                 runs it again"
            )]);
        }
        Ok(None) => Vec::new(),
        Err(e) => {
            let undrained = drain::undrain(client, name, Purpose::Pause).await;
            return Err(format!("{e}{undrained}"));
        }
    };
    if let Err(e) = halt(client, name).await {
        let undrained = drain::undrain(client, name, Purpose::Pause).await;
        return Err(format!("{e}{undrained}"));
    }
    let mut lines: Vec<String> = attached
        .into_iter()
        .map(|issue| {
            format!(
                "#{issue} is attached in your terminal: its session goes on, and the work item \
                 stays held until it ends"
            )
        })
        .collect();
    lines.push(format!(
        "{name}'s runner is stopped: `shep kelpie start {name}` runs it again"
    ));
    Ok(lines)
}

// Whether shep shows `name` running.
async fn up(client: &Client, name: &str) -> Result<bool, String> {
    let rows = flock(client).await?;
    let row = rows.iter().find(|row| row.name == name);
    Ok(row.is_some_and(|row| matches!(row.status, ProcStatus::Online | ProcStatus::Starting)))
}

// Returns once `name` answers `status` as a runner or is down, saying it
// waits while it starts: a runner still opening takes no `drain`.
async fn come_up(
    client: &Client,
    name: &str,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let waited = tokio::time::Instant::now();
    let mut told = false;
    loop {
        if let Answered::Runner(_) | Answered::Down = trigger(client, name, "status", None).await? {
            return Ok(());
        }
        if waited.elapsed() >= patience.start {
            return Err(format!(
                "`{name}` did not answer in {}s, so it was not stopped: `shep bleats {name}` says \
                 why",
                patience.start.as_secs()
            ));
        }
        if !told {
            told = true;
            say(format!("waiting: `{name}` is starting"));
        }
        tokio::time::sleep(patience.poll).await;
    }
}

// The issues of the work items a runner's `status` shows attached.
fn attached(status: &str) -> Vec<u64> {
    let Ok(status) = serde_json::from_str::<serde_json::Value>(status) else {
        return Vec::new();
    };
    let items = status["work_items"].as_array().into_iter().flatten();
    items
        .filter(|item| !item["attached"].is_null())
        .filter_map(|item| item["issue"].as_u64())
        .collect()
}

/// Sends `project`'s runner `rule` with `params`, or leaves the answer in
/// `answers` for it to act on when it starts, when it is stopped
///
/// # Errors
///
/// A message when the project has no kelpie runner, the runner refused the
/// answer, with its reason, it is starting or did not answer, or a stopped
/// runner's answer cannot be left.
pub async fn rule(
    client: &Client,
    project: &ProjectName,
    params: &str,
    answers: &Path,
) -> Result<Vec<String>, String> {
    kelpie_runner(client, &flock(client).await?, project).await?;
    let name = project.as_str();
    let (id, said) = params.split_once(' ').unwrap_or((params, ""));
    let ruled = format!("ruling {id} on {name}: {said}");
    match trigger(client, name, "rule", Some(params)).await? {
        Answered::Runner(body) => match refusal(&body) {
            Some(why) => Err(why),
            None => Ok(vec![ruled]),
        },
        Answered::Down => {
            leave_answer(answers, params)
                .map_err(|e| format!("cannot leave the answer for {name}'s runner: {e}"))?;
            Ok(vec![
                ruled,
                format!(
                    "{name}'s runner is stopped, so it acts on the answer when it starts: \
                     `shep kelpie start {name}`"
                ),
            ])
        }
        other => Err(unanswered(name, other)),
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
            Answered::Runner(body) if draining(&body) => format!("{sheep} (draining): {body}"),
            Answered::Runner(body) => format!("{sheep}: {body}"),
            Answered::Starting => format!("{sheep}: starting"),
            Answered::Down => format!("{sheep}: not running"),
            Answered::TimedOut => format!("{sheep}: running, and did not answer in time"),
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
        other => Err(unanswered(runner, other)),
    }
}

// Why `runner` gave no answer of its own.
fn unanswered(runner: &str, answered: Answered) -> String {
    match answered {
        Answered::Runner(body) => body,
        Answered::Starting => format!("{runner}'s runner is starting: ask again in a moment"),
        Answered::Down => format!("{runner}'s runner is not running"),
        Answered::TimedOut => format!("{runner}'s runner did not answer in time"),
    }
}

// Whether a runner's answer shows it draining.
fn draining(body: &str) -> bool {
    let answer = serde_json::from_str::<serde_json::Value>(body);
    answer.is_ok_and(|answer| answer.get("draining").is_some_and(|d| !d.is_null()))
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
pub(crate) enum Answered {
    /// The runner's own answer, which is always a JSON object
    Runner(String),
    /// A plain-text answer: shep-channel's `unknown action`, from a runner
    /// that has opened its channel and not yet taken its actions
    Starting,
    /// No answer: the sheep is not running, or has no such name
    Down,
    /// The action was delivered and nothing came back in time: the sheep is
    /// running, and what it would have said is unknown
    TimedOut,
}

// What `sheep` made of `action`, under `shep trigger`'s own budget.
pub(crate) async fn trigger(
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
        Some(ActionOutcome::TimedOut) => Answered::TimedOut,
        _ => Answered::Down,
    })
}

#[cfg(test)]
mod tests;
