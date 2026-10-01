//! Kelpie in the maintainer's own flock: `shep kelpie <verb>`
//!
//! shep runs an adopted dog as `shep kelpie <args>`, in the caller's folder,
//! with `SHEP_HOME` naming the shepherd. `add` registers a checkout's runner
//! as a sheep of that shepherd, and every other verb sends it the trigger
//! `shep trigger` would, to the project `-p` names or whose repo holds the
//! folder. The adopted kelpie, enabled, is the dog that holds the leases
//! (ADR 0004).

pub mod add;
pub mod control;
pub mod rule;
mod verbs;

pub use verbs::{USAGE, VERBS, main, split_project, verb_first};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Map, Value};
use shep_client::Client;
use shep_client::shep_core::config::{AppConfig, DogTable};
use shep_client::shep_core::protocol::request::{DogSource, ProcessInfo, Response};
use shep_client::shep_core::protocol::{Request, SelectorSpec};
use shep_client::shep_core::status::ProcStatus;
use shep_client::shep_core::values::UpDuration;

use crate::runner::ProjectName;
use crate::settings::ForgeSlug;
use crate::shepherd::DOG;

/// A git checkout, and the forge repo its `origin` remote names
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// The checkout's top folder
    pub root: PathBuf,
    /// The repo on the forge
    pub forge: ForgeSlug,
}

impl Checkout {
    /// The checkout `folder` is in
    ///
    /// # Errors
    ///
    /// A message when `folder` is in no git checkout, or its `origin` is
    /// missing or not a GitHub repo.
    pub fn of(folder: &Path) -> Result<Self, String> {
        // Git's answer, or none when it ran and refused; a git that cannot run is an error.
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(folder)
                .args(args)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("cannot run git: {e}"))?;
            let answer = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            Ok::<_, String>(output.status.success().then_some(answer))
        };
        let root = git(&["rev-parse", "--show-toplevel"])?
            .ok_or_else(|| format!("{} is not in a git checkout", folder.display()))?;
        let url = git(&["remote", "get-url", "origin"])?
            .ok_or_else(|| format!("the checkout at {root} has no `origin` remote"))?;
        let forge = forge_of(&url)
            .ok_or_else(|| format!("`origin` is {url}, and kelpie works only with GitHub repos"))?;
        Ok(Self {
            root: PathBuf::from(root),
            forge,
        })
    }
}

/// The GitHub repo a remote's URL names, over HTTPS or SSH
pub fn forge_of(url: &str) -> Option<ForgeSlug> {
    let path = [
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
        "git@github.com:",
    ]
    .iter()
    .find_map(|prefix| url.strip_prefix(prefix))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    ForgeSlug::try_from(path.to_owned()).ok()
}

/// What kelpie's sheep are started with
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// Kelpie's own binary
    pub kelpie: PathBuf,
    /// The shepherd's home, which a sheep is not given unless its entry says
    pub shep_home: PathBuf,
    /// Kelpie's home, when `KELPIE_HOME` names one in place of `$SHEP_HOME/kelpie`
    pub kelpie_home: Option<PathBuf>,
}

impl Launch {
    /// Project `name`'s runner, holding `table` as its settings
    pub fn runner(&self, name: &ProjectName, table: Map<String, Value>) -> AppConfig {
        let mut app = self.app(name.as_str(), &["runner", name.as_str()]);
        app.dogs.insert(DOG.to_owned(), DogTable::from(table));
        app
    }

    // A runner needs about 7s to stop cleanly, so shep's 1.6s default kill
    // timeout is raised, and it stops on the channel's shutdown message.
    fn app(&self, name: &str, args: &[&str]) -> AppConfig {
        let mut app = AppConfig::minimal(name, &self.kelpie.display().to_string());
        app.args = args.iter().map(|&a| a.to_owned()).collect();
        app.env
            .insert("SHEP_HOME".to_owned(), self.shep_home.display().to_string());
        if let Some(home) = &self.kelpie_home {
            app.env
                .insert("KELPIE_HOME".to_owned(), home.display().to_string());
        }
        app.autorestart = true;
        app.channel = true;
        app.shutdown_with_message = true;
        app.kill_timeout = UpDuration::from_millis(10_000);
        app
    }
}

/// What stops the adopted kelpie holding the leases, with the fix, or
/// `None` when it runs with its shepherd channel
pub(crate) fn dog_down(rows: &[ProcessInfo]) -> Option<String> {
    dog_problem(rows).map(|(what, fix)| format!("{what}: {fix}"))
}

