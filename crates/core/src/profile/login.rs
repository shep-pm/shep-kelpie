//! The login a worker holds on a repo kelpie's App covers
//!
//! Its token is in a `gh` config folder of its own in the build folder, in the
//! `hosts.yml` that `gh` reads and that git's credential helper, `gh auth
//! git-credential`, answers from. No variable carries it: Codex drops any
//! whose name holds `KEY`, `SECRET` or `TOKEN` from the commands it runs, and
//! a variable shows in every process's environment. Git's own helpers are
//! emptied, which drops the keychain's, and github.com's is `gh`'s, so
//! nothing the maintainer set up is asked.

use std::io;
use std::path::{Path, PathBuf};

use crate::ports::InstallationToken;
use crate::worker_folder;

// The account the token is kept under, and the user git sends with it. `gh`
// reads the older shape, a token directly under the host, only after asking
// GitHub whose it is to migrate it, which a worker's fence may not allow.
const USER: &str = "x-access-token";

/// The folder `gh` reads its config from, in `build`
pub fn config_dir(build: &Path) -> PathBuf {
    build.join(CONFIG_DIR)
}

// Its name in the build folder
const CONFIG_DIR: &str = "gh";

/// Writes `token` where the worker's `gh` finds it, in `build`, readable by
/// the user alone and replacing the last call's
///
/// # Errors
///
/// The error of making the folder or writing the file. Kelpie follows no link
/// in the build folder, which the worker can write, so a `gh` folder or file
/// that is a link is an error, and what it points to is left alone.
pub fn write(build: &Path, token: &InstallationToken) -> io::Result<()> {
    let text = format!(
        "github.com:\n    git_protocol: https\n    user: {USER}\n    oauth_token: {token}\n    \
         users:\n        {USER}:\n            oauth_token: {token}\n",
        token = token.expose()
    );
    // The version says the config is already in the shape `gh` migrates to.
    // Each is for the user alone, whatever mode a file already there had, and
    // renamed into place so a `gh` reading it never finds half of it: with no
    // token in the file it would look in the keychain.
    let dir = Path::new(CONFIG_DIR);
    worker_folder::write_private(build, &dir.join("config.yml"), "version: \"1\"\n")?;
    worker_folder::write_private(build, &dir.join("hosts.yml"), &text)?;
    Ok(())
}

/// What a covered worker's `GIT_CONFIG_PARAMETERS` carries to point git at that
/// login, after the settings every worker's git gets
///
/// `GIT_CONFIG_PARAMETERS` is what `git -c` sets for the commands it runs. Git
/// reads it after every config file, so the helper list is emptied after
/// whatever the maintainer's own files named, and github.com's is the only one
/// left. None of the names holds `KEY`, `SECRET` or `TOKEN`.
pub(super) const GIT_CONFIG: &str = "'credential.helper'='' \
     'credential.https://github.com.helper'='!gh auth git-credential'";

/// The variables that point `gh` at the login in `build`'s `gh` folder, and
/// keep git from prompting
pub(super) fn env(build: &Path) -> Vec<(String, String)> {
    let config = (
        "GH_CONFIG_DIR".to_owned(),
        config_dir(build).to_string_lossy().into_owned(),
    );
    std::iter::once(config).chain(no_prompt()).collect()
}

