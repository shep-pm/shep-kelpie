//! `shep kelpie add`: a checkout becomes a project in the maintainer's flock
//!
//! Everything is read first, so a refusal changes nothing. Then the repo
//! gets the labels kelpie uses, kelpie's home gets its own agent files, and
//! the flock gets the project's runner holding its settings as its
//! `[app.dogs.kelpie]` table. Each is made only when it is missing, so a
//! second `add` changes nothing. The runner is registered stopped. The last
//! line names the command that starts it, or, for a runner already running,
//! the one that pauses it. The dog is the adopted kelpie, and `add` says
//! when it is not running.

use std::path::Path;

use serde_json::{Map, Value};
use shep_client::Client;
use shep_client::shep_core::config::DogTable;
use shep_client::shep_core::protocol::Request;
use shep_client::shep_core::protocol::request::Response;
use shep_client::shep_core::status::ProcStatus;

use super::{Checkout, Launch, flock, kelpie_sheep, send, tables};
use crate::board::READY;
use crate::dog;
use crate::ports::{Forge, NewLabel};
use crate::runner::{HUMAN, IN_PROGRESS, ProjectName, SUMMON_LABEL as SUMMON};
use crate::settings::{Settings, table_of};
use crate::shepherd::DOG;

/// The labels kelpie reads and sets on a project's issues and pull requests
pub const LABELS: [NewLabel<'static>; 4] = [
    NewLabel {
        name: READY,
        color: "0e8a16",
        description: "Kelpie may take this: an issue for its board, or a pull request to rework",
    },
    NewLabel {
        name: HUMAN,
        color: "fbca04",
        description: "Kelpie handed this back and waits on the maintainer",
    },
    NewLabel {
        name: IN_PROGRESS,
        color: "fbca04",
        description: "A worker session is on it",
    },
    NewLabel {
        name: SUMMON,
        color: "1d76db",
        description: "Asks CodeRabbit for a review; only kelpie puts it on",
    },
];

/// Where a project comes from on this machine
#[derive(Debug, Clone, Copy)]
pub struct Place<'a> {
    /// The checkout it runs from
    pub checkout: &'a Checkout,
    /// The maintainer's home folder, for `~/` in settings
    pub home: &'a Path,
    /// The project's own folder under kelpie's home, which a relative path in
    /// its settings is taken from
    pub folder: &'a Path,
    /// Kelpie's `agents` folder, where its own agent files are written
    pub agents: &'a Path,
}

/// A new project's settings, less what `add` reads from the checkout and
/// the forge. The schema's description of each key is what lookout shows
/// beside it.
const DEFAULTS: &str = r#"
merge_authority = "ask"
max_items = 1

[agents]
implementers = ["sonnet-high"]

[pacing]
enabled = true
kickoff_hours = 8

[worker]
allowed_domains = []
build_env = {}
guard_hooks = []
turn_timeout = 60
"#;

