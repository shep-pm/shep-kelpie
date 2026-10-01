//! `shep kelpie upgrade`: installs a new kelpie and restarts the dog and each
//! runner onto it, and `--rollback` puts the previous build back
//!
//! `upgrade --ref <git ref>` builds kelpie at that ref, `--release <version>`
//! downloads a release, and `--binary <path>` takes a build made by hand as it
//! is. The installed kelpie is whatever program the adopted dog runs, read
//! from the shepherd, and every kelpie sheep runs that same path. The new
//! build is written beside it and renamed over it, as shep upgrades itself,
//! and the build it replaces is first copied into `<kelpie home>/builds`.
//!
//! The new build is asked for its shep line (`version --json`) and the
//! shepherd for its version before any file or sheep changes. A shepherd on
//! another minor stops the upgrade there, with the steps to take, because a
//! kelpie built for one shep minor refuses a shepherd on another and would
//! come up into that refusal. One upgrade runs at a time.

pub mod build;
pub mod fetch;
pub mod install;
pub mod lock;
pub mod restart;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use build::Build;
use install::{Change, Layout};
use lock::Lock;
use restart::{Patience, Plan};

use crate::shep_home;
use crate::shepherd::{self, release_line};

/// What the maintainer asked `upgrade` to do
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Install a build
    Install(Source),
    /// Put back the build the last upgrade replaced
    Rollback,
}

/// Where a build to install comes from
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A kelpie binary at this path, installed as it is
    Binary(PathBuf),
    /// Kelpie built at this git ref
    Ref(String),
    /// This released version of kelpie
    Release(String),
}

/// The usage `upgrade` prints for arguments it does not take
pub const USAGE: &str = "usage: shep-kelpie upgrade --ref <git ref> | --release <version> | \
                         --binary <path> | --rollback";

/// Reads `upgrade`'s arguments
///
/// # Errors
///
/// The usage line when they are not exactly one of the four forms.
pub fn parse(args: &[String]) -> Result<Action, String> {
    match args {
        [flag, value] if flag == "--ref" => Ok(Action::Install(Source::Ref(value.clone()))),
        [flag, value] if flag == "--release" => Ok(Action::Install(Source::Release(value.clone()))),
        [flag, value] if flag == "--binary" => {
            Ok(Action::Install(Source::Binary(PathBuf::from(value))))
        }
        [flag] if flag == "--rollback" => Ok(Action::Rollback),
        _ => Err(USAGE.to_owned()),
    }
}

/// Runs `shep-kelpie upgrade <args>`
pub fn main(args: &[String]) -> ExitCode {
    let action = match parse(args) {
        Ok(action) => action,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    let ran = shep_home::required(shep_home::FLOCK_FIX).and_then(|shep_home| {
        let scene = Scene {
            kelpie_home: &kelpie_home()?,
            shep_home: &shep_home,
            repo: &std::env::var("KELPIE_SOURCE").unwrap_or_else(|_| fetch::REPO.to_owned()),
            patience: Patience::default(),
        };
        let say = &mut |line: String| println!("{line}");
        shepherd::block_on(run(&scene, &action, say))
    });
    match ran {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("kelpie upgrade: {message}");
            ExitCode::FAILURE
        }
    }
}

// Kelpie's home is `KELPIE_HOME`, or `~/.kelpie`, as the runner reads it.
fn kelpie_home() -> Result<PathBuf, String> {
    std::env::var_os("KELPIE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".kelpie")))
        .ok_or_else(|| "HOME is not set".to_owned())
}

/// Where an upgrade happens
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    /// Kelpie's home, whose `builds` folder holds the previous build
    pub kelpie_home: &'a Path,
    /// The shepherd the sheep are restarted in
    pub shep_home: &'a Path,
    /// The git repo `--ref` builds from
    pub repo: &'a str,
    /// How long it waits on the shepherd's sheep
    pub patience: Patience,
}