/// The variables that keep `gh` and git from prompting, for a login that
/// needs no `gh` folder
pub(super) fn no_prompt() -> [(String, String); 2] {
    let pair = |name: &str, value: &str| (name.to_owned(), value.to_owned());
    [
        pair("GH_PROMPT_DISABLED", "1"),
        pair("GIT_TERMINAL_PROMPT", "0"),
    ]
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::{Command, Stdio};

    use super::*;

    fn token(text: &str) -> InstallationToken {
        InstallationToken::new(text.to_owned())
    }

    #[cfg(unix)]
    #[test]
    fn the_token_is_written_for_the_user_alone_and_replaced_by_the_next_calls() {
        let build = tempfile::tempdir().unwrap();

        write(build.path(), &token("ghs_first")).unwrap();
        write(build.path(), &token("ghs_second")).unwrap();

        let file = config_dir(build.path()).join("hosts.yml");
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("oauth_token: ghs_second"), "{text}");
        assert!(!text.contains("ghs_first"), "{text}");
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // A folder an earlier kelpie, or the worker, left with looser files.
    #[cfg(unix)]
    #[test]
    fn both_files_are_for_the_user_alone_even_where_older_ones_were_not() {
        let build = tempfile::tempdir().unwrap();
        let dir = config_dir(build.path());
        fs::create_dir_all(&dir).unwrap();
        for name in ["config.yml", "hosts.yml"] {
            fs::write(dir.join(name), "old\n").unwrap();
            fs::set_permissions(dir.join(name), fs::Permissions::from_mode(0o644)).unwrap();
        }

        write(build.path(), &token("ghs_scoped")).unwrap();

        for name in ["config.yml", "hosts.yml"] {
            let mode = fs::metadata(dir.join(name)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
    }

    // The worker can write its build folder, and kelpie writes as the maintainer.
    #[cfg(unix)]
    #[test]
    fn a_link_where_the_file_is_staged_leaves_what_it_points_to_alone() {
        let (build, theirs) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (dir, hosts) = (config_dir(build.path()), theirs.path().join("hosts.yml"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(&hosts, "their login\n").unwrap();
        symlink(&hosts, dir.join("hosts.yml.tmp")).unwrap();

        write(build.path(), &token("ghs_scoped")).unwrap();

        assert_eq!(fs::read_to_string(&hosts).unwrap(), "their login\n");
        let text = fs::read_to_string(dir.join("hosts.yml")).unwrap();
        assert!(text.contains("oauth_token: ghs_scoped"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_for_the_folder_is_refused_and_the_maintainers_own_login_is_not_replaced() {
        let (build, theirs) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(theirs.path().join("hosts.yml"), "their login\n").unwrap();
        symlink(theirs.path(), config_dir(build.path())).unwrap();

        let refused = write(build.path(), &token("ghs_scoped"));

        assert!(refused.is_err());
        assert!(!refused.unwrap_err().to_string().contains("ghs_scoped"));
        let theirs: Vec<_> = fs::read_dir(theirs.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(theirs, ["hosts.yml"]);
        let kept = fs::read_to_string(config_dir(build.path()).join("hosts.yml")).unwrap();
        assert_eq!(kept, "their login\n");
    }

    // Measured with git and gh themselves: a helper the maintainer set up, the
    // keychain's among them, is not asked, and github.com is answered with the
    // token from the folder the worker's `gh` reads, from nothing in a variable.
    #[test]
    fn git_pushes_with_the_token_and_asks_none_of_the_helpers_the_maintainer_set_up() {
        let (build, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(build.path(), &token("ghs_scoped")).unwrap();
        let global = home.path().join("gitconfig");
        fs::write(
            &global,
            "[credential]\n\thelper = !f() { echo username=maintainer; echo password=their-login; }; f\n",
        )
        .unwrap();
        let mut git = Command::new("git");
        git.args(["credential", "fill"])
            .env("GIT_CONFIG_GLOBAL", &global)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(env(build.path()))
            .env("GIT_CONFIG_PARAMETERS", GIT_CONFIG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut git = git.spawn().unwrap();
        git.stdin
            .take()
            .unwrap()
            .write_all(b"protocol=https\nhost=github.com\n\n")
            .unwrap();
        let out = git.wait_with_output().unwrap();
        let answer = String::from_utf8_lossy(&out.stdout);

        assert!(
            answer.contains("password=ghs_scoped\n"),
            "{answer} {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!answer.contains("their-login"), "{answer}");
    }
}
