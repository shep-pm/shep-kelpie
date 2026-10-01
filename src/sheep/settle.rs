//! A runner's start in kelpie's home: the move from the old home, git's
//! links to the moved worktrees, and the links left for old-build runners

use std::path::{Path, PathBuf};
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
    for line in migrate::run(&shared)?
        .into_iter()
        .chain(migrate::run(&own)?)
    {
        println!("{line}");
    }
    if let Some(line) = migrate::repoint(old, kelpie_home, project)? {
        println!("{line}");
    }
    Ok(())
}

/// Points git's links from `repo` at each worktree in `worktrees` again
///
/// A move that died before it relinked, or a link git wrote relative, would
/// otherwise be pruned. Repair changes nothing for a link that is right.
pub fn repair(repo: &Path, worktrees: &Path) {
    let Ok(entries) = std::fs::read_dir(worktrees) else {
        return;
    };
    let mut trees: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|tree| tree.join(".git").is_file())
        .collect();
    if trees.is_empty() {
        return;
    }
    trees.sort();
    let mut args = vec!["worktree".as_ref(), "repair".as_ref()];
    args.extend(trees.iter().map(|t| t.as_os_str()));
    if let Err(e) = crate::worktree::git(repo, args) {
        eprintln!(
            "cannot repair git's links to the worktrees in {}: {e}",
            worktrees.display()
        );
    }
}

/// Removes the links the move left at `old` once every running runner of
/// the shepherd at `shep_home` started after them
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
        flock::runners_younger_than(&client, since).await
    });
    match restarted {
        Ok(true) => migrate::sweep(old, kelpie_home)
            .iter()
            .for_each(|line| println!("{line}")),
        Ok(false) => {}
        Err(e) => eprintln!("cannot tell whether every runner is on this build: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::git;

    #[test]
    fn a_worktree_git_lost_track_of_is_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, worktrees) = (
            dir.path().join("repo"),
            dir.path().join("kelpie/koji/worktrees"),
        );
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]);
        let was = dir.path().join("wt/7");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "kelpie/7",
                was.to_str().unwrap(),
            ],
        );
        std::fs::create_dir_all(&worktrees).unwrap();
        std::fs::rename(&was, worktrees.join("7")).unwrap();
        assert!(git(&repo, &["worktree", "list", "--porcelain"]).contains("prunable"));

        repair(&repo, &worktrees);

        let listed = git(&repo, &["worktree", "list", "--porcelain"]);
        assert!(listed.contains("koji/worktrees/7"), "{listed}");
        assert!(!listed.contains("prunable"), "{listed}");
    }
}
