//! Restarting the dog and each runner onto the installed build
//!
//! One sheep at a time, the dog first: every runner asks it for leases, so it
//! is back before any runner goes down. A runner is restarted only when its
//! `status` shows no merge in flight, and the dog only when no runner has
//! one. A sheep that is stopped stays stopped: it starts on the new build.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use shep_client::Client;
use shep_client::shep_core::status::ProcStatus;
use tokio::time::Instant;

use crate::dog;
use crate::flock::control::{Answered, trigger};
use crate::flock::{Found, flock, kelpie_sheep, resume, tables};

/// How long the upgrade waits, and how often it asks
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Patience {
    /// Between two looks at a sheep
    pub poll: Duration,
    /// For a sheep it restarted to answer its `status`
    pub start: Duration,
    /// For a merge in flight to end
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

/// Restarts the dog and each running runner, saying what it does through `say`
///
/// # Errors
///
/// A message naming the sheep that could not be restarted, did not come back,
/// or kept a merge in flight for longer than `patience.merge`. The sheep
/// restarted before it are on the new build, and running the upgrade again
/// finishes the rest.
pub async fn restart_all(
    client: &Client,
    installed: &Path,
    patience: Patience,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let rows = flock(client).await?;
    let mut runners = Vec::new();
    for name in tables(client).await?.into_keys() {
        match kelpie_sheep(client, &rows, &name, &["runner", &name]).await {
            Ok(Some(found)) => runners.push(found),
            Ok(None) => {}
            Err(e) => say(format!("`{name}` left alone: {e}")),
        }
    }
    let dog = match kelpie_sheep(client, &rows, dog::NAME, &["dog"]).await {
        Ok(Some(dog)) => Some(dog),
        _ => kelpie_sheep(client, &rows, dog::OLD_NAME, &["dog"])
            .await
            .ok()
            .flatten(),
    };
    let (runners, stopped): (Vec<_>, Vec<_>) = runners.into_iter().partition(online);
    let (dog, stopped_dog): (Vec<_>, Vec<_>) = dog.into_iter().partition(online);
    for found in stopped_dog.iter().chain(&stopped) {
        say(format!(
            "`{}` is not running, so it starts on the new build",
            found.row.name
        ));
    }
    let names: Vec<&str> = runners.iter().map(|f| f.row.name.as_str()).collect();
    for found in dog.iter().chain(&runners) {
        if Path::new(&found.script) != installed {
            say(format!(
                "`{}` runs {}, not {}, so it stays on that build",
                found.row.name,
                found.script,
                installed.display()
            ));
        }
    }
    // The dog goes down only when no runner is merging.
    for found in &dog {
        wait_out_merges(client, &names, patience, say).await?;
        bounce(client, &found.row.name, patience, say).await?;
    }
    for found in &runners {
        let name = found.row.name.as_str();
        wait_out_merges(client, &[name], patience, say).await?;
        bounce(client, name, patience, say).await?;
    }
    Ok(())
}

fn online(found: &Found) -> bool {
    found.row.status == ProcStatus::Online
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
        if let Answered::Runner(_) = trigger(client, name, "status").await? {
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
            match trigger(client, name, "status").await? {
                Answered::Runner(body) => busy.extend(
                    merging(&body)
                        .into_iter()
                        .map(|issue| format!("`{name}` is merging #{issue}")),
                ),
                // Not taking its actions yet: it may be mid-merge from before.
                Answered::Starting => busy.push(format!("`{name}` is starting")),
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