/// What stops the adopted kelpie holding the leases, and the fix, apart
///
/// A dog that runs but never named itself to the shepherd is `silent` in
/// shep's listing, and holds nothing: shep restarts it once and then gives
/// up, so it reads as down with a fix of its own.
pub(crate) fn dog_problem(rows: &[ProcessInfo]) -> Option<(String, String)> {
    let name = crate::dog::NAME;
    let row = rows.iter().find(|r| r.name == name);
    let problem = |what: &str, fix: String| Some((what.to_owned(), fix));
    match row.and_then(|r| Some((r.dog.as_ref()?, r.status, r.handshook, r.dog_stale))) {
        Some((
            DogSource::Adopted {
                channel: false,
                path,
            },
            ..,
        )) => problem(
            "kelpie's dog has no shepherd channel, since kelpie was adopted before it asked for one",
            format!(
                "run `shep adopt {path} --name {name}`, then `shep disable {name}` and \
                 `shep enable {name}`"
            ),
        ),
        Some((DogSource::Adopted { .. }, ProcStatus::Online, _, Some(true))) => problem(
            "kelpie's dog is silent and shep has given up on it",
            format!("`shep bleats {name}` says why, and `shep restart {name}` runs it again"),
        ),
        Some((DogSource::Adopted { .. }, ProcStatus::Online, Some(false), _)) => problem(
            "kelpie's dog is silent, since it has not named itself to the shepherd",
            format!("give it a few seconds after a start, then `shep bleats {name}` says why"),
        ),
        Some((DogSource::Adopted { .. }, ProcStatus::Online, ..)) => None,
        Some((DogSource::Adopted { .. }, ..)) => problem(
            "kelpie's dog is not running",
            format!("`shep bleats {name}` says why"),
        ),
        _ => problem(
            "kelpie's dog is not enabled",
            format!("`shep enable {name}` runs it"),
        ),
    }
}

