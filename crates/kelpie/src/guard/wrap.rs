//! What a command runs once the programs wrapped around it are taken off
//!
//! `env`, `timeout`, `nice` and the like run the command after their own
//! options, so the guard judges that command. Some ways of running a command
//! it cannot read are refused: `eval`, `xargs` running git or gh, a shell
//! other than the four it reads, and `env -S`. Others it does not know of.

/// The shells whose `-c` script, or heredoc, the guard reads as commands
pub(super) const SHELLS: [&str; 4] = ["sh", "bash", "zsh", "dash"];

// Shells the guard does not read, whose `-c` scripts are refused.
const OTHER_SHELLS: [&str; 12] = [
    "ksh", "mksh", "pdksh", "oksh", "yash", "fish", "csh", "tcsh", "ash", "busybox", "posh", "rc",
];

// `GIT_` variables that change only how git shows or signs what it does.
// Any other can point git at another repo, index or config.
const GIT_HARMLESS: [&str; 12] = [
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "GIT_TERMINAL_PROMPT",
    "GIT_MERGE_AUTOEDIT",
    "GIT_OPTIONAL_LOCKS",
];

/// Whether setting `name` can point git at another repo or config
pub(super) fn redirects_git(name: &str) -> bool {
    name.starts_with("GIT_") && !GIT_HARMLESS.contains(&name)
        || matches!(name, "HOME" | "XDG_CONFIG_HOME")
}

/// Whether `words` set, for the commands after them, a variable that
/// redirects git: an `export`, a `declare -x`, or an assignment alone,
/// which `set -a` would export
pub(super) fn sets_git_redirect(words: &[String]) -> bool {
    // A name the shell works out when it runs could be any of them.
    let names = |words: &[String]| {
        words.iter().filter(|w| !w.starts_with('-')).any(|w| {
            let name = w.split_once('=').map_or(w.as_str(), |(n, _)| n);
            !is_name(name) || redirects_git(name)
        })
    };
    match words.first().map(|w| program(w)) {
        Some("export" | "declare" | "typeset" | "local" | "readonly") => names(&words[1..]),
        _ => words.iter().all(|w| assignment(w).is_some()) && names(words),
    }
}

/// A command with its wrappers taken off
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Unwrapped<'a> {
    /// The command's own words, its program first
    pub words: &'a [String],
    /// Whether a wrapper moved it to a folder the guard cannot follow
    pub moved: bool,
    /// Whether an assignment points git at another repo
    pub git_redirected: bool,
}

/// `words` with their wrappers taken off; `Ok(None)` when they run nothing
///
/// # Errors
///
/// The refusal, for a command the guard cannot read.
pub(super) fn unwrap(mut words: &[String]) -> Result<Option<Unwrapped<'_>>, String> {
    let mut moved = false;
    let mut git_redirected = false;
    loop {
        let Some(first) = words.first() else {
            return Ok(None);
        };
        if let Some((name, _)) = assignment(first) {
            git_redirected |= redirects_git(name);
            words = &words[1..];
            continue;
        }
        let rest = &words[1..];
        words = match program(first) {
            "env" => {
                let (skip, chdir) = env_options(rest)?;
                moved |= chdir;
                git_redirected |= rest[..skip]
                    .iter()
                    .filter_map(|w| assignment(w))
                    .any(|(name, _)| redirects_git(name));
                &rest[skip..]
            }
            "timeout" => {
                let skip = options(rest, &["-s", "-k", "--signal", "--kill-after"]);
                // Then the duration.
                rest.get(skip + 1..).unwrap_or_default()
            }
            "nice" => {
                let skip = options(rest, &["-n", "--adjustment"]);
                &rest[skip..]
            }
            "exec" => &rest[options(rest, &["-a"])..],
            "command" => {
                // `command -v` and `-V` only say where a program is.
                if rest.first().is_some_and(|w| w == "-v" || w == "-V") {
                    return Ok(None);
                }
                &rest[options(rest, &[])..]
            }
            "nohup" | "setsid" | "builtin" | "time" => &rest[options(rest, &[])..],
            "xargs" => {
                let skip = options(
                    rest,
                    &[
                        "-a", "-d", "-E", "-e", "-I", "-i", "-L", "-l", "-n", "-P", "-s",
                    ],
                );
                if rest
                    .get(skip)
                    .is_some_and(|w| matches!(program(w), "git" | "gh"))
                {
                    return Err(
                        "kelpie cannot check git or gh run through `xargs`: run it directly."
                            .into(),
                    );
                }
                return Ok(None);
            }
            "eval" => {
                return Err(
                    "kelpie cannot check a command run through `eval`: run it directly.".into(),
                );
            }
            "function" => return Err(FUNCTION.into()),
            name if OTHER_SHELLS.contains(&name) && runs_script(rest) => {
                return Err(format!(
                    "kelpie cannot check a `{name}` script: run the commands directly, or with `bash -c`."
                ));
            }
            _ => {
                return Ok(Some(Unwrapped {
                    words,
                    moved,
                    git_redirected,
                }));
            }
        };
    }
}

/// The refusal for a shell function a command defines
pub(super) const FUNCTION: &str =
    "kelpie cannot check a shell function's body when it runs: run the commands directly.";

