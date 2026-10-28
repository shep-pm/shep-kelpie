//! `shep kelpie start`, `pause`, `finish`, `status` and the triggers the
//! other verbs send, over the shepherd's socket
//!
//! A project runs while its runner's sheep does, so `shep start <project>`
//! runs it too. `start` checks the adopted kelpie holds the leases, starts the
//! runner's sheep if it is down, then waits for the runner to answer, since it
//! reads its settings before it opens its channel. `pause` drains the runner,
//! waits for its calls and any merge to end, then stops its sheep. `finish`
//! lets open work items end and picks nothing new, then the runner stops its
//! own sheep. `rule` leaves the answer for a stopped runner to act on.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use shep_client::TRIGGER_DEADLINE;
use shep_client::shep_core::protocol::request::{ActionOutcome, DogSource, ProcessInfo, Response};
use shep_client::shep_core::protocol::{Request, SelectorSpec};
use shep_client::shep_core::status::ProcStatus;

use crate::shepherd::Connection;

use super::{Checkout, flock, halt, kelpie_sheep, resume, tables};
use crate::home::ProjectName;
use crate::runner::leave_answer;
use crate::settings::{ForgeSlug, Settings};
use crate::shepherd;
use crate::upgrade::drain::{self, Purpose};
use crate::upgrade::restart::{Interrupt, Patience, interrupted, wait_out_merges};

/// How long `start` waits for a runner it started to answer
const STARTING: Duration = Duration::from_secs(30);

/// How often it asks while it waits
const ASK_AGAIN: Duration = Duration::from_millis(500);

/// How long the installed kelpie has to say which build it is, for `status`
const BUILD_LIMIT: Duration = Duration::from_secs(10);

/// The project whose repo holds `folder`: its checkout, or a worktree of it
///
/// Failing that, the one project whose `git.remote` is the `origin` of the
/// clone `folder` is in, so the maintainer's own clone finds its project.
///
/// # Errors
///
/// A message when no project, or more than one, has its repo there, listing
/// the projects so one can be named with `-p`.
pub async fn project_here(
    client: &dyn Connection,
    folder: &Path,
    home: &Path,
) -> Result<ProjectName, String> {
    let roots = checkout_roots(folder);
    let tables = tables(client).await?;
    let settings: Vec<(&String, Settings)> = tables
        .iter()
        .filter_map(|(sheep, table)| {
            Settings::from_table(table, sheep, home, home)
                .ok()
                .map(|s| (sheep, s))
        })
        .collect();
    let here: Vec<&String> = settings
        .iter()
        .filter(|(_, s)| {
            let repo = crate::canonical(&s.git.checkout);
            roots.contains(&repo.unwrap_or_else(|_| s.git.checkout.clone()))
        })
        .map(|(sheep, _)| *sheep)
        .collect();
    let projects: Vec<&str> = tables.keys().map(String::as_str).collect();
    if here.is_empty()
        && let Some(found) = by_origin(folder, &settings)?
    {
        return Ok(found);
    }
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

// The repo `settings` works: its `git.remote`, or the `origin` of its checkout.
fn repo_of(settings: &Settings) -> Option<ForgeSlug> {
    (settings.git.remote.clone())
        .or_else(|| Checkout::of(&settings.git.checkout).ok().map(|c| c.forge))
}

/// The repo of every project in the flock
///
/// A table that does not parse still gives its repo when it names `git.remote`
/// or a checkout with an `origin`.
///
/// # Errors
///
/// A message when the shepherd cannot be asked for the projects' tables, or
/// when a project's repo cannot be told, since a post could then name a private
/// repo of the flock unseen.
pub async fn repos(client: &dyn Connection, home: &Path) -> Result<Vec<ForgeSlug>, String> {
    let tables = tables(client).await?;
    tables
        .iter()
        .map(|(sheep, table)| {
            let repo = match Settings::from_table(table, sheep, home, home) {
                Ok(settings) => repo_of(&settings),
                Err(_) => raw_repo_of(table, home),
            };
            repo.ok_or_else(|| {
                format!(
                    "cannot tell which repo project {sheep} works, so kelpie cannot keep its \
                     name off a public repo: fix its settings, or remove the project"
                )
            })
        })
        .collect()
}

// The repo a project table names, read without parsing the rest of it.
fn raw_repo_of(
    table: &serde_json::Map<String, serde_json::Value>,
    home: &Path,
) -> Option<ForgeSlug> {
    let git = table.get("git")?.as_object()?;
    let text = |key: &str| git.get(key)?.as_str().map(str::to_owned);
    if let Some(remote) = text("remote") {
        return ForgeSlug::try_from(remote).ok();
    }
    let checkout = text("checkout")?;
    let folder = match checkout.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(checkout),
    };
    Checkout::of(&folder).ok().map(|c| c.forge)
}

