//! Kelpie in the maintainer's own flock: `shep kelpie add`, `start`,
//! `pause` and `status`
//!
//! shep runs an adopted dog as `shep kelpie <args>`, in the caller's folder,
//! with `SHEP_HOME` naming the shepherd. `add` registers a checkout's runner
//! as a sheep of that shepherd, beside the `kelpie-dog` sheep that holds the
//! leases, and the others drive it with the triggers `shep trigger` sends.
//! An adopted dog gets no shepherd channel, so the dog runs as that sheep
//! and the adopted kelpie stays disabled (ADR 0003).

pub mod add;
pub mod control;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use serde_json::{Map, Value};
use shep_client::Client;
use shep_client::shep_core::config::{AppConfig, DogTable};
use shep_client::shep_core::protocol::request::{ProcessInfo, Response};
use shep_client::shep_core::protocol::{Request, SelectorSpec};
use shep_client::shep_core::values::UpDuration;

use crate::adapters::Gh;
use crate::runner::{ProjectName, ProjectPaths};
use crate::settings::ForgeSlug;
use crate::shepherd::{self, DOG};

/// Runs `kelpie <command> <args>` for `add`, `start`, `pause` or `status`
pub fn main(command: &str, args: &[String]) -> ExitCode {
    let ran = crate::shep_home::required(crate::shep_home::FLOCK_FIX)
        .and_then(|shep_home| shepherd::block_on(run(&shep_home, command, args)));
    match ran {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("kelpie {command}: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run(shep_home: &Path, command: &str, args: &[String]) -> Result<Vec<String>, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let here = std::env::current_dir().map_err(|e| format!("cannot read this folder: {e}"))?;
    let client = shepherd::connect(shep_home)
        .await
        .map_err(|e| e.describe(shep_home))?;
    let named = match args {
        [] => None,
        [name] => Some(ProjectName::try_from(name.as_str()).map_err(|e| e.to_string())?),
        _ => return Err(format!("usage: kelpie {command} [<project>]")),
    };
    // Named, or the one this checkout runs.
    let project = async || match &named {
        Some(name) => Ok(name.clone()),
        None => control::project_here(&client, &Checkout::of(&here)?.root, &home).await,
    };
    match (command, args) {
        ("add", _) => {
            let checkout = Checkout::of(&here)?;
            let name = match named {
                Some(name) => name,
                None => ProjectName::try_from(checkout.forge.name()).map_err(|e| e.to_string())?,
            };
            let kelpie_home = std::env::var_os("KELPIE_HOME").map(PathBuf::from);
            let launch = Launch {
                kelpie: std::env::current_exe()
                    .map_err(|e| format!("cannot find kelpie itself: {e}"))?,
                shep_home: shep_home.to_owned(),
                kelpie_home: kelpie_home.clone(),
            };
            let kelpie_home = kelpie_home.unwrap_or_else(|| home.join(".kelpie"));
            let old = ProjectPaths::under(&kelpie_home, &name).settings;
            let place = add::Place {
                checkout: &checkout,
                home: &home,
                old_settings: &old,
            };
            add::add(&client, &Gh, &launch, &name, place).await
        }
        ("start", _) => control::start(&client, &project().await?).await,
        ("pause", _) => control::pause(&client, &project().await?).await,
        ("status", []) => control::status(&client).await,
        _ => Err(format!("usage: kelpie {command}")),
    }
}

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
    /// Kelpie's home, when it is not `~/.kelpie`
    pub kelpie_home: Option<PathBuf>,
}

impl Launch {
    /// Project `name`'s runner, holding `table` as its settings
    pub fn runner(&self, name: &ProjectName, table: Map<String, Value>) -> AppConfig {
        let mut app = self.app(name.as_str(), &["runner", name.as_str()]);
        app.dogs.insert(DOG.to_owned(), DogTable::from(table));
        app
    }

    /// The dog, which holds the leases every runner asks for
    pub fn dog(&self) -> AppConfig {
        self.app(crate::dog::NAME, &["dog"])
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

/// Every sheep in the flock
async fn flock(client: &Client) -> Result<Vec<ProcessInfo>, String> {
    match client.request(Request::ListFlock).await {
        Ok(Response::Flock(rows)) => Ok(rows),
        Ok(other) => Err(format!("the flock listing came back as {other:?}")),
        Err(e) => Err(format!("cannot list the flock: {e}")),
    }
}

/// Every sheep's `[app.dogs.kelpie]` table, by sheep
async fn tables(client: &Client) -> Result<BTreeMap<String, Map<String, Value>>, String> {
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
async fn resume(client: &Client, name: &str) -> Result<(), String> {
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
            "https://github.com/Hazels-Lab/hazels-lab-website.git",
            "https://github.com/Hazels-Lab/hazels-lab-website",
            "git@github.com:Hazels-Lab/hazels-lab-website.git",
            "ssh://git@github.com/Hazels-Lab/hazels-lab-website/",
        ] {
            let slug = forge_of(url).unwrap_or_else(|| panic!("{url}"));
            assert_eq!(slug.as_str(), "Hazels-Lab/hazels-lab-website", "{url}");
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
            shep_home: "/home/m/.shep".into(),
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
            Some("/home/m/.shep")
        );
        assert!(!app.env.contains_key("KELPIE_HOME"));
        assert!(app.channel && app.shutdown_with_message && app.autorestart);
        assert_eq!(app.kill_timeout.as_millis(), 10_000);
        assert_eq!(app.dogs.get(DOG).map(DogTable::as_map), Some(&table));
        let dog = launch.dog();
        assert_eq!(
            (dog.name.as_str(), dog.args.as_slice()),
            ("kelpie-dog", &["dog".to_owned()][..])
        );
        assert!(dog.dogs.is_empty());
    }
}
