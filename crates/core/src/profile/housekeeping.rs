//! Git's own housekeeping, which a worker's git skips
//!
//! A worker's turn is time, and git spends some of it on its own account: `gc
//! --auto` and `maintenance --auto` after enough loose objects pack the clone
//! while the worker waits, and a fetch that prunes removes tracking refs the
//! worker's merge may still name. Kelpie's clone is thrown away with its work
//! item, so none of it pays. The worker's git is told not to try.

/// What `GIT_CONFIG_PARAMETERS` carries for every worker, whether or not kelpie's App covers its repo
///
/// Git reads that variable after every config file, so these win over the
/// maintainer's and the repo's own. The sandbox runtime sets it to
/// `http.proxyAuthMethod=basic`, which this replaces, so it carries that too:
/// without it git connects to the sandbox's proxy without its login, and no
/// covered worker could push.
pub const PARAMETERS: &str =
    "'http.proxyAuthMethod'='basic' 'gc.auto'='0' 'maintenance.auto'='false' 'fetch.prune'='false'";

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use super::PARAMETERS;

    const SETTINGS: [(&str, &str); 1] = [("GIT_CONFIG_PARAMETERS", PARAMETERS)];

    fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", dir.parent().unwrap())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "w")
            .env("GIT_AUTHOR_EMAIL", "w@example.com")
            .env("GIT_COMMITTER_NAME", "w")
            .env("GIT_COMMITTER_EMAIL", "w@example.com")
            .envs(env.iter().copied())
            .output()
            .unwrap()
    }

    fn ok(dir: &Path, args: &[&str]) {
        let out = git(dir, args, &[]);
        assert!(out.status.success(), "{args:?}: {out:?}");
    }

    // A clone of a repo, as a worker has, that asks for what a repo with many
    // loose objects and a pruning habit does.
    struct Cloned {
        _root: tempfile::TempDir,
        origin: PathBuf,
        clone: PathBuf,
    }

    impl Cloned {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let (origin, clone) = (root.path().join("origin"), root.path().join("clone"));
            fs::create_dir(&origin).unwrap();
            ok(&origin, &["init", "-q", "-b", "main"]);
            ok(&origin, &["commit", "-q", "--allow-empty", "-m", "init"]);
            ok(&origin, &["branch", "topic"]);
            ok(root.path(), &["clone", "-q", "origin", "clone"]);
            ok(&clone, &["config", "fetch.prune", "true"]);
            for n in 0..600 {
                fs::write(clone.join(format!("f{n}")), n.to_string()).unwrap();
            }
            ok(&clone, &["add", "."]);
            ok(&clone, &["commit", "-q", "-m", "many files"]);
            // Loose objects enough for `gc --auto` to find work at a limit of 1.
            ok(&clone, &["config", "gc.auto", "1"]);
            ok(&clone, &["config", "gc.autoDetach", "false"]);
            Self {
                _root: root,
                origin,
                clone,
            }
        }

        // What `gc --auto` prints, which is stdout and stderr both.
        fn gc_auto(&self, env: &[(&str, &str)]) -> String {
            let out = git(&self.clone, &["gc", "--auto"], env);
            [out.stdout, out.stderr]
                .map(|text| String::from_utf8_lossy(&text).into_owned())
                .concat()
        }

        // The branch `topic` is gone from the origin, and the fetch is told
        // nothing about pruning: the clone's config does. Whether the clone
        // still holds `origin/topic` after.
        fn keeps_a_deleted_branch(&self, env: &[(&str, &str)]) -> bool {
            ok(&self.origin, &["branch", "-D", "topic"]);
            git(&self.clone, &["fetch"], env);
            git(
                &self.clone,
                &["rev-parse", "--verify", "--quiet", "origin/topic"],
                &[],
            )
            .status
            .success()
        }
    }

    #[test]
    fn reproduction_auto_gc_packs_the_clone_while_the_worker_waits() {
        let printed = Cloned::new().gc_auto(&[]);

        assert!(printed.contains("Auto packing"), "{printed}");
    }

    #[test]
    fn with_the_settings_auto_gc_prints_nothing() {
        assert_eq!(Cloned::new().gc_auto(&SETTINGS), "");
    }

    #[test]
    fn reproduction_a_fetch_prunes_a_branch_the_origin_deleted() {
        assert!(!Cloned::new().keeps_a_deleted_branch(&[]));
    }

    #[test]
    fn with_the_settings_a_fetch_keeps_a_branch_the_origin_deleted() {
        assert!(Cloned::new().keeps_a_deleted_branch(&SETTINGS));
    }
}
