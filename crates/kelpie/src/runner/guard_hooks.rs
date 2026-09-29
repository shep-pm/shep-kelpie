//! A project's own guard hooks, checked when the runner starts
//!
//! A hook command that is not there fails every tool call it matches, in
//! every worker, so the runner refuses to start on one and names it. A
//! command resolves when its program is on `PATH` or at its path, and every
//! later word that is an absolute or `~/` path exists. `~/` and `$HOME/` are
//! the home folder, as the worker's shell reads them: the runner's own `HOME`
//! and `PATH` are what a worker inherits.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::settings::{Settings, SettingsError};

/// Checks that every one of the project's guard hooks resolves, with `home`
/// and `path` the worker's `HOME` and `PATH`
///
/// # Errors
///
/// [`SettingsError::Invalid`] naming `worker.guard_hooks`, the command, and
/// the word in it that does not resolve.
pub(super) fn check(
    settings: &Settings,
    home: Option<&Path>,
    path: Option<&OsStr>,
) -> Result<(), SettingsError> {
    for hook in &settings.worker.guard_hooks {
        let command = hook.command.as_str();
        if let Some(word) = unresolved(command, home, path) {
            return Err(SettingsError::Invalid {
                setting: "worker.guard_hooks",
                reason: format!("`{command}` does not resolve: {word} is not there"),
            });
        }
    }
    Ok(())
}

// The first word of `command` that does not resolve.
fn unresolved(command: &str, home: Option<&Path>, path: Option<&OsStr>) -> Option<String> {
    let mut words = command
        .split_whitespace()
        .map(|w| w.trim_matches(['\'', '"']))
        .skip_while(|w| w.contains('=') && !w.starts_with(['/', '~', '$']));
    let Some(program) = words.next() else {
        return Some("its program".to_owned());
    };
    let found = match expand(program, home) {
        Some(file) => file.is_file(),
        None if program.contains('/') || program.starts_with('~') => false,
        None => path
            .is_some_and(|dirs| std::env::split_paths(dirs).any(|dir| dir.join(program).is_file())),
    };
    if !found {
        return Some(program.to_owned());
    }
    words
        .find(|w| expand(w, home).is_some_and(|p| !p.exists()))
        .map(str::to_owned)
}

// `word` as a path, when it is one: absolute, or under the home folder.
// With no home, a `~/` word names nothing, and a folder that is not there.
fn expand(word: &str, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(rest) = word
        .strip_prefix("~/")
        .or_else(|| word.strip_prefix("$HOME/"))
    {
        return Some(home.map_or_else(|| PathBuf::from("/nonexistent"), |h| h.join(rest)));
    }
    word.starts_with('/').then(|| PathBuf::from(word))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::test::Rig;

    fn with_hook(rig: &Rig, command: &str) {
        let hook = format!(
            "[[app.dogs.kelpie.worker.guard_hooks]]\nevent = \"PreToolUse\"\nmatcher = \"Bash\"\ncommand = \"{command}\"\n"
        );
        rig.edit_settings(|s| format!("{s}\n{hook}"));
    }

    // As the runner checks it, with the rig's home as the worker's `HOME`.
    fn checked(rig: &Rig) -> Result<(), String> {
        let path = std::env::var_os("PATH");
        check(&rig.settings(), Some(rig.home.path()), path.as_deref()).map_err(|e| e.to_string())
    }

    #[test]
    fn a_project_with_no_guard_hooks_opens() {
        let rig = Rig::new("koji");
        assert!(rig.open().is_ok());
    }

    #[test]
    fn a_hook_that_does_not_resolve_stops_the_runner_at_start() {
        let rig = Rig::new("koji");
        with_hook(&rig, "kelpie-no-such-guard --strict");
        let err = rig.open().map(drop).unwrap_err().to_string();
        assert_eq!(
            err,
            "setting `worker.guard_hooks`: `kelpie-no-such-guard --strict` does not resolve: \
             kelpie-no-such-guard is not there"
        );
    }

    #[test]
    fn a_hook_whose_program_is_not_there_is_named() {
        for (command, word) in [
            ("kelpie-no-such-guard --strict", "kelpie-no-such-guard"),
            ("/nowhere/guard", "/nowhere/guard"),
            ("~/.claude/hooks/gone.sh", "~/.claude/hooks/gone.sh"),
            ("GUARD_MODE=strict", "its program"),
        ] {
            let rig = Rig::new("koji");
            with_hook(&rig, command);
            assert_eq!(
                checked(&rig),
                Err(format!(
                    "setting `worker.guard_hooks`: `{command}` does not resolve: {word} is not there"
                ))
            );
        }
    }

    #[test]
    fn a_hook_whose_script_is_not_there_is_named() {
        let rig = Rig::new("koji");
        with_hook(&rig, "sh ~/.claude/hooks/git-gh-guard.js");
        let err = checked(&rig).unwrap_err();
        assert!(
            err.contains("~/.claude/hooks/git-gh-guard.js is not there"),
            "{err}"
        );
    }

    #[test]
    fn a_hook_on_path_or_in_the_home_folder_resolves() {
        for command in [
            "sh ~/hooks/guard.sh",
            "sh $HOME/hooks/guard.sh --flag",
            "~/hooks/guard.sh",
            "/bin/sh -c true",
            "GUARD_MODE=strict sh ~/hooks/guard.sh",
        ] {
            let rig = Rig::new("koji");
            let hooks = rig.home.path().join("hooks");
            fs::create_dir(&hooks).unwrap();
            fs::write(hooks.join("guard.sh"), "exit 0\n").unwrap();
            with_hook(&rig, command);
            assert_eq!(checked(&rig), Ok(()), "{command}");
        }
    }
}
