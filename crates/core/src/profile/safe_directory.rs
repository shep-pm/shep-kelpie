//! The clone's folders git is told to trust on Windows

use std::path::Path;

/// `GIT_CONFIG_PARAMETERS` entries that trust each of `dirs`
///
/// The clone is owned by `Administrators` and the worker runs as the sandbox's
/// user, so git refuses it as dubious ownership. Git reads `safe.directory`
/// only from system, global and command-line config, and compares the path
/// with `/` separators.
pub(super) fn parameters<'a>(dirs: impl IntoIterator<Item = &'a Path>) -> String {
    dirs.into_iter()
        .map(|dir| {
            let dir = dir
                .to_string_lossy()
                .replace('\\', "/")
                .replace('\'', r"'\''");
            format!("'safe.directory'='{dir}'")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use crate::ports::InstallationToken;
    use crate::profile::{WorkerProfile, housekeeping, login};
    use crate::settings::{BuildDir, EnvName};

    static NO_BUILD_ENV: BTreeMap<EnvName, BuildDir> = BTreeMap::new();

    fn profile<'a>(
        worktree: &'a Path,
        common: &'a Path,
        token: Option<&'a InstallationToken>,
    ) -> WorkerProfile<'a> {
        WorkerProfile {
            worktree,
            build: Path::new("/k/targets/shep/7"),
            git_common_dir: common,
            git_dir: Path::new(CLONE),
            branch: "kelpie/7",
            base: "main",
            kelpie_pushes: false,
            kelpie: Path::new("/opt/kelpie"),
            skills: Path::new("/k/shep/skills"),
            kelpie_home: Path::new("/k"),
            repo: Path::new("/k/repos/shep"),
            guard_hooks: &[],
            allowed_domains: &[],
            registries: &[],
            build_env: &NO_BUILD_ENV,
            home: Path::new("/Users/me"),
            shep_home: Path::new("/srv/shep"),
            door: Path::new("/k/dog/lease.sock"),
            token,
            login_token: None,
        }
    }

    fn git_config(profile: &WorkerProfile<'_>, windows: bool) -> String {
        profile.env_for(windows)["GIT_CONFIG_PARAMETERS"].clone()
    }

    const WORKTREE: &str = r"C:\k\wt\shep\7";
    // With `/` separators, so a path is inside another on any host the tests run on
    const COMMON: &str = "C:/k/repos/shep/.git";
    const CLONE: &str = "C:/k/wt/shep/7.git";

    #[test]
    fn on_windows_git_is_told_to_trust_the_checkouts_git_dir_the_worktree_and_the_clones_git_dir() {
        let profile = profile(Path::new(WORKTREE), Path::new(COMMON), None);

        assert_eq!(
            git_config(&profile, true),
            format!(
                "{} 'safe.directory'='C:/k/repos/shep/.git' 'safe.directory'='C:/k/wt/shep/7' 'safe.directory'='C:/k/wt/shep/7.git'",
                housekeeping::PARAMETERS
            )
        );
    }

    #[test]
    fn on_windows_a_covered_worker_trusts_them_beside_its_login_settings() {
        let token = InstallationToken::new("ghs_scoped".to_owned());
        let profile = profile(Path::new(WORKTREE), Path::new(COMMON), Some(&token));

        assert_eq!(
            git_config(&profile, true),
            format!(
                "{} 'safe.directory'='C:/k/repos/shep/.git' 'safe.directory'='C:/k/wt/shep/7' 'safe.directory'='C:/k/wt/shep/7.git' {}",
                housekeeping::PARAMETERS,
                login::GIT_CONFIG
            )
        );
    }

    #[test]
    fn a_quote_in_a_path_is_escaped_as_git_reads_it() {
        let profile = profile(Path::new(r"C:\k\o'neil\7"), Path::new(COMMON), None);

        assert!(
            git_config(&profile, true).contains(r#"'safe.directory'='C:/k/o'\''neil/7'"#),
            "{}",
            git_config(&profile, true)
        );
    }

    #[test]
    fn off_windows_the_clone_is_the_workers_own_and_nothing_is_added() {
        let token = InstallationToken::new("ghs_scoped".to_owned());
        let plain = profile(Path::new(WORKTREE), Path::new(COMMON), None);
        let covered = profile(Path::new(WORKTREE), Path::new(COMMON), Some(&token));

        assert_eq!(git_config(&plain, false), housekeeping::PARAMETERS);
        assert_eq!(
            git_config(&covered, false),
            format!("{} {}", housekeeping::PARAMETERS, login::GIT_CONFIG)
        );
    }
}
