//! A project's own guard hooks, checked when the runner starts
//!
//! A hook command that is not there fails every tool call it matches, in
//! every worker, so the runner refuses to start on one and names it. A
//! command resolves when its program is on `PATH` or at its path, and every
//! later word that is an absolute or `~/` path exists. `~/` and `$HOME/` are
//! the home folder, as the worker's shell reads them.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::settings::{Settings, SettingsError};

/// Checks that every one of the project's guard hooks resolves, on `path`
///
/// # Errors
///
/// [`SettingsError::Invalid`] naming `worker.guard_hooks`, the command, and
/// the word in it that does not resolve.
pub(super) fn check(
    settings: &Settings,
    home: &Path,
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
fn unresolved(command: &str, home: &Path, path: Option<&OsStr>) -> Option<String> {
    let mut words = command
        .split_whitespace()
        .map(|w| w.trim_matches(['\'', '"']))
        .skip_while(|w| w.contains('=') && !w.starts_with(['/', '~', '$']));
    let Some(program) = words.next() else {
        return Some("its program".to_owned());
    };
    let found = match expand(program, home) {
        Some(file) => file.is_file(),
        None if program.contains('/') => false,
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
fn expand(word: &str, home: &Path) -> Option<PathBuf> {
    if let Some(rest) = word
        .strip_prefix("~/")
        .or_else(|| word.strip_prefix("$HOME/"))
    {
        return Some(home.join(rest));
    }
    word.starts_with('/').then(|| PathBuf::from(word))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::test::Rig;

    fn open_with_hook(rig: &Rig, command: &str) -> Result<(), String> {
        let hook = format!(
            "[[worker.guard_hooks]]\nevent = \"PreToolUse\"\nmatcher = \"Bash\"\ncommand = \"{command}\"\n"
        );
        rig.edit_settings(|s| format!("{s}\n{hook}"));
        rig.open().map(drop).map_err(|e| e.to_string())
    }

    #[test]
    fn a_project_with_no_guard_hooks_opens() {
        let rig = Rig::new("koji");
        assert!(rig.open().is_ok());
    }

    #[test]
    fn a_hook_whose_program_is_not_there_stops_the_runner_naming_it() {
        for (command, word) in [
            ("kelpie-no-such-guard --strict", "kelpie-no-such-guard"),
            ("/nowhere/guard", "/nowhere/guard"),
            ("~/.claude/hooks/gone.sh", "~/.claude/hooks/gone.sh"),
            ("GUARD_MODE=strict", "its program"),
        ] {
            let rig = Rig::new("koji");
            let err = open_with_hook(&rig, command).unwrap_err();
            assert_eq!(
                err,
                format!(
                    "setting `worker.guard_hooks`: `{command}` does not resolve: {word} is not there"
                )
            );
        }
    }

    #[test]
    fn a_hook_whose_script_is_not_there_stops_the_runner_naming_it() {
        let rig = Rig::new("koji");
        let err = open_with_hook(&rig, "sh ~/.claude/hooks/git-gh-guard.js").unwrap_err();
        assert!(
            err.contains("~/.claude/hooks/git-gh-guard.js is not there"),
            "{err}"
        );
    }

    #[test]
    fn a_hook_on_path_or_in_the_home_folder_opens() {
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
            assert_eq!(open_with_hook(&rig, command), Ok(()), "{command}");
        }
    }
}
