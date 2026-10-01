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
    let codex = migrate::codex(old, kelpie_home)?;
    let own = migrate::project(old, kelpie_home, project)?;
    let mut lines = migrate::run(&shared)?;
    for line in migrate::run(&codex)?.into_iter().chain(migrate::run(&own)?) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    // A runner's start moves kelpie's Codex login with the rest of its home,
    // and on its own when an earlier start moved the rest without it.
    #[test]
    fn a_runners_start_moves_the_codex_login() {
        let root = tempfile::tempdir().unwrap();
        let koji = ProjectName::try_from("koji").unwrap();
        let (old, new) = (root.path().join("a/.kelpie"), root.path().join("a/new"));
        write(&old.join("settings.toml"), "[webhook]\n");
        write(&old.join("codex/auth.json"), "{}");
        moved(&old, &new, &koji).unwrap();
        assert_eq!(
            std::fs::read_to_string(new.join("codex/auth.json")).unwrap(),
            "{}"
        );
        assert!(!old.join("codex").exists());

        let (old, new) = (root.path().join("b/.kelpie"), root.path().join("b/new"));
        write(&old.join("settings.toml"), "[webhook]\n");
        moved(&old, &new, &koji).unwrap();
        write(&old.join("codex/auth.json"), "{}");
        moved(&old, &new, &koji).unwrap();
        assert!(new.join("codex/auth.json").is_file());
    }
}
