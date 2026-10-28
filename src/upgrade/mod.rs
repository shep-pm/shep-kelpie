//! `shep kelpie upgrade`: installs a new kelpie, restarts the dog and runners
//!
//! `--ref <git ref>` builds kelpie at that ref, `--release <version>` downloads a release, and
//! `--binary <path>` takes a hand-made build as it is. `--rollback` puts the previous build back.
//! The installed kelpie is the program the adopted dog runs, and the build it replaces is first
//! copied into `<kelpie home>/builds`. The agent files and the projects' settings are migrated
//! before the restarts. Before any file or sheep changes, the build gives its shep line
//! (`version --json`) and the shepherd its version. An earlier shep, or a refused protocol,
//! stops the upgrade there. Each runner is handed over in place or drained, unless `--now`.

pub mod drain;
pub mod fetch;
pub mod handover;
pub mod install;
pub mod lock;
mod migrate;
pub mod restart;
pub mod seen;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use install::{Change, Layout};
use lock::Lock;
use restart::{Interrupt, Patience, Plan};
use shep_client::shep_core::protocol::HelloAck;

use crate::home::Build;
use crate::shep_home;
use crate::shepherd::{self, Dial};

pub use migrate::main as migrate_main;

/// What the maintainer asked `upgrade` to do
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Install a build
    Install(Source),
    /// Restart onto the build already at the installed path, as `cargo install`
    /// leaves it
    Installed,
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
pub const USAGE: &str = "usage: shep-kelpie upgrade [--now] [--ref <git ref> | --release \
                         <version> | --binary <path> | --rollback]";

/// Takes `--now` off the front or the end of `upgrade`'s arguments, and
/// says whether it was there
pub fn now(args: &[String]) -> (bool, &[String]) {
    match args {
        [first, rest @ ..] if first == "--now" => (true, rest),
        [rest @ .., last] if last == "--now" => (true, rest),
        _ => (false, args),
    }
}

