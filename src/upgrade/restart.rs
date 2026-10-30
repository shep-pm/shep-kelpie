//! Restarting the dog and each runner onto the installed build
//!
//! One sheep at a time, the dog first: every runner asks it for leases. A
//! runner is restarted only when its `status` shows no merge in flight, and the
//! dog only when no runner has one. A runner that does not answer `status` is
//! waited on and named if the wait runs out. A stopped sheep stays stopped.
//! A silent runner after its restart does not hold the others back: it is
//! asked again after them. Unless asked to restart at once, each runner is
//! drained first ([`super::drain`]). The dog is never drained. Every sheep
//! restarted must run the adopted dog's path, or the restart moves nothing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use shep_client::shep_core::protocol::request::DogSource;
use shep_client::shep_core::status::ProcStatus;
use shep_kelpie_protocol::status::Failing;
use tokio::time::Instant;

use super::drain::{self, Purpose};
use super::handover::{Handed, hand_over};
use crate::dog;
use crate::flock::control::{Answered, trigger};
use crate::flock::{flock, kelpie_sheep, resume, script, tables};
use crate::home::stamp_of;
use crate::interrupt::{Caught, Interrupts};
use crate::shepherd::Connection;
use crate::ticket::TicketId;

/// How long the upgrade waits, and how often it asks
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Patience {
    /// Between two looks at a sheep
    pub poll: Duration,
    /// For a sheep it restarted to answer its `status`
    pub start: Duration,
    /// For a merge in flight to end, or a runner to say whether it has one
    pub merge: Duration,
    /// Past the longest a drained runner's call may run, for its last call
    /// to end and be recorded
    pub margin: Duration,
    /// For a worker turn a pause ends at its next tool call, before the stop
    /// cuts the call short
    pub tool: Duration,
}

impl Default for Patience {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(500),
            // A runner needs about 7s to stop cleanly before it starts.
            start: Duration::from_secs(60),
            merge: Duration::from_secs(60 * 60),
            margin: Duration::from_secs(5 * 60),
            // A full test run is about six minutes, so a call at its usual
            // longest ends inside this; a hung one does not, and is cut.
            tool: Duration::from_secs(10 * 60),
        }
    }
}

/// What stops an upgrade part way, other than a failure
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {
    /// Ctrl-C or SIGTERM (Ctrl-Break on Windows), from the runners' restarts on
    Signals,
    /// Nothing
    Never,
    /// A stand-in Ctrl-C this long after the runners' restarts begin
    #[cfg(test)]
    After(Duration),
}

/// Waits for `interrupt`, and names it
pub(crate) async fn interrupted(interrupt: Interrupt) -> &'static str {
    match interrupt {
        Interrupt::Signals => match Interrupts::listen(&[Caught::Interrupt, Caught::Terminate]) {
            Ok(mut interrupts) => interrupts.recv().await.name(),
            Err(_) => std::future::pending().await,
        },
        Interrupt::Never => std::future::pending().await,
        #[cfg(test)]
        Interrupt::After(after) => {
            tokio::time::sleep(after).await;
            "Ctrl-C"
        }
    }
}

/// One of kelpie's sheep, and the program its entry runs
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    name: String,
    program: String,
    status: ProcStatus,
}

impl Member {
    // Running, or about to be: it has exec'd the file the upgrade replaces.
    fn online(&self) -> bool {
        running(self.status)
    }
}

pub(super) fn running(status: ProcStatus) -> bool {
    matches!(status, ProcStatus::Online | ProcStatus::Starting)
}

/// The installed kelpie and the sheep that run it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The program the adopted dog runs, which is the installed kelpie
    pub program: PathBuf,
    dog: Member,
    runners: Vec<Member>,
}

