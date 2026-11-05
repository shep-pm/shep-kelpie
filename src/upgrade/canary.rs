//! The canary: the first runner restarted, and the check that its new build is healthy
//!
//! The canary is the first runner to show no merge in flight. After its
//! restart, `status` is asked until it passes or fails on a definite signal:
//! it passes at its first full pass of the loop on the installed build, and
//! fails on a refused setting, a step or turn that newly fails, another build,
//! or silence. The other runners are not touched until it passes.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

use super::restart::{Patience, busy, is_running, not_running as down, silent};
use crate::flock::control::{Answered, trigger};
use crate::home::{Build, stamp_of};
use crate::shepherd::Connection;
use crate::ticket::TicketId;

/// Why `restart_all` stopped
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// The canary failed a check: the dog and the canary run the new build, and
    /// no other runner was touched
    Canary(String),
    /// A restart failed, was interrupted, or waited out: finishing the
    /// restarts is what is left to do
    Other(String),
}

impl From<String> for Stopped {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

/// The installed file, as the canary's `status` must show it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Installed {
    pub stamp: String,
    pub commit: Option<String>,
}

impl Installed {
    /// Reads the stamp of `program`, and the commit its build names
    pub(super) fn of(program: &Path) -> Result<Self, String> {
        Ok(Self {
            stamp: stamp_of(program)?,
            // A build from before `version --json` names none.
            commit: Build::of(program).ok().and_then(|build| build.commit),
        })
    }
}

/// The runner picked to go first, and the `status` it showed when picked
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Picked<'a> {
    pub name: &'a str,
    pub before: String,
}

/// Picks the canary from `names`: the first to show no merge in flight
///
/// Every running runner is asked in parallel each `patience.poll`. A runner
/// found stopped is taken out of `names` and said to stay stopped. `None`
/// when none is left.
///
/// # Errors
///
/// A message naming each runner still merging, or not answering, after
/// `patience.merge`, or one from the shepherd.
pub(super) async fn pick<'a>(
    client: &dyn Connection,
    names: &mut Vec<&'a str>,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<Option<Picked<'a>>, String> {
    let waited = Instant::now();
    let mut told = Vec::new();
    loop {
        let looks = join_all(
            names
                .iter()
                .map(|&name| trigger(client, name, "status", None)),
        )
        .await;
        let mut busy_now = Vec::new();
        let mut gone = Vec::new();
        for (&name, look) in names.iter().zip(looks) {
            let look = look?;
            if let Answered::Down = look {
                if !is_running(client, name).await? {
                    gone.push(name);
                    continue;
                }
                busy_now.push(format!("`{name}` did not answer `status`"));
                continue;
            }
            let lines = busy(name, &look);
            if lines.is_empty()
                && let Answered::Runner(before) = look
            {
                return Ok(Some(Picked { name, before }));
            }
            busy_now.extend(lines);
        }
        for name in gone {
            say(format!(
                "`{name}` was stopped meanwhile, so it stays stopped"
            ));
            names.retain(|&n| n != name);
        }
        if names.is_empty() {
            return Ok(None);
        }
        if waited.elapsed() >= patience.merge {
            return Err(format!(
                "{} after {}s, so no runner was restarted",
                busy_now.join(" and "),
                patience.merge.as_secs()
            ));
        }
        if told != busy_now {
            say(format!("waiting: {}", busy_now.join(" and ")));
            told = busy_now;
        }
        tokio::time::sleep(patience.poll).await;
    }
}

/// Picks the canary, and takes a pick found stopped out of the running
///
/// # Errors
///
/// As [`pick`].
pub(super) async fn choose<'a>(
    client: &dyn Connection,
    names: &mut Vec<&'a str>,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<Option<Picked<'a>>, String> {
    loop {
        let Some(picked) = pick(client, names, patience, say).await? else {
            return Ok(None);
        };
        say(format!(
            "`{}` shows no merge in flight, so it goes first, as the canary: the other \
             runners go on once it shows the new build healthy",
            picked.name
        ));
        if is_running(client, picked.name).await? {
            return Ok(Some(picked));
        }
        say(format!(
            "`{}` was stopped meanwhile, so it stays stopped",
            picked.name
        ));
        names.retain(|&n| n != picked.name);
    }
}

/// What one `status` of the canary says of its build
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Its first full pass on the installed build
    Passed,
    /// Not yet: no pass, or a body that names nothing wrong
    Waiting,
    /// A definite signal, as the sentence that follows "the canary `name`"
    Failed(String),
}

