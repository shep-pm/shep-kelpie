//! The variables a worker's login puts in its environment

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use crate::ports::InstallationToken;
    use crate::profile::{GH_TOKEN_VAR, WorkerProfile, login};
    use crate::settings::{BuildDir, EnvName};

    static NO_BUILD_ENV: BTreeMap<EnvName, BuildDir> = BTreeMap::new();

    fn env(
        token: Option<&InstallationToken>,
        login_token: Option<&InstallationToken>,
    ) -> BTreeMap<String, String> {
        WorkerProfile {
            worktree: Path::new("/k/wt/shep/7"),
            build: Path::new("/k/targets/shep/7"),
            git_common_dir: Path::new("/k/repos/shep/.git"),
            git_dir: Path::new("/k/repos/shep/.git/worktrees/7"),
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
            login_token,
        }
        .env_for(false)
    }

    const LOGIN_VARS: [&str; 3] = ["GH_CONFIG_DIR", "GH_PROMPT_DISABLED", "GIT_TERMINAL_PROMPT"];

    #[test]
    fn a_maintainers_login_worker_has_git_ask_gh_and_nothing_else_and_no_gh_folder() {
        let login_token = InstallationToken::new("gho_theirs".to_owned());

        let env = env(None, Some(&login_token));

        assert!(env["GIT_CONFIG_PARAMETERS"].ends_with(login::GIT_CONFIG));
        assert_eq!(env["GIT_TERMINAL_PROMPT"], "0");
        assert_eq!(env["GH_PROMPT_DISABLED"], "1");
        assert_eq!(env[GH_TOKEN_VAR], "gho_theirs");
        assert!(!env.contains_key("GH_CONFIG_DIR"), "{env:?}");
    }

    #[test]
    fn an_app_token_worker_has_its_own_gh_folder_and_the_maintainers_login_is_not_handed_over() {
        let (token, login_token) = (
            InstallationToken::new("ghs_scoped".to_owned()),
            InstallationToken::new("gho_theirs".to_owned()),
        );

        let env = env(Some(&token), Some(&login_token));

        assert!(env["GIT_CONFIG_PARAMETERS"].ends_with(login::GIT_CONFIG));
        assert_eq!(env["GH_CONFIG_DIR"], "/k/targets/shep/7/gh");
        assert_eq!(env["GIT_TERMINAL_PROMPT"], "0");
        assert_eq!(env["GH_PROMPT_DISABLED"], "1");
        assert!(!env.contains_key(GH_TOKEN_VAR), "{env:?}");
    }

    #[test]
    fn a_worker_with_no_login_has_none_of_the_login_variables() {
        let env = env(None, None);

        assert!(!env["GIT_CONFIG_PARAMETERS"].contains("credential"));
        for name in LOGIN_VARS.into_iter().chain([GH_TOKEN_VAR]) {
            assert!(!env.contains_key(name), "{name}");
        }
    }
}
