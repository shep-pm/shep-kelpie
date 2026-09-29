//! `shep kelpie add`: a checkout becomes a project in the maintainer's flock
//!
//! Everything is read first, so a refusal changes nothing. Then the repo
//! gets the labels kelpie uses, the flock gets the project's runner holding
//! its settings as its `[app.dogs.kelpie]` table, and the dog's sheep. Each
//! is made only when it is missing, so a second `add` changes nothing.
//! Both sheep are registered stopped: `shep kelpie start` starts them.

use std::path::Path;

use serde_json::{Map, Value};
use shep_client::Client;
use shep_client::shep_core::config::DogTable;
use shep_client::shep_core::protocol::request::{ProcessInfo, Response};
use shep_client::shep_core::protocol::{Request, SelectorSpec};

use super::{Checkout, Launch, flock, send, tables};
use crate::board::READY;
use crate::dog;
use crate::ports::{Forge, NewLabel, Visibility};
use crate::runner::{HUMAN, ProjectName, SUMMON_LABEL as SUMMON};
use crate::settings::{Settings, table_of};
use crate::shepherd::DOG;

/// The labels kelpie reads and sets on a project's issues and pull requests
pub const LABELS: [NewLabel; 3] = [
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
    /// The project's settings file from before the tables, if it has one
    pub old_settings: &'a Path,
}

/// A new project's settings, less what `add` reads from the checkout and
/// the forge. The schema's description of each key is what lookout shows
/// beside it.
const DEFAULTS: &str = r#"
merge_authority = "ask"
max_items = 1
generated = []

[models.worker]
model = "claude-sonnet-5"
effort = "medium"

[models.reviewer]
model = "claude-sonnet-5"
effort = "medium"

[models.judge]
model = "claude-opus-5-5"
effort = "low"

[models.relay]
model = "claude-haiku-4-5-20251001"
effort = "low"

[review]
loop_guard = 8

[coderabbit]
divisor = 1000

[pacing]
enabled = true
kickoff_hours = 8

[worker]
allowed_domains = []
build_env = {}
guard_hooks = []
turn_timeout = 60
"#;