/// Judges the canary's `status` `now` against the `before` read and the installed file
pub(super) fn judge(before: &str, now: &str, installed: &Installed) -> Verdict {
    let (Ok(before), Ok(now)) = (
        serde_json::from_str::<Value>(before),
        serde_json::from_str::<Value>(now),
    ) else {
        return Verdict::Waiting;
    };
    if let Some(refused) = now.get("settings_refused").filter(|r| !r.is_null()) {
        let why = refused["reason"].as_str().unwrap_or("no reason given");
        return Verdict::Failed(format!("refuses its settings: {why}"));
    }
    if let Some(shown) = now["build"].as_str().filter(|&b| b != installed.stamp) {
        return Verdict::Failed(format!(
            "shows build {shown}, not the installed {}",
            installed.stamp
        ));
    }
    if let (Some(shown), Some(commit)) = (now["commit"].as_str(), &installed.commit)
        && shown != commit
    {
        return Verdict::Failed(format!("shows commit {shown}, not the installed {commit}"));
    }
    let items = |status: &Value| match status["work_items"].as_array() {
        Some(items) => items.clone(),
        None => Vec::new(),
    };
    let failing_before: Vec<TicketId> = items(&before)
        .iter()
        .filter(|item| !item["failing"].is_null())
        .filter_map(|item| TicketId::from_json(&item["issue"]))
        .collect();
    let working_before: Vec<TicketId> = before["working"]
        .as_array()
        .map(|tickets| tickets.iter().filter_map(TicketId::from_json).collect())
        .unwrap_or_default();
    for item in items(&now) {
        let Some(ticket) = TicketId::from_json(&item["issue"]) else {
            continue;
        };
        if !item["failing"].is_null() && !failing_before.contains(&ticket) {
            let reason = item["failing"]["reason"]
                .as_str()
                .unwrap_or("no reason given");
            return Verdict::Failed(format!(
                "shows {}'s step failing, which it did not before: {reason}",
                ticket.shown()
            ));
        }
        if item["turn"]["state"] == "failed" && working_before.contains(&ticket) {
            let reason = item["turn"]["reason"].as_str().unwrap_or("no reason given");
            return Verdict::Failed(format!(
                "shows {}'s worker turn failed, which was going on before: {reason}",
                ticket.shown()
            ));
        }
    }
    let built = now["build"].as_str() == Some(installed.stamp.as_str());
    let commit = installed
        .commit
        .as_deref()
        .is_none_or(|commit| now["commit"].as_str() == Some(commit));
    match now["passes"].as_u64() {
        Some(passes) if passes >= 1 && built && commit => Verdict::Passed,
        _ => Verdict::Waiting,
    }
}

/// Asks the restarted canary `name` for its `status` until it passes
///
/// # Errors
///
/// [`Stopped::Canary`] naming the failed check, when the canary fails one,
/// answers nothing in `patience.start`, or is not running. A shepherd failure
/// is [`Stopped::Other`].
pub(super) async fn check(
    client: &dyn Connection,
    name: &str,
    before: &str,
    installed: &Installed,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<(), Stopped> {
    let waited = Instant::now();
    let mut heard = false;
    loop {
        match trigger(client, name, "status", None).await? {
            Answered::Runner(now) => {
                heard = true;
                match judge(before, &now, installed) {
                    Verdict::Passed => {
                        say(format!(
                            "the canary `{name}` shows the new build healthy, so the other \
                             runners are restarted together"
                        ));
                        return Ok(());
                    }
                    Verdict::Failed(why) => {
                        return Err(Stopped::Canary(format!("the canary `{name}` {why}")));
                    }
                    Verdict::Waiting => {}
                }
            }
            Answered::Down if !is_running(client, name).await? => {
                return Err(Stopped::Canary(not_running(name)));
            }
            _ => {}
        }
        if !heard && waited.elapsed() >= patience.start {
            return Err(Stopped::Canary(silent_canary(name, patience.start)));
        }
        tokio::time::sleep(patience.poll).await;
    }
}

/// The failure of a canary that is not running after its restart
pub(super) fn not_running(name: &str) -> String {
    format!("the canary {}", down(name))
}

/// The failure of a canary that answers no `status` after its restart
pub(super) fn silent_canary(name: &str, wait: Duration) -> String {
    format!("the canary {}", silent(name, wait))
}

/// Runs `futures` together on one task and returns their outputs in order
pub(super) async fn join_all<F: Future>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output> {
    let mut futures: Vec<Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut done: Vec<Option<F::Output>> = futures.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut all = true;
        for (future, output) in futures.iter_mut().zip(done.iter_mut()) {
            if output.is_none() {
                match future.as_mut().poll(cx) {
                    Poll::Ready(value) => *output = Some(value),
                    Poll::Pending => all = false,
                }
            }
        }
        if all { Poll::Ready(()) } else { Poll::Pending }
    })
    .await;
    done.into_iter().flatten().collect()
}