// The one project whose `git.remote` is `folder`'s `origin`, for a clone that
// is no project's checkout. A project with no `git.remote` set has its
// checkout's own `origin` stand in, as its runner reads it. None when `folder`
// has no GitHub `origin` or no project has it.
fn by_origin(
    folder: &Path,
    settings: &[(&String, Settings)],
) -> Result<Option<ProjectName>, String> {
    let Ok(clone) = Checkout::of(folder) else {
        return Ok(None);
    };
    let matching: Vec<&str> = settings
        .iter()
        .filter(|(_, s)| {
            // GitHub owner and repo names are case-insensitive.
            repo_of(s)
                .is_some_and(|forge| forge.as_str().eq_ignore_ascii_case(clone.forge.as_str()))
        })
        .map(|(sheep, _)| sheep.as_str())
        .collect();
    match matching.as_slice() {
        [] => Ok(None),
        [one] => ProjectName::try_from(*one)
            .map(Some)
            .map_err(|e| e.to_string()),
        more => Err(format!(
            "{} all have {} as their remote, so name one with `-p <project>`",
            more.join(", "),
            clone.forge.as_str()
        )),
    }
}

// The checkouts `folder` belongs to: its own top folder, and for a worktree
// the checkout it was made from. None when it is in no git checkout.
fn checkout_roots(folder: &Path) -> Vec<PathBuf> {
    let output = super::git_in(folder)
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
        .map(|root| crate::canonical(&root).unwrap_or(root))
        .collect()
}