/// The maintainer's own local-round command, run when it is installed
const QWEN_REVIEW: &str = "~/.claude/scripts/qwen-review.sh";

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
    if [dog::NAME, dog::OLD_NAME].contains(&name.as_str()) {
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
    let public = forge
        .visibility(repo)
        .map_err(|e| asked("visibility", &e))?
        == Visibility::Public;
    let rows = flock(client).await?;
    let runner = kelpie_sheep(client, &rows, name.as_str(), &["runner", name.as_str()]).await?;
    let old_dog = kelpie_sheep(client, &rows, dog::OLD_NAME, &["dog"]).await?;
    let dog = kelpie_sheep(client, &rows, dog::NAME, &["dog"]).await?;
    if old_dog.is_some() && dog.is_some() {
        return Err(format!(
            "both `{}` and `{}` run kelpie's dog, and one book needs one dog",
            dog::OLD_NAME,
            dog::NAME
        ));
    }
    // The runner's table as set, or the one to write.
    let (set, table) = match tables(client).await?.remove(name.as_str()) {
        Some(set) => (true, set),
        None => (false, settings(name, place, public)?),
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
    let mut done = Vec::new();
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

        match (runner, set) {
            (None, _) => {
                let request = Request::Add {
                    apps: vec![launch.runner(name, table)],
                };
                send(client, request, |r| matches!(r, Response::Added(_))).await?;
                done.push(format!(
                    "runner `{name}`: added with its settings, stopped until `shep kelpie start`"
                ));
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

        done.push(replace_dog(client, launch, old_dog, dog).await?);
        Ok::<(), String>(())
    }
    .await;
    match wrote {
        Ok(()) => Ok(done),
        Err(e) if done.is_empty() => Err(e),
        Err(e) => Err(format!("{e}, after this much: {}", done.join("; "))),
    }
}

// The dog's sheep, made when missing. A dog set up from a Flockfile under
// its old name holds the name `shep adopt` needs, so it is replaced, and
// the new one started at once when the old one ran: its book is on disk.
// The old one goes first, so a failure leaves at most one dog, and `add`
// again finishes the job.
async fn replace_dog(
    client: &Client,
    launch: &Launch,
    old: Option<ProcessInfo>,
    dog: Option<ProcessInfo>,
) -> Result<String, String> {
    // `add` refused both before it wrote anything.
    if dog.is_some() {
        return Ok(format!("dog `{}`: already there", dog::NAME));
    }
    if old.is_some() {
        let delete = Request::Delete {
            selector: SelectorSpec::Name(dog::OLD_NAME.to_owned()),
        };
        send(client, delete, |r| matches!(r, Response::Deleted(_))).await?;
    }
    let request = Request::Add {
        apps: vec![launch.dog()],
    };
    send(client, request, |r| matches!(r, Response::Added(_))).await?;
    let Some(old) = old else {
        return Ok(format!(
            "dog `{}`: added, stopped until `shep kelpie start`",
            dog::NAME
        ));
    };
    if old.status == shep_client::shep_core::status::ProcStatus::Online {
        super::resume(client, dog::NAME).await?;
    }
    Ok(format!(
        "dog `{}`: replaces `{}`, which held the name kelpie is adopted under",
        dog::NAME,
        dog::OLD_NAME
    ))
}

// The row named `name`, if the flock has one, refusing one that is not
// kelpie's own: a dog, or a sheep started with other arguments.
async fn kelpie_sheep(
    client: &Client,
    rows: &[ProcessInfo],
    name: &str,
    args: &[&str],
) -> Result<Option<ProcessInfo>, String> {
    let Some(row) = rows.iter().find(|r| r.name == name) else {
        return Ok(None);
    };
    let taken = || format!("a sheep named `{name}` is already in this flock, and is not kelpie's");
    if row.dog.is_some() {
        return Err(taken());
    }
    let request = Request::SheepConfig {
        name: name.to_owned(),
    };
    match client.request(request).await {
        Ok(Response::SheepConfig(view)) if view.config.args == args => Ok(Some(row.clone())),
        Ok(Response::SheepConfig(_)) => Err(taken()),
        Ok(other) => Err(format!("the shepherd answered {other:?} for `{name}`")),
        Err(e) => Err(format!("cannot read `{name}`'s config: {e}")),
    }
}

// The project's settings: its file from before the tables when it has one,
// else the defaults with what the checkout and the forge say.
fn settings(
    name: &ProjectName,
    place: Place<'_>,
    public: bool,
) -> Result<Map<String, Value>, String> {
    let root = &place.checkout.root;
    let table = match std::fs::read_to_string(place.old_settings) {
        Ok(text) => table_of(&text)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut table = table_of(DEFAULTS)?;
            let text = |s: &str| Value::String(s.to_owned());
            table.insert("repo".into(), text(&root.display().to_string()));
            table.insert("forge".into(), text(place.checkout.forge.as_str()));
            table.insert(
                "ci".into(),
                Value::Bool(root.join(".github/workflows").is_dir()),
            );
            if let Some(Value::Object(coderabbit)) = table.get_mut("coderabbit") {
                coderabbit.insert("enabled".into(), Value::Bool(public));
            }
            let installed = QWEN_REVIEW
                .strip_prefix("~/")
                .is_some_and(|script| place.home.join(script).is_file());
            let local = if installed {
                [("kind", "command"), ("command", QWEN_REVIEW)].as_slice()
            } else {
                [("kind", "off")].as_slice()
            };
            let local = local
                .iter()
                .map(|&(k, v)| (k.to_owned(), text(v)))
                .collect();
            if let Some(Value::Object(review)) = table.get_mut("review") {
                review.insert("local".into(), Value::Object(local));
            }
            table
        }
        Err(e) => return Err(format!("cannot read {}: {e}", place.old_settings.display())),
    };
    let folder = place.old_settings.parent().unwrap_or(place.home);
    let loaded = Settings::from_table(&table, name.as_str(), place.home, folder)
        .map_err(|e| e.to_string())?;
    if loaded.repo != *root || loaded.forge != place.checkout.forge {
        return Err(format!(
            "{} sets project {name} up for {} at {}, not this checkout",
            place.old_settings.display(),
            loaded.forge.as_str(),
            loaded.repo.display()
        ));
    }
    Ok(table)
}

#[cfg(test)]
mod tests;