impl Plan {
    /// Reads the flock: the adopted dog's program, and the runners
    ///
    /// Sheep kelpie did not start are left alone, and said so through `say`.
    ///
    /// # Errors
    ///
    /// A message when the flock has no adopted kelpie, or when a sheep that
    /// would be restarted runs another program than the dog does: a restart
    /// cannot move it onto the installed build.
    pub async fn read(
        client: &dyn Connection,
        say: &mut dyn FnMut(String),
    ) -> Result<Self, String> {
        let rows = flock(client).await?;
        let dog = rows
            .iter()
            .find(|row| row.name == dog::NAME)
            .and_then(|row| match row.dog.as_ref()? {
                DogSource::Adopted { path, .. } => Some(Member {
                    name: row.name.clone(),
                    program: path.clone(),
                    status: row.status,
                }),
                _ => None,
            })
            .ok_or_else(|| {
                format!(
                    "the flock has no adopted kelpie, so there is no installed kelpie to \
                     upgrade: `shep adopt <path to kelpie> --name {}` adopts one",
                    dog::NAME
                )
            })?;
        let mut runners = Vec::new();
        for name in tables(client).await?.into_keys() {
            match kelpie_sheep(client, &rows, &name, &["runner", &name]).await {
                Ok(Some(row)) => runners.push(Member {
                    program: script(client, &name).await?,
                    status: row.status,
                    name,
                }),
                Ok(None) => {}
                Err(e) => say(format!("`{name}` left alone: {e}")),
            }
        }
        let plan = Self {
            program: PathBuf::from(&dog.program),
            dog,
            runners,
        };
        plan.agrees()?;
        Ok(plan)
    }

    // Every sheep to be restarted runs the dog's program, else none is touched.
    fn agrees(&self) -> Result<(), String> {
        let strays: Vec<String> = self
            .runners
            .iter()
            .filter(|m| m.online() && Path::new(&m.program) != self.program)
            .map(|m| format!("`{}` runs {}", m.name, m.program))
            .collect();
        if strays.is_empty() {
            return Ok(());
        }
        Err(format!(
            "the dog runs {}, and {}: a restart cannot move a sheep onto the installed build. \
             Nothing was installed or restarted. Point those sheep at {}, then run the \
             upgrade again",
            self.program.display(),
            strays.join(" and "),
            self.program.display()
        ))
    }
}