/// Registers `place.checkout` as project `name`, and says what it did
///
/// # Errors
///
/// A message naming what refused, before anything changed when the
/// refusal is one `add` can see coming: a default branch other than
/// `main`, a name another sheep holds, or a project already set up for
/// another checkout.
pub async fn add(
    client: &Client,
    forge: &dyn Forge,
    launch: &Launch,
    name: &ProjectName,
    place: Place<'_>,
) -> Result<Vec<String>, String> {
    let Place { checkout, .. } = place;
    // `shep disable kelpie` deletes a sheep named `kelpie`, the adopted dog's name.
    if name.as_str() == dog::NAME {
        return Err(format!(
            "`{name}` is kelpie's own name, so name the project: `shep kelpie add <project>`"
        ));
    }
    let repo = &checkout.forge;
    let slug = repo.as_str();
    let asked =
        |what: &str, e: &dyn core::fmt::Display| format!("cannot read {slug}'s {what}: {e}");
    let branch = forge
        .default_branch(repo)
        .map_err(|e| asked("default branch", &e))?;
    if branch != "main" {
        return Err(format!(
            "{slug}'s default branch is {branch}, and kelpie cuts and merges every branch \
             against `main`"
        ));
    }
    let rows = flock(client).await?;
    let mut done = Vec::new();
    let runner = kelpie_sheep(client, &rows, name.as_str(), &["runner", name.as_str()]).await?;
    let mut tables = tables(client).await?;
    // One runner per checkout and per repo: two would take the same issues.
    // Read from each table by its raw keys, so one that no longer parses
    // still counts.
    for row in rows.iter().filter(|r| r.name != name.as_str()) {
        let sheep = row.name.as_str();
        let Some(other) = tables.get(sheep).cloned() else {
            continue;
        };
        let text = |key: &str| other.get(key).and_then(Value::as_str).map(str::to_owned);
        let runs_from = text("repo").map(|repo| match repo.strip_prefix("~/") {
            Some(rest) => place.home.join(rest),
            None => repo.into(),
        });
        let same_repo = text("forge").as_deref() == Some(slug);
        if runs_from.as_deref() == Some(checkout.root.as_path()) || same_repo {
            return Err(format!(
                "project `{sheep}` already runs this checkout or {slug}: `shep kelpie start {sheep}`"
            ));
        }
    }
    // The runner's table as set, or the one to write.
    let (set, table) = match tables.remove(name.as_str()) {
        Some(set) => (true, set),
        None => (false, settings(name, place)?),
    };
    if set {
        let loaded = Settings::from_table(&table, name.as_str(), place.home, place.home)
            .map_err(|e| e.to_string())?;
        if loaded.repo != checkout.root {
            return Err(format!(
                "project {name} runs from {}, not this checkout",
                loaded.repo.display()
            ));
        }
    }

    // A failure part way says what had changed by then.
    let wrote = async {
        let have = forge.repo_labels(repo).map_err(|e| asked("labels", &e))?;
        for label in LABELS {
            if have.iter().any(|l| l == label.name) {
                done.push(format!("label `{}`: already on {slug}", label.name));
            } else {
                forge
                    .create_label(repo, &label)
                    .map_err(|e| format!("cannot make label `{}` on {slug}: {e}", label.name))?;
                done.push(format!("label `{}`: made on {slug}", label.name));
            }
        }

        let folder = place.agents.display();
        match crate::agents::write_defaults(place.agents, place.home).map_err(|e| e.to_string())? {
            written if written.is_empty() => {
                done.push(format!("agent files: kelpie's own already in {folder}"));
            }
            written => done.push(format!(
                "agent files: wrote {} in {folder}",
                written.join(", ")
            )),
        }

        let running = runner
            .as_ref()
            .is_some_and(|r| matches!(r.status, ProcStatus::Online | ProcStatus::Starting));
        match (runner, set) {
            (None, _) => {
                let request = Request::Add {
                    apps: vec![launch.runner(name, table)],
                };
                send(client, request, |r| matches!(r, Response::Added(_))).await?;
                done.push(format!("runner `{name}`: added with its settings, stopped"));
            }
            (Some(_), false) => {
                let request = Request::SetSheepDogSettings {
                    name: name.as_str().to_owned(),
                    dog: DOG.to_owned(),
                    table: Some(DogTable::from(table)),
                };
                send(client, request, |r| {
                    matches!(r, Response::SheepDogSettingsSet { .. })
                })
                .await?;
                done.push(format!(
                    "runner `{name}`: already there, and given its settings"
                ));
            }
            _ => done.push(format!("runner `{name}`: already there with its settings")),
        }

        done.extend(super::dog_down(&rows));
        done.push(match running {
            true => format!("runner `{name}` is running: `shep kelpie pause {name}` stops it"),
            false => format!("`shep kelpie start {name}` runs it"),
        });
        Ok::<(), String>(())
    }
    .await;
    match wrote {
        Ok(()) => Ok(done),
        Err(e) if done.is_empty() => Err(e),
        Err(e) => Err(format!("{e}, after this much: {}", done.join("; "))),
    }
}

// The project's settings: the defaults with what the checkout and the forge say.
fn settings(name: &ProjectName, place: Place<'_>) -> Result<Map<String, Value>, String> {
    let root = &place.checkout.root;
    let mut table = table_of(DEFAULTS)?;
    let text = |s: &str| Value::String(s.to_owned());
    table.insert("repo".into(), text(&root.display().to_string()));
    table.insert("forge".into(), text(place.checkout.forge.as_str()));
    table.insert(
        "ci".into(),
        Value::Bool(root.join(".github/workflows").is_dir()),
    );
    // `qwen` first where the maintainer's script is installed, as `add`
    // writes its file, and no review bot: those are listed by hand.
    let reviewers = crate::settings::default_reviewers(place.home);
    let reviewers = reviewers.iter().map(|name| text(name.as_str())).collect();
    if let Some(Value::Object(agents)) = table.get_mut("agents") {
        agents.insert("reviewers".into(), Value::Array(reviewers));
    }
    Settings::from_table(&table, name.as_str(), place.home, place.folder)
        .map_err(|e| e.to_string())?;
    Ok(table)
}

#[cfg(test)]
mod tests;
