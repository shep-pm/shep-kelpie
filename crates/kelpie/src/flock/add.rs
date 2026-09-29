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
use shep_client::shep_core::config::{AppConfig, DogTable};
use shep_client::shep_core::protocol::request::Response;
use shep_client::shep_core::protocol::{Request, SelectorSpec};
use shep_client::shep_core::status::ProcStatus;

use super::{Checkout, Found, Launch, flock, kelpie_sheep, send, tables};
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
    let mut done = Vec::new();
    // Kelpie adopted and left enabled holds the name `kelpie` as a dog, and
    // only restarts: its dog runs as a sheep of its own.
    let adopted = rows
        .iter()
        .any(|r| r.name == dog::OLD_NAME && r.dog.is_some());
    if adopted {
        done.push(format!(
            "kelpie is adopted and enabled, which only restarts it: run `shep disable {}`",
            dog::OLD_NAME
        ));
    }
    let runner = kelpie_sheep(client, &rows, name.as_str(), &["runner", name.as_str()]).await?;
    let old_dog = match adopted {
        true => None,
        false => kelpie_sheep(client, &rows, dog::OLD_NAME, &["dog"]).await?,
    };
    let dog = kelpie_sheep(client, &rows, dog::NAME, &["dog"]).await?;
    if old_dog.is_some() && dog.is_some() {
        return Err(format!(
            "both `{}` and `{}` run kelpie's dog, and one book needs one dog",
            dog::OLD_NAME,
            dog::NAME
        ));
    }
    let mut tables = tables(client).await?;
    // One runner per checkout and per repo: two would take the same issues.
    for (sheep, table) in &tables {
        let Ok(other) = Settings::from_table(table, sheep, place.home, place.home) else {
            continue;
        };
        if sheep != name.as_str() && (other.repo == checkout.root || other.forge == *repo) {
            return Err(format!(
                "project `{sheep}` already runs {} from {}: `shep kelpie start {sheep}`",
                other.forge.as_str(),
                other.repo.display()
            ));
        }
    }
    // The runner's table as set, or the one to write.
    let (set, table) = match tables.remove(name.as_str()) {
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
    let new_dog = dog_app(launch, old_dog.as_ref())?;

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

        match (dog, new_dog) {
            (Some(_), _) => done.push(format!("dog `{}`: already there", dog::NAME)),
            (None, new_dog) => replace_dog(client, new_dog, old_dog, &mut done).await?,
        }
        Ok::<(), String>(())
    }
    .await;
    match wrote {
        Ok(()) => Ok(done),
        Err(e) if done.is_empty() => Err(e),
        Err(e) => Err(format!("{e}, after this much: {}", done.join("; "))),
    }
}

/// The dog's half of [`add`] alone: `kelpie-dog` in place of a Flockfile
/// dog under the old name `kelpie`, keeping its variables and its book
///
/// # Errors
///
/// A message when the flock runs the dog under both names, a variable the
/// old entry sets is missing here, or a step fails, naming what changed.
pub async fn move_dog(client: &Client, launch: &Launch) -> Result<Vec<String>, String> {
    let rows = flock(client).await?;
    let old = kelpie_sheep(client, &rows, dog::OLD_NAME, &["dog"]).await?;
    if kelpie_sheep(client, &rows, dog::NAME, &["dog"])
        .await?
        .is_some()
    {
        return match old {
            Some(_) => Err("both `kelpie` and `kelpie-dog` run kelpie's dog".into()),
            None => Ok(vec![format!("dog `{}`: already there", dog::NAME)]),
        };
    }
    let new_dog = dog_app(launch, old.as_ref())?;
    let mut done = Vec::new();
    match replace_dog(client, new_dog, old, &mut done).await {
        Ok(()) => Ok(done),
        Err(e) => Err(format!("{e}, after this much: {}", done.join("; "))),
    }
}

// The dog's sheep as `add` makes it. One replacing a Flockfile dog keeps
// every variable that entry set: shep withholds their values, so each is
// taken from this command's own environment, and one missing there stops
// `add` before anything changes.
fn dog_app(launch: &Launch, old: Option<&Found>) -> Result<AppConfig, String> {
    let mut app = launch.dog();
    for key in old.map_or(&[][..], |old| old.env_keys.as_slice()) {
        if app.env.contains_key(key) {
            continue;
        }
        let value = std::env::var(key).map_err(|_| {
            format!(
                "`{}`'s entry sets {key}, which shep does not hand back: run this with {key} \
                 set as that entry sets it",
                dog::OLD_NAME
            )
        })?;
        app.env.insert(key.clone(), value);
    }
    Ok(app)
}

// Adds the dog's sheep. A dog set up from a Flockfile under its old name
// holds the name `shep adopt` needs, so it goes first, which leaves at most
// one dog if anything after it fails, and the new one starts at once when
// the old one ran: its book is on disk.
async fn replace_dog(
    client: &Client,
    new_dog: AppConfig,
    old: Option<Found>,
    done: &mut Vec<String>,
) -> Result<(), String> {
    if old.is_some() {
        let delete = Request::Delete {
            selector: SelectorSpec::Name(dog::OLD_NAME.to_owned()),
        };
        send(client, delete, |r| matches!(r, Response::Deleted(_))).await?;
        done.push(format!(
            "dog `{}`: deleted, since it held the name kelpie is adopted under. If what \
             follows failed, `shep kelpie add` again adds `{}`, which reads the same book",
            dog::OLD_NAME,
            dog::NAME
        ));
    }
    let request = Request::Add {
        apps: vec![new_dog],
    };
    send(client, request, |r| matches!(r, Response::Added(_))).await?;
    match old {
        Some(old) if old.row.status == ProcStatus::Online => {
            super::resume(client, dog::NAME).await?;
            done.push(format!("dog `{}`: added and started", dog::NAME));
        }
        _ => done.push(format!(
            "dog `{}`: added, stopped until `shep kelpie start`",
            dog::NAME
        )),
    }
    Ok(())
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