/// Restarts the dog and each running runner, saying what it does through `say`
///
/// With `keep_calls`, each runner is handed over to the installed build. One
/// that refuses, predates the handover or is slow to show the build is drained
/// before its merge wait, and sent `undrain` if not restarted after all.
///
/// # Errors
///
/// A message naming the sheep that could not be restarted, did not come back,
/// kept a merge in flight past `patience.merge`, or would not say whether it
/// had one; a drained runner that kept a call past its ceiling and
/// `patience.margin`; or the interrupt. A runner silent after its restart is
/// named after the others, the dog at once. Sheep restarted before it are on
/// the new build, and running the upgrade again finishes the rest.
pub async fn restart_all(
    client: &dyn Connection,
    plan: &Plan,
    patience: Patience,
    keep_calls: bool,
    interrupt: Interrupt,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    for member in plan
        .runners
        .iter()
        .chain([&plan.dog])
        .filter(|m| !m.online())
    {
        say(match member.status {
            ProcStatus::Stopped if Path::new(&member.program) == plan.program => format!(
                "`{}` is stopped, so it stays stopped and starts on the new build",
                member.name
            ),
            ProcStatus::Stopped => format!(
                "`{}` is stopped, and runs {}, so it does not start on the new build",
                member.name, member.program
            ),
            status => format!(
                "`{}` is {status}, not running, so it is left alone: `shep restart {}` runs it \
                 on the new build",
                member.name, member.name
            ),
        });
    }
    let names: Vec<&str> = plan
        .runners
        .iter()
        .filter(|m| m.online())
        .map(|m| m.name.as_str())
        .collect();
    // The dog goes down only when no runner is merging.
    if plan.dog.online() {
        wait_out_merges(client, &names, patience, Purpose::Upgrade, say).await?;
        if let Bounced::Silent = bounce_if_running(client, &plan.dog.name, patience, say).await? {
            return Err(silent(&plan.dog.name, patience.start));
        }
    }
    let stop = interrupted(interrupt);
    tokio::pin!(stop);
    // What a handed over runner shows in its `status`.
    let installed = match keep_calls {
        true => Some(stamp_of(&plan.program)?),
        false => None,
    };
    let mut slow = Vec::new();
    let restarted = async {
        for name in names {
            let Some(installed) = &installed else {
                wait_out_merges(client, &[name], patience, Purpose::Upgrade, say).await?;
                if let Bounced::Silent = bounce_if_running(client, name, patience, say).await? {
                    slow.push(name);
                }
                continue;
            };
            let handed = tokio::select! {
                handed = hand_over(client, name, installed, patience, say) => handed?,
                by = &mut stop => return Err(format!("stopped by {by} while `{name}` was handing over")),
            };
            match handed {
                Handed::Over => continue,
                Handed::Stays(why) => say(why),
            }
            let restarted = async {
                drain::drain(client, name, patience, Purpose::Upgrade, say).await?;
                wait_out_merges(client, &[name], patience, Purpose::Upgrade, say).await?;
                bounce_if_running(client, name, patience, say).await
            };
            let restarted = tokio::select! {
                restarted = restarted => restarted,
                by = &mut stop => Err(format!("stopped by {by} while `{name}` was draining")),
            };
            match restarted {
                Ok(Bounced::Answered) => {}
                Ok(Bounced::Silent) => slow.push(name),
                Err(e) => {
                    let undrained = drain::undrain(client, name, Purpose::Upgrade).await;
                    return Err(format!("{e}{undrained}"));
                }
            }
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(e) = restarted {
        let unasked: Vec<String> = slow
            .iter()
            .map(|name| silent(name, patience.start))
            .collect();
        return Err(match unasked.is_empty() {
            true => e,
            false => format!("{e}\nNot asked again: {}", unasked.join("; ")),
        });
    }
    tokio::select! {
        asked = ask_again(client, slow, patience, say) => asked,
        by = &mut stop => Err(format!("stopped by {by} while the runners slow to answer were asked again")),
    }
}

/// Says which running runners answer `status` with a build other than `commit`,
/// or name none at all
///
/// Run after the restarts, so a runner the upgrade moved is not named, nor one
/// that stopped and starts on the new build when it starts. A runner that
/// answers nothing is left to what the restarts already said of it. Nothing
/// here fails an upgrade, which is done by now.
pub async fn still_behind(
    client: &dyn Connection,
    plan: &Plan,
    commit: &str,
    say: &mut dyn FnMut(String),
) {
    for name in plan
        .runners
        .iter()
        .filter(|m| m.online())
        .map(|m| m.name.as_str())
    {
        let Ok(Answered::Runner(body)) = trigger(client, name, "status", None).await else {
            continue;
        };
        let shown = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|status| status["commit"].as_str().map(str::to_owned));
        let behind = match shown {
            Some(shown) if shown == commit => None,
            Some(shown) => Some(format!(
                "`{name}` still runs {shown}, not the installed {commit}"
            )),
            None => Some(format!(
                "`{name}` names no build, and the installed build is {commit}"
            )),
        };
        if let Some(behind) = behind {
            say(behind);
        }
    }
}

/// How a restarted sheep did in `patience.start`
enum Bounced {
    Answered,
    /// Running, and no `status` answer yet
    Silent,
}

fn silent(name: &str, wait: Duration) -> String {
    format!(
        "`{name}` did not answer in {}s after its restart: `shep bleats {name}` says why",
        wait.as_secs()
    )
}

// Asks each of `names`, restarted and silent, for its `status` again until all
// have answered or `patience.start` runs out. One that stopped meanwhile is
// not waited on. A shepherd failure ends the asking, and is reported with the
// runners found down and those not heard from by then.
async fn ask_again(
    client: &dyn Connection,
    names: Vec<&str>,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let mut pending = names;
    let mut failed = Vec::new();
    if let Err(e) = settle(client, &mut pending, &mut failed, patience, say).await {
        failed.push(e);
    }
    failed.extend(pending.into_iter().map(|name| {
        format!("`{name}` never answered after its restart: `shep bleats {name}` says why")
    }));
    match failed.is_empty() {
        true => Ok(()),
        false => Err(failed.join("; ")),
    }
}

// Leaves in `pending` the runners that never answered, and in `failed` those
// found not running.
async fn settle(
    client: &dyn Connection,
    pending: &mut Vec<&str>,
    failed: &mut Vec<String>,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let waited = Instant::now();
    while !pending.is_empty() {
        let mut at = 0;
        while let Some(&name) = pending.get(at) {
            // Each look ends with the shared wait, so a few silent runners
            // cannot stretch it to a trigger's own deadline apiece.
            let left = patience.start.saturating_sub(waited.elapsed());
            let looked = tokio::time::timeout(
                left.max(patience.poll),
                trigger(client, name, "status", None),
            )
            .await;
            if let Answered::Runner(_) = looked.unwrap_or(Ok(Answered::TimedOut))? {
                say(format!("restarted `{name}`"));
                pending.remove(at);
            } else if is_running(client, name).await? {
                at += 1;
            } else {
                failed.push(format!(
                    "`{name}` is not running after its restart: `shep bleats {name}` says why"
                ));
                pending.remove(at);
            }
        }
        if waited.elapsed() >= patience.start {
            break;
        }
        if !pending.is_empty() {
            tokio::time::sleep(patience.poll).await;
        }
    }
    Ok(())
}

async fn is_running(client: &dyn Connection, name: &str) -> Result<bool, String> {
    Ok(flock(client)
        .await?
        .into_iter()
        .find(|row| row.name == name)
        .is_some_and(|row| running(row.status)))
}

// Restarts `name` unless it is not running now. The plan is a look from
// before the wait, which can be long, and a sheep the maintainer stopped since
// stays stopped.
async fn bounce_if_running(
    client: &dyn Connection,
    name: &str,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<Bounced, String> {
    if !is_running(client, name).await? {
        say(format!(
            "`{name}` was stopped meanwhile, so it stays stopped"
        ));
        return Ok(Bounced::Answered);
    }
    bounce(client, name, patience, say).await
}

async fn bounce(
    client: &dyn Connection,
    name: &str,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<Bounced, String> {
    resume(client, name).await?;
    let waited = Instant::now();
    loop {
        if let Answered::Runner(_) = trigger(client, name, "status", None).await? {
            say(format!("restarted `{name}`"));
            return Ok(Bounced::Answered);
        }
        if waited.elapsed() >= patience.start {
            if !is_running(client, name).await? {
                return Err(format!(
                    "`{name}` is not running after its restart: `shep bleats {name}` says why"
                ));
            }
            say(format!(
                "`{name}` did not answer in {}s after its restart, so the others go on first",
                patience.start.as_secs()
            ));
            return Ok(Bounced::Silent);
        }
        tokio::time::sleep(patience.poll).await;
    }
}

/// Returns once none of `names` has a merge in flight, asking again until then
///
/// A runner that does not answer is waited on like one that is merging, and
/// named when the wait runs out.
///
/// # Errors
///
/// A message naming each runner still merging, or not answering, after
/// `patience.merge`, and what was not done for `purpose`, or one from the
/// shepherd.
pub(crate) async fn wait_out_merges(
    client: &dyn Connection,
    names: &[&str],
    patience: Patience,
    purpose: Purpose,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let waited = Instant::now();
    let mut told = Vec::new();
    loop {
        let mut busy = Vec::new();
        for &name in names {
            match trigger(client, name, "status", None).await? {
                Answered::Runner(body) => {
                    busy.extend(merging(&body).into_iter().map(|(ticket, failing)| {
                        let mut line = format!("`{name}` is merging {}", ticket.shown());
                        if let Some(failing) = failing {
                            line.push_str(&format!(
                                ", whose cleanup has failed {} times: {}",
                                failing.failures, failing.reason
                            ));
                        }
                        line
                    }))
                }
                // Not taking its actions yet: it may be mid-merge from before.
                Answered::Starting => busy.push(format!("`{name}` is starting")),
                // Delivered and unanswered: whether it merges is unknown.
                Answered::TimedOut => busy.push(format!("`{name}` did not answer `status`")),
                Answered::Down => {}
            }
        }
        if busy.is_empty() {
            return Ok(());
        }
        if waited.elapsed() >= patience.merge {
            return Err(format!(
                "{} after {}s, so {}",
                busy.join(" and "),
                patience.merge.as_secs(),
                purpose.not_done()
            ));
        }
        if told != busy {
            say(format!("waiting: {}", busy.join(" and ")));
            told = busy;
        }
        tokio::time::sleep(patience.poll).await;
    }
}

/// Each ticket a runner's `status` shows being merged, with its failing step
///
/// A work item in `merge` is being merged, and one in `done` with its pull
/// request merged is having its worktree removed: a restart cuts neither short.
/// A runner from before ticket ids writes a GitHub issue number, which reads as
/// the GitHub id of that number.
pub fn merging(status: &str) -> Vec<(TicketId, Option<Failing>)> {
    let Ok(Value::Object(status)) = serde_json::from_str::<Value>(status) else {
        return Vec::new();
    };
    let items = match (status.get("work_items"), status.get("work_item")) {
        (Some(Value::Array(items)), _) => items.iter().collect(),
        (_, Some(item @ Value::Object(_))) => vec![item],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter(|item| {
            let phase = &item["phase"];
            match phase["state"].as_str().or(phase.as_str()) {
                Some("merge") => true,
                Some("done") => phase["merged"].as_bool().unwrap_or(true),
                _ => false,
            }
        })
        .filter_map(|item| {
            let ticket = TicketId::from_json(&item["issue"])?;
            let failing = serde_json::from_value(item["failing"].clone()).ok();
            Some((ticket, failing))
        })
        .collect()
}
