//! The migrations an upgrade runs after the build is installed and before the restarts
//!
//! The agent files are migrated first, then the projects' settings. A bare
//! upgrade runs this process's steps, since the file on disk is the build
//! already. An install runs the installed build's `upgrade migrate`, which runs
//! that build's steps, since this process predates the build it installed and
//! does not know its migrations. A rollback runs none.

use std::path::Path;
use std::process::ExitCode;

use super::{Action, Scene};
use crate::agent_verbs;
use crate::flock::set::Homes;
use crate::home::KELPIE_VAR;
use crate::shepherd::{self, Connection, Socket};

// How a build's `upgrade` usage begins, which one older than `upgrade migrate` prints for it.
const OLD_USAGE: &str = "usage: shep-kelpie upgrade";

/// Runs `shep-kelpie upgrade migrate`: this build's migrations, a line for each
pub fn main() -> ExitCode {
    let ran = crate::shep_home::required(crate::shep_home::FLOCK_FIX).and_then(|shep_home| {
        let kelpie_home = crate::home::kelpie_home_of(&shep_home);
        shepherd::block_on(async {
            let client = shepherd::connect(&Socket, &shep_home)
                .await
                .map_err(|e| e.describe(&shep_home))?;
            let say = &mut |line: String| println!("{line}");
            steps(&kelpie_home, &shep_home, &client, say).await
        })
    });
    match ran {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Migrates the agent files and the settings for `action`, giving each line to `say`
///
/// An agent migration that fails is said and not fatal: the restarted dog
/// migrates the agent files at its start.
///
/// # Errors
///
/// Why the settings were not migrated, so the runners are not restarted onto
/// a build that may refuse them.
pub(super) async fn run(
    scene: &Scene<'_>,
    action: &Action,
    installed: &Path,
    client: &dyn Connection,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    match action {
        Action::Rollback => Ok(()),
        Action::Installed => steps(scene.kelpie_home, scene.shep_home, client, say).await,
        Action::Install(_) => by_build(scene, installed, say),
    }
}

// This build's steps, as `agents migrate` and `settings migrate` take them.
async fn steps(
    kelpie_home: &Path,
    shep_home: &Path,
    client: &dyn Connection,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let folder = kelpie_home.join(crate::agents::FOLDER);
    match agent_verbs::migrate(&folder) {
        Ok(lines) => lines.into_iter().for_each(&mut *say),
        Err(why) => say(format!(
            "the agent files were not migrated: {why}. The dog migrates them when it starts, or \
             `shep kelpie agents migrate` does"
        )),
    }
    let home = crate::home::user_home().ok_or(crate::home::HOME_UNSET)?;
    let homes = Homes {
        home: &home,
        kelpie_home,
        shep_home,
    };
    let lines = crate::flock::set::migrate::migrate(client, homes).await?;
    lines.into_iter().for_each(say);
    Ok(())
}

// Says the lines `<installed> upgrade migrate` prints, and fails as it does.
fn by_build(
    scene: &Scene<'_>,
    installed: &Path,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let output = crate::spawn::command(installed)
        .args(["upgrade", "migrate"])
        .env("SHEP_HOME", scene.shep_home)
        .env(KELPIE_VAR, scene.kelpie_home)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {} upgrade migrate: {e}", installed.display()))?;
    let said = String::from_utf8_lossy(&output.stdout);
    said.lines().map(str::to_owned).for_each(&mut *say);
    let err = String::from_utf8_lossy(&output.stderr);
    // A build older than the verb answers it with its usage, having changed nothing.
    if output.status.code() == Some(2) && err.starts_with(OLD_USAGE) {
        say(format!(
            "{} predates `upgrade migrate`, so it migrated nothing",
            installed.display()
        ));
        return Ok(());
    }
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{} upgrade migrate exited {}: {}",
        installed.display(),
        output.status,
        err.trim()
    ))
}
