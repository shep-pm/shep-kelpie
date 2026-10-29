//! A repo's common dir, read once and kept for that repo

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use super::{WorktreeError, canonical, git};

static KNOWN: Mutex<BTreeMap<PathBuf, PathBuf>> = Mutex::new(BTreeMap::new());

/// The common git dir of `repo`, canonical, read from git the first time
/// and kept after
///
/// # Errors
///
/// [`WorktreeError`] when git cannot name the dir. A failure is not kept.
pub(super) fn of(repo: &Path) -> Result<PathBuf, WorktreeError> {
    kept(&KNOWN, repo, || {
        let common = git(
            repo,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        Ok(canonical(Path::new(&common)))
    })
}

fn kept(
    known: &Mutex<BTreeMap<PathBuf, PathBuf>>,
    repo: &Path,
    read: impl FnOnce() -> Result<PathBuf, WorktreeError>,
) -> Result<PathBuf, WorktreeError> {
    let key = canonical(repo);
    if let Some(common) = known
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return Ok(common.clone());
    }
    let common = read()?;
    known
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, common.clone());
    Ok(common)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::Path;

    use super::*;
    use crate::worktree::{WorktreeError, trusted};

    fn init(repo: &Path) {
        std::fs::create_dir_all(repo).unwrap();
        git(repo, ["init", "--quiet", "-b", "main"]).unwrap();
        git(
            repo,
            [
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "first",
            ],
        )
        .unwrap();
    }

    fn add_worktree(repo: &Path, at: &Path) {
        git(
            repo,
            [
                "worktree",
                "add",
                "--quiet",
                "--detach",
                at.to_str().unwrap(),
            ],
        )
        .unwrap();
    }

    fn worktree_in(repo: &Path, name: &str) -> PathBuf {
        init(repo);
        let at = repo.join(name);
        add_worktree(repo, &at);
        at
    }

    #[test]
    fn a_repo_is_read_once_and_a_second_repo_gets_its_own() {
        let known = Mutex::new(BTreeMap::new());
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("one"), dir.path().join("two"));
        let reads = Cell::new(0);
        let read = |at: &str| {
            reads.set(reads.get() + 1);
            Ok(PathBuf::from(at))
        };

        assert_eq!(
            kept(&known, &one, || read("one/.git")).unwrap(),
            Path::new("one/.git")
        );
        assert_eq!(
            kept(&known, &one, || read("never")).unwrap(),
            Path::new("one/.git")
        );
        assert_eq!(
            kept(&known, &two, || read("two/.git")).unwrap(),
            Path::new("two/.git")
        );
        assert_eq!(reads.get(), 2);
    }

    #[test]
    fn a_failed_read_is_read_again() {
        let known = Mutex::new(BTreeMap::new());
        let dir = tempfile::tempdir().unwrap();
        let fail = || Err(WorktreeError::Spawn("no git".into()));

        assert!(kept(&known, dir.path(), fail).is_err());
        let found = kept(&known, dir.path(), || Ok(PathBuf::from("x"))).unwrap();
        assert_eq!(found, Path::new("x"));
    }

    #[test]
    fn a_worktree_of_another_repo_is_refused_after_its_repo_was_read() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("one"), dir.path().join("two"));
        let own = worktree_in(&one, "w");
        let other = worktree_in(&two, "w");

        assert!(trusted(&one, &own).is_ok());
        assert!(trusted(&two, &other).is_ok());
        assert!(matches!(
            trusted(&one, &other),
            Err(WorktreeError::Foreign(_))
        ));
        assert!(matches!(
            trusted(&two, &own),
            Err(WorktreeError::Foreign(_))
        ));
    }

    #[test]
    fn a_commondir_rewritten_after_the_repo_was_read_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (dir.path().join("one"), dir.path().join("two"));
        let at = worktree_in(&one, "w");
        init(&two);
        assert!(trusted(&one, &at).is_ok());

        let own = one.join(".git/worktrees/w");
        std::fs::write(own.join("commondir"), two.join(".git").to_str().unwrap()).unwrap();

        assert!(matches!(trusted(&one, &at), Err(WorktreeError::Foreign(_))));
    }

    #[test]
    fn a_clone_is_trusted_by_its_own_git_dir_and_no_other_repos() {
        let dir = tempfile::tempdir().unwrap();
        let (source, clone) = (dir.path().join("source"), dir.path().join("clone"));
        init(&source);
        git(
            dir.path(),
            ["clone", "--quiet", source.to_str().unwrap(), "clone"],
        )
        .unwrap();

        let in_clone = trusted(&clone.join(".git"), &clone).unwrap();
        assert!(in_clone(&["rev-parse", "HEAD"]).is_ok());
        assert!(matches!(
            trusted(&source.join(".git"), &clone),
            Err(WorktreeError::Foreign(_))
        ));
    }
}