/// Reads `upgrade`'s arguments
///
/// # Errors
///
/// The usage line when they are not exactly one of the five forms.
pub fn parse(args: &[String]) -> Result<Action, String> {
    match args {
        [] => Ok(Action::Installed),
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
    let (now, args) = now(args);
    let action = match parse(args) {
        Ok(action) => action,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    let ran = shep_home::required(shep_home::FLOCK_FIX).and_then(|shep_home| {
        let scene = Scene {
            kelpie_home: &crate::home::kelpie_home_of(&shep_home),
            shep_home: &shep_home,
            shepherd: &shepherd::Socket,
            repo: &std::env::var("KELPIE_SOURCE").unwrap_or_else(|_| fetch::REPO.to_owned()),
            patience: Patience::default(),
            now,
            interrupt: Interrupt::Signals,
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

/// Where an upgrade happens
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    /// Kelpie's home, whose `builds` folder holds the previous build
    pub kelpie_home: &'a Path,
    /// The shepherd the sheep are restarted in
    pub shep_home: &'a Path,
    /// How the shepherd is reached
    pub shepherd: &'a dyn Dial,
    /// The git repo `--ref` builds from
    pub repo: &'a str,
    /// How long it waits on the shepherd's sheep
    pub patience: Patience,
    /// Restart each runner without handing it over or draining it first
    pub now: bool,
    /// What stops the restarts part way, sending a drained runner `undrain`
    pub interrupt: Interrupt,
}

/// Installs or rolls back a build, and restarts the dog and the runners onto it
///
/// Every line of progress goes to `say` as it happens, since a wait for a
/// merge can take a while.
///
/// # Errors
///
/// A message when the build cannot be had, run or installed, when the
/// shepherd cannot be reached or runs a shep the build refuses, when the
/// settings are not migrated, or when a restart fails. Nothing is installed or
/// restarted on the first three, and nothing restarted on the fourth. A
/// failure after the swap says what is installed and the command that
/// finishes the restarts, `upgrade --binary` on the installed file.
pub async fn run(
    scene: &Scene<'_>,
    action: &Action,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let _lock = Lock::take(scene.kelpie_home)?;
    // Before a build is fetched: no shepherd, no upgrade.
    let client = connect(scene).await?;
    let daemon = client.daemon();
    // Before a build is fetched: every sheep to restart runs the installed path.
    let early = Plan::read(&client, say).await?;
    let layout = Layout::new(scene.kelpie_home, &early.program);
    let staged = match action {
        Action::Installed => {
            if seen::unchanged(&layout) {
                say(format!(
                    "nothing changed since the last upgrade: {} is the build it restarted onto, \
                     so nothing was restarted",
                    layout.installed().display()
                ));
                return Ok(());
            }
            // Taken before the build is asked, so a file replaced after it is
            // not recorded as the one restarted onto.
            let taken = seen::stamp(&layout)?;
            let build = Build::of(layout.installed())?;
            fits(&build, &daemon, scene, layout.installed())?;
            (None, Some(build), Some(taken))
        }
        Action::Install(source) => {
            let binary = fetch_source(scene, source)?;
            let staged = install::stage(&layout, &binary)?;
            let build = Build::of(staged.path())?;
            if let Source::Release(version) = source {
                check_release(version, &build)?;
            }
            fits(&build, &daemon, scene, &binary)?;
            (Some(staged), Some(build), None)
        }
        Action::Rollback => {
            let staged = install::stage_previous(&layout)?;
            // The build put back, kept for the lines that name it and the
            // runners left behind on another one.
            let build = match Build::of(staged.path()) {
                Ok(build) => {
                    fits(&build, &daemon, scene, &layout.previous())?;
                    Some(build)
                }
                // Only a build from before `version --json` cannot say, and the
                // maintainer asked for it by name. One that does not run is refused.
                Err(e) if e.predates_version() => {
                    say(format!(
                        "{} does not say which shep it is made for, so the shepherd's version is \
                         unchecked",
                        layout.previous().display()
                    ));
                    None
                }
                Err(e) => return Err(e.into()),
            };
            (Some(staged), build, None)
        }
    };
    // The build can take minutes: look at the shepherd and its flock again,
    // since a sheep may have been stopped or moved meanwhile.
    let client = connect(scene).await?;
    let plan = Plan::read(&client, &mut |_| {}).await?;
    if plan.program != early.program {
        return Err(format!(
            "the dog runs {} now, and ran {} when the upgrade began. Nothing was installed or \
             restarted: run it again",
            plan.program.display(),
            early.program.display()
        ));
    }
    let (staged, build, taken) = staged;
    // A bare upgrade has no copy to swap in: the file is the build already.
    let change = match staged {
        Some(staged) => install::install(&layout, staged)?,
        None => Change::Unchanged,
    };
    // Before the restarts, which can wait on a merge while the file is replaced.
    let taken = taken.map_or_else(|| seen::stamp(&layout), Ok);
    let installed = layout.installed().display().to_string();
    let finish = format!("`shep kelpie upgrade --binary {installed}`");
    match (action, change) {
        (Action::Installed, _) => {
            let build = named(build.as_ref().expect("an install has a build"));
            say(format!("{build} is installed as {installed}"));
            // Without a record, a copy may be kept from an earlier upgrade: say which.
            let kept = Build::of(&layout.previous()).map_or_else(
                |_| "no earlier build is kept either".to_owned(),
                |b| {
                    format!(
                        "the previous build kept, kelpie {}, is the one before it",
                        b.kelpie
                    )
                },
            );
            say(format!(
                "`--rollback` has no copy of the build cargo replaced: {kept} (unless the last \
                 upgrade stopped after its own swap, which kept the build it replaced). `--ref`, \
                 `--release` and `--binary` keep one"
            ));
        }
        (Action::Rollback, _) => {
            let back = build.as_ref().map(named).unwrap_or(installed.clone());
            say(format!(
                "put back {back}, and kept the build it replaced as the previous"
            ));
        }
        (_, Change::First | Change::Replaced) => {
            let build = named(build.as_ref().expect("an install has a build"));
            let kept = Build::of(&layout.previous())
                .map_or(String::new(), |b| format!(", kelpie {}", b.kelpie));
            say(if change == Change::First {
                format!("installed {build} as {installed}")
            } else {
                format!(
                    "installed {build}, and kept the previous build{kept} for `shep kelpie \
                     upgrade --rollback`"
                )
            });
        }
        (_, Change::Unchanged) => {
            let build = named(build.as_ref().expect("an install has a build"));
            say(format!("{build} is installed already"));
        }
    }
    // A runner whose settings the new build would refuse is better left on the old one.
    migrate::run(scene, action, layout.installed(), &*client, say)
        .await
        .map_err(|why| {
            format!(
                "the settings were not migrated: {why}\nNothing was restarted. Once that is put \
                 right, `shep kelpie settings migrate` migrates them and {finish} restarts onto \
                 the new build"
            )
        })?;
    say(format!(
        "restarting onto it: if this stops before it finishes, {finish} finishes the restarts"
    ));
    restart::restart_all(
        &client,
        &plan,
        scene.patience,
        !scene.now,
        scene.interrupt,
        say,
    )
    .await
    .map_err(|e| {
        // A bare upgrade swapped nothing, so there is no swap for `--rollback` to undo.
        // After a rollback the installed build is the older one, and another
        // `--rollback` swaps forward again.
        let (installed_is, still_runs, swapped) = match action {
            Action::Installed => ("The new build", "the old one", String::new()),
            Action::Install(_) => (
                "The new build",
                "the old one",
                " `--rollback` would swap the builds back, not finish them".to_owned(),
            ),
            Action::Rollback => (
                "The rolled back build",
                "the build it replaced",
                " `--rollback` would swap the builds forward again, not finish them".to_owned(),
            ),
        };
        format!(
            "{e}\n{installed_is} is installed at {installed}, and a sheep not yet restarted \
                 may still run {still_runs}. To finish the restarts: {finish}.{swapped}"
        )
    })?;
    // Only a finished restart is recorded, so a bare run after a stop finishes it.
    if let Err(e) = taken.and_then(|taken| seen::record(&layout, &taken)) {
        say(format!(
            "{e}, so a bare `shep kelpie upgrade` will restart again"
        ));
    }
    // The upgrade is done: name the runners it left on another build, which is
    // what a runner whose restart did not take looks like from here.
    if let Some(commit) = build.and_then(|build| build.commit) {
        restart::still_behind(&client, &plan, &commit, say).await;
    }
    Ok(())
}

async fn connect(scene: &Scene<'_>) -> Result<Box<dyn crate::shepherd::Connection>, String> {
    shepherd::connect_any(scene.shepherd, scene.shep_home)
        .await
        .map_err(|e| e.describe(scene.shep_home))
}

// What a build is called in the lines that name it: its version, the shep it
// is made for, and the commit it was made from when it names one.
fn named(build: &Build) -> String {
    match build.commit.as_deref() {
        Some(commit) => format!(
            "kelpie {} (shep {}, built from {commit})",
            build.kelpie, build.shep
        ),
        None => format!("kelpie {} (shep {})", build.kelpie, build.shep),
    }
}

// A release must hold the kelpie it names, or nothing is installed.
fn check_release(version: &str, build: &Build) -> Result<(), String> {
    if build.kelpie == version.trim_start_matches('v') {
        return Ok(());
    }
    Err(format!(
        "release {version} holds a kelpie that says it is {}",
        build.kelpie
    ))
}

fn fetch_source(scene: &Scene<'_>, source: &Source) -> Result<PathBuf, String> {
    let work = scene.kelpie_home.join("upgrade");
    match source {
        Source::Binary(path) => {
            let meta = std::fs::metadata(path)
                .ok()
                .filter(std::fs::Metadata::is_file)
                .ok_or_else(|| format!("{} is not a file", path.display()))?;
            if !has_executable_bits(&meta) {
                return Err(format!(
                    "{} is not executable: `chmod +x {}` makes it so",
                    path.display(),
                    path.display()
                ));
            }
            Ok(path.clone())
        }
        Source::Ref(reference) => {
            fetch::build_ref(&work, scene.repo, reference, fetch::Limits::default())
        }
        #[cfg(windows)]
        Source::Release(_) => {
            Err("kelpie publishes no Windows release asset: use `--ref` or `--binary`".to_owned())
        }
        #[cfg(not(windows))]
        Source::Release(version) => fetch::download_release(&work, version),
    }
}

// Windows keeps no executable bit, so a regular file is taken as it is.
#[cfg(unix)]
fn has_executable_bits(meta: &std::fs::Metadata) -> bool {
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn has_executable_bits(_: &std::fs::Metadata) -> bool {
    true
}

// Whether `build` and the shepherd `daemon` take each other, else what to do
// about it. `binary` is the build, which runs its own upgrade once the
// shepherd is moved.
fn fits(build: &Build, daemon: &HelloAck, scene: &Scene<'_>, binary: &Path) -> Result<(), String> {
    let shep_home = scene.shep_home;
    let running = &daemon.daemon_version;
    let too_old = daemon
        .min_supported
        .zip(build.protocol)
        .is_some_and(|(oldest, spoken)| spoken < oldest);
    if shepherd::takes(&build.shep, running) && !too_old {
        return Ok(());
    }
    let shepherd_first = format!(
        "\n  1. upgrade shep and reload its shepherd\n  2. run the new build's own upgrade: \
         `SHEP_HOME={1} {0} upgrade --binary {0}`",
        binary.display(),
        shep_home.display()
    );
    let build_first = "install a kelpie built for the shepherd's shep line instead".to_owned();
    let what_to_do = match (shepherd::number(running), shepherd::number(&build.shep)) {
        _ if too_old => build_first,
        (Some(_), Some(_)) => shepherd_first,
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
