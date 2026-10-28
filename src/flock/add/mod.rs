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
use shep_client::shep_core::config::DogTable;
use shep_client::shep_core::protocol::Request;
use shep_client::shep_core::protocol::request::Response;
use shep_client::shep_core::status::ProcStatus;

use super::share::share_none;
use super::{Checkout, Launch, flock, gateways, kelpie_sheep, send, tables};
use crate::board::{HUMAN, IN_PROGRESS, READY};
use crate::coderabbit::LABEL as SUMMON;
use crate::dog;
use crate::home::ProjectName;
use crate::ports::{Forge, NewLabel};
use crate::settings::{BaseBranch, Gateways, Settings, table_of};
use crate::shepherd::{Connection, DOG};

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
    /// The old home, `~/.kelpie`, a runner refuses to start beside, when
    /// `KELPIE_HOME` does not name its home instead
    pub old_home: Option<&'a Path>,
    /// The branch `--base` named, when it did: the repo's default branch
    /// otherwise
    pub base: Option<&'a str>,
}

/// A new project's settings, less what `add` reads from the checkout and
/// the forge. The schema's description of each key is what lookout shows
/// beside it.
const DEFAULTS: &str = r#"
[git]
merging = "ask"
issues = "ask"

[ci]
fix_attempts = -1

[concurrency]
active_items = 1
pending_rulings = 2

[budget]
per_item_usd = 10

[agents]

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
/// refusal is one `add` can see coming: a `--base` the repo has no branch
/// of, a name another sheep holds, or a project already set up for another
/// checkout or base.
pub async fn add(
    client: &dyn Connection,
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
    let base = match place.base {
        Some(named) => {
            let base = BaseBranch::try_from(named.to_owned())
                .map_err(|e| format!("`--base {named}` {e}"))?;
            let there = forge
                .branch_exists(repo, base.as_str())
                .map_err(|e| asked("branches", &e))?;
            if !there {
                return Err(format!("{slug} has no branch `{named}` to work against"));
            }
            base
        }
        None => BaseBranch::try_from(branch).map_err(|e| format!("{slug}'s default branch {e}"))?,
    };
    let rows = flock(client).await?;
    let mut done = Vec::new();
    let runner = kelpie_sheep(client, &rows, name.as_str(), &["runner", name.as_str()]).await?;
    let mut tables = tables(client).await?;
    share_none(
        &rows,
        &tables,
        name.as_str(),
        place.home,
        &checkout.root,
        slug,
    )
    .map_err(|taken| taken.to_string())?;
    // The runner's table as set, or the one to write.
    let (set, table) = match tables.remove(name.as_str()) {
        Some(set) => (true, set),
        None => {
            let gateways = gateways(client).await?;
            (false, settings(name, place, &base, &gateways)?)
        }
    };
    if set {
        let loaded = Settings::from_table(&table, name.as_str(), place.home, place.home)
            .map_err(|e| e.to_string())?;
        if loaded.git.checkout != checkout.root {
            return Err(format!(
                "project {name} runs from {}, not this checkout",
                loaded.git.checkout.display()
            ));
        }
        if place.base.is_some() && loaded.git.base() != base.as_str() {
            return Err(format!(
                "project {name} works against `{}`, not `{}`: `shep kelpie set git.base {}` \
                 changes it",
                loaded.git.base(),
                base.as_str(),
                base.as_str()
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
        if runner.is_some() && super::stop_on_refusal(client, name.as_str()).await? {
            done.push(format!(
                "runner `{name}`: now stays stopped after a start it refused"
            ));
        }
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

        if let Some(old) = place.old_home {
            let kelpie_home = place.folder.parent().unwrap_or(place.folder);
            if let Err(why) = crate::home::runner_may_start(old, kelpie_home, name) {
                done.push(format!(
                    "warning: runner `{name}` will refuse to start until this is dealt with: {why}"
                ));
            }
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
fn settings(
    name: &ProjectName,
    place: Place<'_>,
    base: &BaseBranch,
    gateways: &Gateways,
) -> Result<Map<String, Value>, String> {
    let root = &place.checkout.root;
    let mut table = table_of(DEFAULTS)?;
    let text = |s: &str| Value::String(s.to_owned());
    if let Some(Value::Object(repo)) = table.get_mut("git") {
        repo.insert("checkout".into(), text(&root.display().to_string()));
        repo.insert("remote".into(), text(place.checkout.forge.as_str()));
        repo.insert("base".into(), text(base.as_str()));
    }
    if let Some(Value::Object(ci)) = table.get_mut("ci") {
        let workflows = root.join(".github/workflows").is_dir();
        ci.insert("block".into(), Value::Bool(workflows));
    }
    // `iq3-xxs-review` first where kelpie holds a paddock gateway, and no
    // review bot: those are listed by hand.
    let reviewers = crate::settings::default_reviewers(gateways);
    let reviewers = reviewers.iter().map(|name| text(name.as_str())).collect();
    if let Some(Value::Object(agents)) = table.get_mut("agents") {
        agents.insert("reviewers".into(), Value::Array(reviewers));
    }
    super::set::migrate::pm_decides(&mut table);
    Settings::from_table(&table, name.as_str(), place.home, place.folder)
        .map_err(|e| e.to_string())?;
    Ok(table)
}

#[cfg(test)]
mod tests;
