//! Restarting the dog and each runner onto the installed build
//!
//! One sheep at a time, the dog first: every runner asks it for leases, so it
//! is back before any runner goes down. A runner is restarted only when its
//! `status` shows no merge in flight, and the dog only when no runner has
//! one. A runner that does not answer `status` is not a runner with no merge:
//! it is waited on, and named if the wait runs out. A sheep that is stopped
//! stays stopped: it starts on the new build.
//!
//! The installed kelpie is the program the adopted dog runs, and every sheep
//! restarted must run that same path, or the restart would not move it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use shep_client::Client;
use shep_client::shep_core::protocol::request::DogSource;
use shep_client::shep_core::status::ProcStatus;
use tokio::time::Instant;

use crate::dog;
use crate::flock::control::{Answered, trigger};
use crate::flock::{flock, kelpie_sheep, resume, script, tables};

/// How long the upgrade waits, and how often it asks
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Patience {
    /// Between two looks at a sheep
    pub poll: Duration,
    /// For a sheep it restarted to answer its `status`
    pub start: Duration,
    /// For a merge in flight to end, or a runner to say whether it has one
    pub merge: Duration,
}

impl Default for Patience {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(500),
            // A runner needs about 7s to stop cleanly before it starts.
            start: Duration::from_secs(60),
            merge: Duration::from_secs(60 * 60),
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

fn running(status: ProcStatus) -> bool {
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
    pub async fn read(client: &Client, say: &mut dyn FnMut(String)) -> Result<Self, String> {
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
/// # Errors
///
/// A message naming the sheep that could not be restarted, did not come back,
/// or kept a merge in flight, or would not say whether it had one, for longer
/// than `patience.merge`. The sheep restarted before it are on the new build,
/// and running the upgrade again finishes the rest.
pub async fn restart_all(
    client: &Client,
    plan: &Plan,
    patience: Patience,
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
        wait_out_merges(client, &names, patience, say).await?;
        bounce_if_running(client, &plan.dog.name, patience, say).await?;
    }
    for name in names {
        wait_out_merges(client, &[name], patience, say).await?;
        bounce_if_running(client, name, patience, say).await?;
    }
    Ok(())
}

// Restarts `name` unless it is not running now. The plan is a look from
// before the wait, which can be long, and a sheep the maintainer stopped since
// stays stopped.
async fn bounce_if_running(
    client: &Client,
    name: &str,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let now = flock(client)
        .await?
        .into_iter()
        .find(|row| row.name == name)
        .map(|row| row.status);
    if !now.is_some_and(running) {
        say(format!(
            "`{name}` was stopped meanwhile, so it stays stopped"
        ));
        return Ok(());
    }
    bounce(client, name, patience, say).await
}

async fn bounce(
    client: &Client,
    name: &str,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    resume(client, name).await?;
    let waited = Instant::now();
    loop {
        if let Answered::Runner(_) = trigger(client, name, "status", None).await? {
            say(format!("restarted `{name}`"));
            return Ok(());
        }
        if waited.elapsed() >= patience.start {
            return Err(format!(
                "`{name}` did not answer in {}s after its restart: `shep bleats {name}` says why",
                patience.start.as_secs()
            ));
        }
        tokio::time::sleep(patience.poll).await;
    }
}

// Returns once none of `names` has a merge in flight, asking again until then.
// A runner that does not answer is waited on like one that is merging, and
// named when the wait runs out.
async fn wait_out_merges(
    client: &Client,
    names: &[&str],
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let waited = Instant::now();
    let mut told = Vec::new();
    loop {
        let mut busy = Vec::new();
        for &name in names {
            match trigger(client, name, "status", None).await? {
                Answered::Runner(body) => busy.extend(
                    merging(&body)
                        .into_iter()
                        .map(|issue| format!("`{name}` is merging #{issue}")),
                ),
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
                "{} after {}s, so nothing was restarted for it",
                busy.join(" and "),
                patience.merge.as_secs()
            ));
        }
        if told != busy {
            say(format!("waiting: {}", busy.join(" and ")));
            told = busy;
        }
        tokio::time::sleep(patience.poll).await;
    }
}

/// The issue of each work item a runner's `status` shows being merged
///
/// A work item in `merge` is being merged, and one in `done` with its pull
/// request merged is having its worktree removed: a restart cuts neither short.
pub fn merging(status: &str) -> Vec<u64> {
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
        .filter_map(|item| item["issue"].as_u64())
        .collect()
}
