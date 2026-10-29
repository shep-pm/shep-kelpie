//! A `packed-refs.lock` a worker left behind
//!
//! The fence lets a worker's git make `packed-refs.lock`, since a commit locks
//! `packed-refs` on its own before it finds nothing to change in it. Git
//! removes the lock when it is done, and holds it for milliseconds. A worker
//! killed in that moment leaves it, and every git that deletes a ref in the
//! repo, kelpie's own included, then waits out its timeout and fails. Kelpie
//! takes a lock that old away before it makes a worker's worktree ready, from
//! the checkout's git dir and from the worktree's clone.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

// Far longer than any git holds it: kelpie's own git waits 5 seconds for it.
const STALE_AFTER: Duration = Duration::from_secs(60);

/// Removes `packed-refs.lock` from `git_dir` when it is older than a minute at `now`
///
/// A lock that is a link, a folder or newer is left alone, and so is one that
/// cannot be removed: git reports that when it needs the lock.
pub(super) fn clear(git_dir: &Path, now: SystemTime) {
    let lock = git_dir.join("packed-refs.lock");
    let Ok(meta) = fs::symlink_metadata(&lock) else {
        return;
    };
    let old = meta
        .modified()
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age > STALE_AFTER);
    if meta.is_file() && old {
        let _ = fs::remove_file(&lock);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use super::*;

    fn lock_made_at(dir: &Path, made: SystemTime) {
        let file = fs::File::create(dir.join("packed-refs.lock")).unwrap();
        file.set_modified(made).unwrap();
    }

    #[test]
    fn a_lock_a_killed_git_left_is_removed_and_one_in_use_is_not() {
        let (stale, held) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let now = SystemTime::now();
        lock_made_at(stale.path(), now - Duration::from_secs(61));
        lock_made_at(held.path(), now - Duration::from_secs(5));

        clear(stale.path(), now);
        clear(held.path(), now);

        assert!(!stale.path().join("packed-refs.lock").exists());
        assert!(held.path().join("packed-refs.lock").exists());
    }

    #[test]
    fn no_lock_is_nothing_to_do() {
        clear(tempfile::tempdir().unwrap().path(), SystemTime::now());
    }

    // The worker writes the git dir's lock name, and kelpie writes as the maintainer.
    #[cfg(unix)]
    #[test]
    fn a_link_in_place_of_the_lock_leaves_what_it_points_to_alone() {
        let (git_dir, theirs) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let target = theirs.path().join("file");
        fs::write(&target, "theirs\n").unwrap();
        symlink(&target, git_dir.path().join("packed-refs.lock")).unwrap();

        clear(
            git_dir.path(),
            SystemTime::now() + Duration::from_secs(3600),
        );

        assert_eq!(fs::read_to_string(&target).unwrap(), "theirs\n");
    }
}