/// The sheep named `name`, if the flock has one, refusing one that is not
/// kelpie's: a dog, or a sheep started with arguments other than `args`
pub(crate) async fn kelpie_sheep(
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

/// Whether every running sheep started as a kelpie runner has been up for
/// less than `age`
///
/// # Errors
///
/// A message when the flock or a sheep's config cannot be read.
pub(crate) async fn runners_younger_than(client: &Client, age: Duration) -> Result<bool, String> {
    let rows = flock(client).await?;
    let up = rows
        .iter()
        .filter(|r| r.dog.is_none() && r.status == ProcStatus::Online);
    for row in up {
        if Duration::from_millis(row.uptime_ms) < age {
            continue;
        }
        let request = Request::SheepConfig {
            name: row.name.clone(),
        };
        match client.request(request).await {
            Ok(Response::SheepConfig(view))
                if view.config.args.first().is_some_and(|a| a == "runner") =>
            {
                return Ok(false);
            }
            Ok(Response::SheepConfig(_)) => {}
            Ok(other) => {
                return Err(format!(
                    "the shepherd answered {other:?} for `{}`",
                    row.name
                ));
            }
            Err(e) => return Err(format!("cannot read `{}`'s config: {e}", row.name)),
        }
    }
    Ok(true)
}

/// The program `name`'s entry runs
pub(crate) async fn script(client: &Client, name: &str) -> Result<String, String> {
    let request = Request::SheepConfig {
        name: name.to_owned(),
    };
    match client.request(request).await {
        Ok(Response::SheepConfig(view)) => Ok(view.config.script),
        Ok(other) => Err(format!("the shepherd answered {other:?} for `{name}`")),
        Err(e) => Err(format!("cannot read `{name}`'s config: {e}")),
    }
}

/// Every sheep in the flock
pub(crate) async fn flock(client: &Client) -> Result<Vec<ProcessInfo>, String> {
    match client.request(Request::ListFlock).await {
        Ok(Response::Flock(rows)) => Ok(rows),
        Ok(other) => Err(format!("the flock listing came back as {other:?}")),
        Err(e) => Err(format!("cannot list the flock: {e}")),
    }
}

/// Every sheep's `[app.dogs.kelpie]` table, by sheep
pub(crate) async fn tables(
    client: &Client,
) -> Result<BTreeMap<String, Map<String, Value>>, String> {
    match client
        .request(Request::DogSheepSettings { dog: DOG.into() })
        .await
    {
        Ok(Response::DogSheepSettings { tables }) => Ok(tables
            .into_iter()
            .map(|(sheep, table)| (sheep, table.as_map().clone()))
            .collect()),
        Ok(other) => Err(format!("the sheep's tables came back as {other:?}")),
        Err(e) => Err(format!("cannot read the sheep's tables: {e}")),
    }
}

/// Sends `request`, which names one sheep, and refuses any other answer than `expect`
async fn send(
    client: &Client,
    request: Request,
    expect: fn(&Response) -> bool,
) -> Result<(), String> {
    let what = format!("{request:?}");
    match client.request(request).await {
        Ok(reply) if expect(&reply) => Ok(()),
        Ok(other) => Err(format!("the shepherd answered {other:?} to {what}")),
        Err(e) => Err(format!("the shepherd refused {what}: {e}")),
    }
}

/// Starts `name`, which the caller has seen registered and not running
///
/// A restart, so a sheep that is running is restarted: callers check first.
pub(crate) async fn resume(client: &Client, name: &str) -> Result<(), String> {
    let request = Request::Restart {
        selector: SelectorSpec::Name(name.to_owned()),
    };
    send(client, request, |r| matches!(r, Response::Restarted { .. })).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_github_remote_names_its_repo_over_https_or_ssh() {
        for url in [
            "https://github.com/shep-pm/koji-website.git",
            "https://github.com/shep-pm/koji-website",
            "git@github.com:shep-pm/koji-website.git",
            "ssh://git@github.com/shep-pm/koji-website/",
        ] {
            let slug = forge_of(url).unwrap_or_else(|| panic!("{url}"));
            assert_eq!(slug.as_str(), "shep-pm/koji-website", "{url}");
        }
    }

    #[test]
    fn a_checkout_is_its_top_folder_and_origin_s_repo() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = |args: &[&str]| {
            let ran = Command::new("git").arg("-C").arg(&root).args(args).output();
            assert!(ran.unwrap().status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        let inside = root.join("src/deep");
        std::fs::create_dir_all(&inside).unwrap();
        let err = Checkout::of(&inside).unwrap_err();
        assert!(err.ends_with("has no `origin` remote"), "{err}");

        git(&["remote", "add", "origin", "git@github.com:shep-pm/koji.git"]);
        let checkout = Checkout::of(&inside).unwrap();
        assert_eq!(checkout.root, root);
        assert_eq!(checkout.forge.as_str(), "shep-pm/koji");

        git(&[
            "remote",
            "set-url",
            "origin",
            "https://gitlab.com/shep-pm/koji",
        ]);
        let err = Checkout::of(&root).unwrap_err();
        assert!(err.contains("only with GitHub repos"), "{err}");
        let outside = tempfile::tempdir().unwrap();
        let err = Checkout::of(outside.path()).unwrap_err();
        assert!(err.ends_with("is not in a git checkout"), "{err}");
    }

    #[test]
    fn a_remote_elsewhere_names_no_repo() {
        for url in [
            "https://gitlab.com/a/b.git",
            "/srv/git/b.git",
            "https://github.com/a",
            "https://github.com/a/b/c",
        ] {
            assert_eq!(forge_of(url), None, "{url}");
        }
    }

    #[test]
    fn a_runner_is_a_channel_sheep_holding_its_table_and_the_shepherd_s_home() {
        let launch = Launch {
            kelpie: "/opt/kelpie".into(),
            shep_home: "/home/me/.shep".into(),
            kelpie_home: None,
        };
        let mut table = Map::new();
        table.insert("forge".into(), Value::String("o/r".into()));
        let name = ProjectName::try_from("koji").unwrap();
        let app = launch.runner(&name, table.clone());
        assert_eq!(
            (app.name.as_str(), app.script.as_str()),
            ("koji", "/opt/kelpie")
        );
        assert_eq!(app.args, ["runner", "koji"]);
        assert_eq!(
            app.env.get("SHEP_HOME").map(String::as_str),
            Some("/home/me/.shep")
        );
        assert!(!app.env.contains_key("KELPIE_HOME"));
        assert!(app.channel && app.shutdown_with_message && app.autorestart);
        assert_eq!(app.kill_timeout.as_millis(), 10_000);
        assert_eq!(app.dogs.get(DOG).map(DogTable::as_map), Some(&table));
    }
}