/// A command's program, by name: `/usr/bin/git` is `git`
pub(super) fn program(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// The script a shell runs with `-c`, `-lc` and the like
pub(super) fn script(words: &[String]) -> Option<&str> {
    let at = words[1..].iter().position(|w| is_c_flag(w))?;
    words.get(at + 2).map(String::as_str)
}

fn is_c_flag(word: &str) -> bool {
    word.starts_with('-') && !word.starts_with("--") && word.contains('c')
}

// A shell given a script to run, as `-c` or a heredoc on stdin.
fn runs_script(rest: &[String]) -> bool {
    rest.iter().any(|w| is_c_flag(w) || w.starts_with("<<"))
}

fn assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    is_name(name).then_some((name, value))
}

// A shell variable's name, as written.
fn is_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// How many words a wrapper's own options take; `valued` take a separate value.
fn options(rest: &[String], valued: &[&str]) -> usize {
    let mut i = 0;
    while let Some(word) = rest.get(i) {
        if word == "--" {
            return i + 1;
        }
        if !word.starts_with('-') || word == "-" {
            return i;
        }
        i += if valued.contains(&word.as_str()) {
            2
        } else {
            1
        };
    }
    i
}

// `env`'s own options, and whether one moves the command (`-C`).
fn env_options(rest: &[String]) -> Result<(usize, bool), String> {
    let mut i = 0;
    let mut chdir = false;
    while let Some(word) = rest.get(i) {
        match word.as_str() {
            "--" => return Ok((i + 1, chdir)),
            "-S" | "--split-string" => {
                return Err(
                    "kelpie cannot check a command `env -S` splits: run it directly.".into(),
                );
            }
            w if w.starts_with("-S") || w.starts_with("--split-string=") => {
                return Err(
                    "kelpie cannot check a command `env -S` splits: run it directly.".into(),
                );
            }
            "-u" | "--unset" | "-P" => i += 2,
            "-C" | "--chdir" => {
                chdir = true;
                i += 2;
            }
            w if w.starts_with("--chdir=") => {
                chdir = true;
                i += 1;
            }
            w if w.starts_with('-') => i += 1,
            w if assignment(w).is_some() => i += 1,
            _ => return Ok((i, chdir)),
        }
    }
    Ok((i, chdir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(words: &str) -> Vec<String> {
        words.split(' ').map(str::to_owned).collect()
    }

    fn runs(words: &str) -> Vec<String> {
        let words = w(words);
        unwrap(&words).unwrap().unwrap().words.to_vec()
    }

    #[test]
    fn wrappers_and_their_options_come_off() {
        for line in [
            "git push",
            "env git push",
            "env -i -u HOME A=1 git push",
            "timeout 5 git push",
            "timeout -k 1 -s KILL 5m git push",
            "nice -n 10 git push",
            "nice -5 git push",
            "nohup env timeout 3 git push",
            "exec -a x git push",
            "command git push",
            "time -p git push",
            "A=1 B=2 git push",
        ] {
            assert_eq!(runs(line), w("git push"), "{line}");
        }
    }

    #[test]
    fn what_the_guard_cannot_read_is_refused() {
        for line in [
            "eval git push",
            "xargs git",
            "xargs -n 1 gh pr create",
            "ksh -c x",
            "fish -c x",
            "env -S x",
            "env --split-string=x",
            "function f",
        ] {
            assert!(unwrap(&w(line)).is_err(), "{line}");
        }
    }

    #[test]
    fn a_git_redirect_and_a_move_are_seen() {
        let words = w("GIT_DIR=/x git push");
        assert!(unwrap(&words).unwrap().unwrap().git_redirected);
        let words = w("env -C /tmp git push");
        assert!(unwrap(&words).unwrap().unwrap().moved);
        let words = w("env -i GIT_WORK_TREE=. git push");
        assert!(unwrap(&words).unwrap().unwrap().git_redirected);
        for line in [
            "GIT_CONFIG_COUNT=1 git push",
            "GIT_CONFIG_GLOBAL=cfg git push",
            "HOME=. git push",
            "XDG_CONFIG_HOME=x git push",
        ] {
            assert!(unwrap(&w(line)).unwrap().unwrap().git_redirected, "{line}");
        }
        for line in [
            "export GIT_DIR=/x",
            "declare -x GIT_WORK_TREE=.",
            "export GIT_DIR",
            "GIT_DIR=/x",
            "export $(printf GIT_DIR=/x)",
            "export $V",
        ] {
            assert!(sets_git_redirect(&w(line)), "{line}");
        }
        for line in ["export GIT_PAGER=cat", "A=1", "GIT_DIR=/x git log"] {
            assert!(!sets_git_redirect(&w(line)), "{line}");
        }
        let words = w("GIT_PAGER=cat git log");
        assert!(!unwrap(&words).unwrap().unwrap().git_redirected);
    }

    #[test]
    fn what_runs_nothing_is_nothing() {
        for line in ["command -v git", "env", "xargs echo"] {
            assert_eq!(unwrap(&w(line)), Ok(None), "{line}");
        }
        assert_eq!(
            runs("ksh script.ksh"),
            w("ksh script.ksh"),
            "a script file is not read"
        );
    }
}
