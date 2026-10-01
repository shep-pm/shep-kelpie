//! A runner's start in kelpie's home: the move from the old home, and the
//! links it left there for old-build runners

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::flock;
use crate::home::migrate;
use crate::runner::ProjectName;
use crate::shepherd;

/// Moves kelpie's shared files and `project`'s own from the old home `old`
/// into `kelpie_home`, and points its state at them
///
/// # Errors
///
/// What could not move, so the runner never opens on half its files.
pub fn moved(old: &Path, kelpie_home: &Path, project: &ProjectName) -> Result<(), String> {
    let shared = migrate::shared(old, kelpie_home);
    let own = migrate::project(old, kelpie_home, project)?;
    let mut lines = migrate::run(&shared)?;
    for line in migrate::run(&own)? {
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines.iter().for_each(|line| println!("{line}"));
    if let Some(line) = migrate::repoint(old, kelpie_home, project)? {
        println!("{line}");
    }
    Ok(())
}

/// Removes the links the move left at `old` once every running runner of
/// the shepherd at `shep_home` started after them, but the one that made them
pub fn sweep_when_restarted(old: &Path, kelpie_home: &Path, shep_home: &Path) {
    let links = migrate::links(old, kelpie_home);
    let made = links
        .iter()
        .filter_map(|l| std::fs::symlink_metadata(l).ok()?.modified().ok())
        .max();
    let Some(made) = made else { return };
    let since = SystemTime::now()
        .duration_since(made)
        .unwrap_or(Duration::ZERO);
    let restarted = shepherd::block_on(async {
        let client = shepherd::connect(shep_home)
            .await
            .map_err(|e| e.describe(shep_home))?;
        flock::runners_younger_than(&client, since, made_by(kelpie_home)).await
    });
    match restarted {
        Ok(true) => migrate::sweep(old, kelpie_home)
            .iter()
            .for_each(|line| println!("{line}")),
        Ok(false) => {}
        Err(e) => eprintln!("cannot tell whether every runner is on this build: {e}"),
    }
}

// The runner that made the links: shep counts its uptime from before them,
// and it already runs this build.
fn made_by(kelpie_home: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(kelpie_home.join(migrate::LINKED_BY)).ok()?;
    text.trim().parse().ok()
}