/// Installs or rolls back a build, and restarts the dog and the runners onto it
///
/// Every line of progress goes to `say` as it happens, since a wait for a
/// merge can take a while.
///
/// # Errors
///
/// A message when the build cannot be had, run or installed, when the
/// shepherd cannot be reached or runs another shep minor than the build, or
/// when a restart fails. Nothing is installed or restarted on the first
/// three; the files and the sheep restarted before a later failure stay as
/// they are, and running the upgrade again finishes it.
pub async fn run(
    scene: &Scene<'_>,
    action: &Action,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let _lock = Lock::take(scene.kelpie_home)?;
    // Before a build is fetched: no shepherd, no upgrade.
    let client = shepherd::connect_any(scene.shep_home)
        .await
        .map_err(|e| e.describe(scene.shep_home))?;
    let running = client.daemon().daemon_version.clone();
    // Before a build is fetched: every sheep to restart runs the installed path.
    let plan = Plan::read(&client, say).await?;
    let layout = Layout::new(scene.kelpie_home, &plan.program);
    match action {
        Action::Install(source) => {
            let binary = fetch_source(scene, source)?;
            let staged = install::stage(&layout, &binary)?;
            let build = Build::of(staged.path())?;
            if let Source::Release(version) = source
                && build.kelpie != version.trim_start_matches('v')
            {
                return Err(format!(
                    "release {version} holds a kelpie that says it is {}",
                    build.kelpie
                ));
            }
            fits(&build, &running, scene.shep_home, &binary)?;
            let change = install::install(&layout, staged)?;
            say(match change {
                Change::First => format!(
                    "installed kelpie {} (shep {}) as {}",
                    build.kelpie,
                    build.shep,
                    layout.installed().display()
                ),
                Change::Replaced => format!(
                    "installed kelpie {} (shep {}), and kept the previous build{} for \
                     `shep kelpie upgrade --rollback`",
                    build.kelpie,
                    build.shep,
                    Build::of(&layout.previous())
                        .map_or(String::new(), |b| format!(", kelpie {}", b.kelpie))
                ),
                Change::Unchanged => format!(
                    "kelpie {} (shep {}) is installed already",
                    build.kelpie, build.shep
                ),
            });
        }
        Action::Rollback => {
            let staged = install::stage_previous(&layout)?;
            match Build::of(staged.path()) {
                Ok(build) => fits(&build, &running, scene.shep_home, &layout.previous())?,
                // A build from before `version --json` cannot say; the
                // maintainer asked for it by name.
                Err(_) => say(format!(
                    "{} does not say which shep it is made for, so the shepherd's minor is unchecked",
                    layout.previous().display()
                )),
            }
            install::install(&layout, staged)?;
            let back = match Build::of(layout.installed()) {
                Ok(b) => format!("kelpie {} (shep {})", b.kelpie, b.shep),
                Err(_) => layout.installed().display().to_string(),
            };
            say(format!(
                "put back {back}, and kept the build it replaced as the previous"
            ));
        }
    }
    restart::restart_all(&client, &plan, scene.patience, say).await
}

fn fetch_source(scene: &Scene<'_>, source: &Source) -> Result<PathBuf, String> {
    let work = scene.kelpie_home.join("upgrade");
    match source {
        Source::Binary(path) => {
            if !path.is_file() {
                return Err(format!("{} is not a file", path.display()));
            }
            Ok(path.clone())
        }
        Source::Ref(reference) => fetch::build_ref(&work, scene.repo, reference),
        Source::Release(version) => fetch::download_release(&work, version),
    }
}

// Whether `build` takes the shepherd at `running`, else what to do about it.
// `binary` is the build, which runs its own upgrade once the shepherd is moved.
fn fits(build: &Build, running: &str, shep_home: &Path, binary: &Path) -> Result<(), String> {
    let (theirs, ours) = (release_line(running), release_line(&build.shep));
    if theirs == ours {
        return Ok(());
    }
    let number = |line: (Option<&str>, Option<&str>)| {
        let part = |p: Option<&str>| p.and_then(|p| p.parse::<u64>().ok());
        part(line.0).zip(part(line.1))
    };
    let shepherd_first = format!(
        "\n  1. upgrade shep and reload its shepherd\n  2. run the new build's own upgrade: \
         `{0} upgrade --binary {0}`",
        binary.display()
    );
    let build_first = "install a kelpie built for the shepherd's shep line instead".to_owned();
    let what_to_do = match (number(theirs), number(ours)) {
        (Some(running), Some(built)) if built > running => shepherd_first,
        (Some(_), Some(_)) => build_first,
        _ => format!("either:{shepherd_first}\n  or {build_first}"),
    };
    Err(format!(
        "kelpie {} is made for shep {}, and the shepherd at {} runs shep {running}, which it \
         would refuse. Nothing was installed or restarted. To go on: {what_to_do}",
        build.kelpie,
        build.shep,
        shep_home.display()
    ))
}

#[cfg(test)]
mod tests;