/// Starts `project`'s runner when it is down, then waits for it to answer `status`
///
/// A runner whose `status` says it is finishing, or finished and not yet
/// stopped, is sent `start`, so the board picks again. So is a paused one on
/// an older build, whose own `start` runs it.
///
/// # Errors
///
/// A message when the adopted kelpie is not running as the dog with its
/// channel, the project has no kelpie runner, or it does not answer `status`
/// in time. A runner that takes `start` and answers late is no error: the line
/// says `start` has most likely been taken.
pub async fn start(client: &dyn Connection, project: &ProjectName) -> Result<Vec<String>, String> {
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
            Answered::Runner(body) if !held(&body) => return Ok(vec![body]),
            Answered::Runner(_) => {
                return match trigger(client, runner, "start", None).await? {
                    Answered::Runner(body) => refusal(&body).map_or(Ok(vec![body]), Err),
                    Answered::TimedOut => Ok(vec![late(runner, "start", &check_status(runner))]),
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

// Whether a runner's `status` says it picks nothing until `start`: finishing
// or finished, or paused, as only a build from before the run state was the
// sheep's can be.
fn held(body: &str) -> bool {
    let run = run(body);
    matches!(run.as_deref(), Some("paused" | "finishing" | "finished"))
}

// What a runner's `status` says under `run`, if anything.
fn run(body: &str) -> Option<String> {
    let status = serde_json::from_str::<serde_json::Value>(body).ok()?;
    status["run"].as_str().map(str::to_owned)
}

/// Puts `project`'s runner in finishing: open work items run to their end
///
/// The board picks nothing new, and the runner then stops its sheep, at once
/// with no work item open. A runner still starting is waited on as `pause`
/// waits, saying so to `say`; one not running is left alone. Adopted pull
/// requests still waiting for a slot stay waiting for the next `start`.
///
/// # Errors
///
/// A message when the project has no kelpie runner, it does not come up within
/// `patience`, the runner refused `finish`, or it did not answer `status`. A
/// runner busy past the deadline is no error: the line says `finish` has most
/// likely been taken. [`Format::Json`] returns the runner's answer as it came.
pub async fn finish(
    client: &dyn Connection,
    project: &ProjectName,
    format: Format,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<Vec<String>, String> {
    let name = project.as_str();
    let runner = match kelpie_runner(client, &flock(client).await?, project).await {
        Err(e) if name == "all" && !tables(client).await?.contains_key(name) => {
            return Err(format!("{e}; finish --all finishes every project"));
        }
        found => found?,
    };
    if !matches!(runner.status, ProcStatus::Online | ProcStatus::Starting) {
        let said = format!(
            "{name}'s runner is {}, so it has nothing to finish",
            runner.status
        );
        return Ok(vec![said_as(format, name, said)]);
    }
    come_up(client, name, patience, say).await?;
    let body = match trigger(client, name, "finish", None).await? {
        Answered::Runner(body) => body,
        Answered::TimedOut => {
            let said = late(name, "finish", &check_status(name));
            return Ok(vec![said_as(format, name, said)]);
        }
        other => return Err(unanswered(name, other)),
    };
    if let Some(why) = refusal(&body) {
        return Err(why);
    }
    if format == Format::Json {
        return Ok(vec![body]);
    }
    let open = listed(&body, "work_items", "issue");
    let mut lines = vec![match open.as_slice() {
        [] => format!("{name} has no work item open, so its runner stops now"),
        open => format!(
            "{name} is finishing {}: the board picks nothing new, and the runner stops once \
             they end. `shep kelpie start {name}` picks again",
            open.join(", ")
        ),
    }];
    let adopted = listed(&body, "adopted", "pull_request");
    if !adopted.is_empty() {
        lines.push(format!(
            "adopted and waiting for a slot, {} stay waiting until `shep kelpie start {name}`",
            adopted.join(", ")
        ));
    }
    Ok(lines)
}

/// What [`finish_all`] made of each project
#[derive(Debug, PartialEq, Eq)]
pub struct FinishedAll {
    /// The lines of the projects that took `finish` or had nothing to finish
    pub lines: Vec<String>,
    /// A line, the project and its reason, for each runner that refused or did not answer
    pub refused: Vec<String>,
}

/// [`finish`] for every project in the shepherd's book, one after the other
///
/// With [`Format::Json`] each project's answer is a line behind its name, as
/// [`status`] prints them. A project with no work to finish or no runner
/// running counts as done, and one that refuses does not stop the rest.
///
/// # Errors
///
/// A message when the shepherd cannot list the projects.
pub async fn finish_all(
    client: &dyn Connection,
    format: Format,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<FinishedAll, String> {
    let mut done = FinishedAll {
        lines: Vec::new(),
        refused: Vec::new(),
    };
    for sheep in tables(client).await?.into_keys() {
        let project = match ProjectName::try_from(sheep.as_str()) {
            Ok(project) => project,
            Err(why) => {
                done.refused.push(format!("{sheep}: {why}"));
                continue;
            }
        };
        match finish(client, &project, format, patience, say).await {
            Ok(lines) if format == Format::Json => {
                done.lines
                    .extend(lines.iter().map(|body| json_status(&sheep, body)));
            }
            Ok(lines) => done.lines.extend(lines),
            Err(why) => done.refused.push(format!("{sheep}: {why}")),
        }
    }
    if done.lines.is_empty() && done.refused.is_empty() {
        done.lines.push(no_projects());
    }
    Ok(done)
}

fn no_projects() -> String {
    "no projects: `shep kelpie add` in a checkout sets one up".into()
}

// The `key` of each entry in the `list` a runner's `status` shows, each as the
// maintainer reads it: `#7` for a GitHub ticket or a pull request, `k12` for a
// local ticket.
fn listed(status: &str, list: &str, key: &str) -> Vec<String> {
    let Ok(status) = serde_json::from_str::<serde_json::Value>(status) else {
        return Vec::new();
    };
    let entries = status[list].as_array().into_iter().flatten();
    entries.filter_map(|entry| shown(&entry[key])).collect()
}

// A status field that names a ticket or a pull request, as the maintainer
// writes it. A ticket id is a string; a pull request number is still a number.
fn shown(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(
            text.strip_prefix('k')
                .map_or_else(|| format!("#{text}"), |_| text.clone()),
        ),
        serde_json::Value::Number(number) => number.as_u64().map(|n| format!("#{n}")),
        _ => None,
    }
}

/// Lets `project`'s runner end what it has running, then stops its sheep
///
/// The runner is drained and waited on until no call runs and no merge is in
/// flight, bounded by `patience` as `upgrade` is. Each line it waits on goes to
/// `say`. A session the maintainer attached goes on, and its work item stays
/// held until it ends. A finishing runner is sent `pausing` just before the
/// stop, so it comes back picking, and `finish` again if the stop is refused.
///
/// # Errors
///
/// A message when the project has no kelpie runner, it does not come up, the
/// wait runs out or `interrupt` comes first, or the shepherd refuses the stop.
/// The runner is then sent `undrain keep` and left running. Once the stop is
/// sent, `interrupt` is no longer heard.
pub async fn pause(
    client: &dyn Connection,
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
            Answered::Runner(body) => {
                let finishing = run(&body).is_some_and(|run| run == "finishing");
                Some((attached(&body), finishing))
            }
            Answered::Down => None,
            _ => Some((Vec::new(), false)),
        })
    };
    let drained = tokio::select! {
        drained = drained => drained,
        by = interrupted(interrupt) => Err(format!("stopped by {by} while `{name}` was pausing")),
    };
    let (attached, finishing) = match drained {
        Ok(Some(found)) => found,
        // Gone while it was waited on, as a stop from elsewhere or a crash leaves it.
        Ok(None) if !up(client, name).await? => {
            return Ok(vec![format!(
                "{name}'s runner stopped while it was being paused: `shep kelpie start {name}` \
                 runs it again"
            )]);
        }
        Ok(None) => (Vec::new(), false),
        Err(e) => {
            let undrained = drain::undrain(client, name, Purpose::Pause).await;
            return Err(format!("{e}{undrained}"));
        }
    };
    // A pause is the stronger stop: the runner comes back picking.
    if finishing && let Err(e) = ask(client, name, "pausing").await {
        let undrained = drain::undrain(client, name, Purpose::Pause).await;
        return Err(format!("{e}{undrained}"));
    }
    if let Err(e) = halt(client, name).await {
        let refinished = match finishing {
            true => match ask(client, name, "finish").await {
                Ok(()) => ", and it was sent `finish`, so it goes on finishing".to_owned(),
                Err(why) => format!(", and it could not be sent `finish` again: {why}"),
            },
            false => String::new(),
        };
        let undrained = drain::undrain(client, name, Purpose::Pause).await;
        return Err(format!("{e}{refinished}{undrained}"));
    }
    let mut lines: Vec<String> = attached
        .into_iter()
        .map(|ticket| {
            format!(
                "{ticket} is attached in your terminal: its session goes on, and the work item \
                 stays held until it ends"
            )
        })
        .collect();
    lines.push(format!(
        "{name}'s runner is stopped: `shep kelpie start {name}` runs it again"
    ));
    Ok(lines)
}

// Sends `name` `action`, refusing any answer but the runner's own with no error.
async fn ask(client: &dyn Connection, name: &str, action: &str) -> Result<(), String> {
    match trigger(client, name, action, None).await? {
        Answered::Runner(body) => refusal(&body).map_or(Ok(()), Err),
        other => Err(unanswered(name, other)),
    }
}

// Whether shep shows `name` running.
async fn up(client: &dyn Connection, name: &str) -> Result<bool, String> {
    let rows = flock(client).await?;
    let row = rows.iter().find(|row| row.name == name);
    Ok(row.is_some_and(|row| matches!(row.status, ProcStatus::Online | ProcStatus::Starting)))
}

// Returns once `name` answers `status` as a runner or is down, saying it
// waits while it starts: a runner still opening takes no `drain`.
async fn come_up(
    client: &dyn Connection,
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

// The work items a runner's `status` shows attached, each named as the
// maintainer writes its ticket.
fn attached(status: &str) -> Vec<String> {
    let Ok(status) = serde_json::from_str::<serde_json::Value>(status) else {
        return Vec::new();
    };
    let items = status["work_items"].as_array().into_iter().flatten();
    items
        .filter(|item| !item["attached"].is_null())
        .filter_map(|item| shown(&item["issue"]))
        .collect()
}

/// Sends `project`'s runner `rule` with `params`, or leaves the answer in
/// `answers` for it to act on when it starts, when it is stopped
///
/// # Errors
///
/// A message when the project has no kelpie runner, the runner refused the
/// answer, with its reason, it is starting, or a stopped runner's answer
/// cannot be left. A runner busy past the deadline is no error: the answer
/// has most likely been taken, and the second line says how to check.
pub async fn rule(
    reach: &Reach,
    client: &dyn Connection,
    project: &ProjectName,
    params: &str,
    answers: &Path,
) -> Result<Vec<String>, String> {
    kelpie_runner(client, &flock(client).await?, project).await?;
    let name = project.as_str();
    let (id, said) = params.split_once(' ').unwrap_or((params, ""));
    let ruled = format!("ruling {id} on {name}: {said}");
    match reach.trigger(client, name, "rule", Some(params)).await? {
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
        Answered::TimedOut => Ok(vec![
            ruled,
            late(name, "rule", &format!("shep kelpie rule -p {name}")),
        ]),
        other => Err(unanswered(name, other)),
    }
}

// What marks a runner's answer: draining, and its run state when it has one.
fn marks(body: &str) -> Vec<String> {
    (draining(body).then(|| "draining".to_owned()))
        .into_iter()
        .chain(run(body))
        .collect()
}

/// `sheep`'s runner's answer `body` to `status` as the line the JSON form
/// prints: its name, its marks and the object as it came
pub fn json_status(sheep: &str, body: &str) -> String {
    match marks(body).as_slice() {
        [] => format!("{sheep}: {body}"),
        marks => format!("{sheep} ({}): {body}", marks.join(", ")),
    }
}

/// `sheep`'s runner's answer `body` to `status` as its short block of lines,
/// beside the commit `installed` names when the installed kelpie could be asked
pub fn plain_status(sheep: &str, body: &str, installed: Option<&str>) -> Vec<String> {
    match serde_json::from_str(body) {
        Ok(object) => plain::block(sheep, &marks(body), &object, installed),
        Err(_) => vec![json_status(sheep, body)],
    }
}

/// The commit the installed kelpie names, read from the program the adopted dog
/// runs, which is the file `upgrade` installs onto
///
/// `None` when the flock has no adopted kelpie, when its build cannot be asked
/// in [`BUILD_LIMIT`], or when that build names no commit. Nothing a maintainer
/// is asked to fix: `status` says nothing of the installed build then.
pub(crate) async fn installed_build(client: &dyn Connection) -> Option<String> {
    let program = match flock(client)
        .await
        .ok()?
        .iter()
        .find(|row| row.name == crate::dog::NAME)?
        .dog
        .as_ref()?
    {
        DogSource::Adopted { path, .. } => path.clone(),
        _ => return None,
    };
    crate::home::Build::within(Path::new(&program), BUILD_LIMIT)
        .ok()?
        .commit
}

/// `sheep`'s runner's answer `body` to `action` with `params` as the plain
/// lines `timings`, `board`, `drain`, `undrain`, `gate`, `drop`, `add`,
/// `triage`, `rework` and `adopt` print
pub fn plain_reply(sheep: &str, action: &str, params: Option<&str>, body: &str) -> Vec<String> {
    plain::reply(sheep, action, params, body)
}

/// How a verb prints a runner's answer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A short line or block of lines
    Plain,
    /// The runner's JSON object as it came, which `status` prints on one line
    /// per project, behind its name
    Json,
}

/// Every project's status: a short block each, or with [`Format::Json`] a
/// line holding its name and its runner's answer
///
/// A project whose runner gave no answer is one line either way.
///
/// # Errors
///
/// A message when the shepherd cannot list the projects.
pub async fn status(
    reach: &Reach,
    client: &dyn Connection,
    format: Format,
) -> Result<Vec<String>, String> {
    let installed = match format {
        Format::Plain => installed_build(client).await,
        Format::Json => None,
    };
    let mut lines = Vec::new();
    for sheep in tables(client).await?.into_keys() {
        match reach.trigger(client, &sheep, "status", None).await? {
            Answered::Runner(body) => match format {
                Format::Plain => lines.extend(plain_status(&sheep, &body, installed.as_deref())),
                Format::Json => lines.push(json_status(&sheep, &body)),
            },
            Answered::Starting => lines.push(format!("{sheep}: starting")),
            Answered::Down => lines.push(format!("{sheep}: not running")),
            Answered::TimedOut => {
                lines.push(format!("{sheep}: running, and did not answer in time"));
            }
        }
    }
    if lines.is_empty() {
        lines.push(no_projects());
    }
    Ok(lines)
}

/// Sends `project`'s runner `action` with `params`, as `shep trigger` would
///
/// For an action that changes nothing, so a retry after an error is safe.
///
/// # Errors
///
/// A message when the project has no kelpie runner, it is not running, it
/// did not answer in time, or it refused the trigger, with its reason.
pub async fn send(
    reach: &Reach,
    client: &dyn Connection,
    project: &ProjectName,
    action: &str,
    params: Option<&str>,
) -> Result<Vec<String>, String> {
    kelpie_runner(client, &flock(client).await?, project).await?;
    let runner = project.as_str();
    match reach.trigger(client, runner, action, params).await? {
        Answered::Runner(body) => match refusal(&body) {
            Some(why) => Err(why),
            None => Ok(vec![body]),
        },
        other => Err(unanswered(runner, other)),
    }
}

/// Sends `project`'s runner `action` with `params`, an action that changes its state
///
/// The answer is plain lines, or the runner's object with [`Format::Json`]. A
/// runner busy mid-step can take the action and not answer in time. That is no
/// error, since a retry would repeat it: the line says the action has most
/// likely been taken, and the command that shows it.
///
/// # Errors
///
/// A message when the project has no kelpie runner, it is not running or is
/// starting, or it refused the action, with its reason.
pub async fn change(
    reach: &Reach,
    client: &dyn Connection,
    project: &ProjectName,
    action: &str,
    params: Option<&str>,
    format: Format,
) -> Result<Vec<String>, String> {
    kelpie_runner(client, &flock(client).await?, project).await?;
    let runner = project.as_str();
    match reach.trigger(client, runner, action, params).await? {
        Answered::Runner(body) => match (refusal(&body), format) {
            (Some(why), _) => Err(why),
            (None, Format::Json) => Ok(vec![body]),
            (None, Format::Plain) => Ok(plain_reply(runner, action, params, &body)),
        },
        Answered::TimedOut => {
            let check = match (action, params) {
                ("board", Some(_)) => format!("shep kelpie board -p {runner}"),
                _ => check_status(runner),
            };
            let said = late(runner, action, &check);
            Ok(vec![said_as(format, runner, said)])
        }
        other => Err(unanswered(runner, other)),
    }
}

/// Hands `project`'s project manager `note` for its next wake
///
/// A runner busy mid-step can take the note and not answer in time. The
/// shepherd delivered the trigger then, so the note has most likely arrived,
/// and an error would invite a retry that wakes the project manager a second
/// time for one note. It is no error, and says the note may have arrived. The
/// runner's own refusal, which comes after the deadline, is not seen.
///
/// # Errors
///
/// A message when the project has no kelpie runner, it is not running, or
/// it refused the note, with its reason. With [`Format::Json`] it returns the
/// runner's answer as it came.
pub async fn tell(
    reach: &Reach,
    client: &dyn Connection,
    project: &ProjectName,
    note: &str,
    format: Format,
) -> Result<Vec<String>, String> {
    kelpie_runner(client, &flock(client).await?, project).await?;
    let runner = project.as_str();
    match reach.trigger(client, runner, "tell", Some(note)).await? {
        Answered::Runner(body) => match refusal(&body) {
            Some(why) => Err(why),
            None if format == Format::Json => Ok(vec![body]),
            None => Ok(vec![format!(
                "told {project}'s project manager, which reads it on its next wake"
            )]),
        },
        Answered::TimedOut => {
            let said = format!(
                "{runner}'s runner did not answer in time, and the note may have arrived: \
                 do not tell it again unless the project manager does not wake for it, \
                 and `shep bleats {runner}` says why"
            );
            Ok(vec![said_as(format, runner, said)])
        }
        other => Err(unanswered(runner, other)),
    }
}

// What kelpie itself says when the runner gave no object to print: the
// sentence, or under [`Format::Json`] an object that holds it.
fn said_as(format: Format, sheep: &str, said: String) -> String {
    match format {
        Format::Plain => said,
        Format::Json => serde_json::json!({ "sheep": sheep, "message": said }).to_string(),
    }
}

// The command that shows a runner's state.
fn check_status(runner: &str) -> String {
    format!("shep kelpie status -p {runner}")
}

// What a verb that changes `runner` says when it took `action` and gave no
// answer in time.
fn late(runner: &str, action: &str, check: &str) -> String {
    format!(
        "{runner}'s runner was busy and did not answer in time, so `{action}` has most likely \
         been taken: `{check}` shows it"
    )
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
pub(crate) fn refusal(body: &str) -> Option<String> {
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
    client: &dyn Connection,
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
    client: &dyn Connection,
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

mod plain;
mod socket;

pub use socket::Reach;

#[cfg(test)]
mod tests;
