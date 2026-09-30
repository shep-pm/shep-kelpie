//! What a command runs once the programs wrapped around it are taken off
//!
//! `env`, `sudo`, `timeout` and the like run the command after their own
//! options, so the guard judges that command. A shell's commands are read
//! from its `-c` script, a `<<<` string or a heredoc, and a shell reading
//! them from a pipe or a file on stdin is refused. So are other ways of
//! running a command the guard cannot read: `eval`, `xargs` running git or
//! gh, a shell other than the four it reads, `env -S` and `script -c`.

mod stdin;
mod vars;

pub(super) use stdin::{Shell, shell};
use stdin::{is_c_flag, redirection, stdin_path};
use vars::assignment;
pub(super) use vars::{commands_assigned, configures_git, redirects_git, sets};

/// The shells whose `-c` script, `<<<` string or heredoc the guard reads as commands
pub(super) const SHELLS: [&str; 4] = ["sh", "bash", "zsh", "dash"];

/// Shells the guard does not read, whose scripts are refused
pub(super) const OTHER_SHELLS: [&str; 12] = [
    "ksh", "mksh", "pdksh", "oksh", "yash", "fish", "csh", "tcsh", "ash", "posh", "rc", "elvish",
];

/// How surely a command's words run the program the guard found in them
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reach {
    /// It is the command's program, or runs behind a wrapper the guard knows
    Runs,
    /// It is a word of a program the guard does not know, which may only name it
    Named,
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
    /// Whether an assignment gives git config from the command's text
    pub git_configured: bool,
}

/// `words` with their wrappers taken off; `Ok(None)` when they run nothing
///
/// # Errors
///
/// The refusal, for a command the guard cannot read.
pub(super) fn unwrap(mut words: &[String]) -> Result<Option<Unwrapped<'_>>, String> {
    let mut moved = false;
    let mut git_redirected = false;
    let mut git_configured = false;
    loop {
        let Some(first) = words.first() else {
            return Ok(None);
        };
        if let Some((name, _)) = assignment(first) {
            git_redirected |= redirects_git(name);
            git_configured |= configures_git(name);
            words = &words[1..];
            continue;
        }
        let rest = &words[1..];
        words = match program(first) {
            "env" => {
                let (skip, chdir) = env_options(rest)?;
                moved |= chdir;
                for (name, _) in rest[..skip].iter().filter_map(|w| assignment(w)) {
                    git_redirected |= redirects_git(name);
                    git_configured |= configures_git(name);
                }
                &rest[skip..]
            }
            "timeout" => {
                let skip = options(rest, &["-s", "-k", "--signal", "--kill-after"]);
                // Then the duration.
                rest.get(skip + 1..).unwrap_or_default()
            }
            "nice" => &rest[options(rest, &["-n", "--adjustment"])..],
            "sudo" => {
                let skip = options(rest, &SUDO_VALUED);
                let short = |w: &String| w.starts_with('-') && !w.starts_with("--");
                moved |= rest[..skip]
                    .iter()
                    .any(|w| w.starts_with("--chdir") || short(w) && w.starts_with("-D"));
                // `-s` and `-i` with no command start a shell on stdin.
                let shell = rest[..skip].iter().any(|w| {
                    matches!(w.as_str(), "--shell" | "--login")
                        || short(w) && !w.starts_with("-D") && w.contains(['s', 'i'])
                });
                if shell && skip == rest.len() {
                    return Err(STDIN.into());
                }
                &rest[skip..]
            }
            "doas" => {
                let skip = options(rest, &["-u", "-C", "-a"]);
                if skip == rest.len() && rest.iter().any(|w| w == "-s") {
                    return Err(STDIN.into());
                }
                &rest[skip..]
            }
            "su" => {
                return Err(
                    "kelpie cannot check commands run through `su`: run them directly.".into(),
                );
            }
            "busybox" => rest,
            "caffeinate" => &rest[options(rest, &["-t", "-w"])..],
            "stdbuf" => &rest[options(rest, &["-i", "-o", "-e"])..],
            "script" => {
                let skip = options(rest, &["-t", "-T"]);
                if rest[..skip]
                    .iter()
                    .any(|w| is_c_flag(w) || w.starts_with("--command"))
                {
                    return Err(
                        "kelpie cannot check a command `script -c` runs: run it directly.".into(),
                    );
                }
                // Then the file it records to, and with no command, a shell on stdin.
                match rest.get(skip + 1..) {
                    Some(command) if !command.is_empty() => command,
                    _ => return Err(STDIN.into()),
                }
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
            // A script file is not read, but stdin is a pipe the guard cannot see.
            "source" | "." => {
                return match rest.first() {
                    Some(w) if stdin_path(w) || redirection(w).is_some_and(|r| r.stdin) => Err(
                        "kelpie cannot check commands `source` reads from a pipe: run them \
                         directly."
                            .into(),
                    ),
                    _ => Ok(None),
                };
            }
            name if OTHER_SHELLS.contains(&name) && shell(words) != Shell::Unread => {
                return Err(other_shell(name));
            }
            _ => {
                return Ok(Some(Unwrapped {
                    words,
                    moved,
                    git_redirected,
                    git_configured,
                }));
            }
        };
    }
}

// `sudo`'s options that take the next word.
const SUDO_VALUED: [&str; 20] = [
    "-u",
    "-g",
    "-C",
    "-D",
    "-h",
    "-p",
    "-r",
    "-t",
    "-T",
    "-U",
    "--user",
    "--group",
    "--close-from",
    "--chdir",
    "--host",
    "--prompt",
    "--role",
    "--type",
    "--command-timeout",
    "--other-user",
];

/// The refusal for a shell reading commands the call's text does not hold
pub(super) const STDIN: &str = "kelpie cannot check a shell reading its commands from a pipe or \
                                a file: run them directly, or with `bash -c`.";

/// The refusal for a shell function a command defines
pub(super) const FUNCTION: &str =
    "kelpie cannot check a shell function's body when it runs: run the commands directly.";

/// The refusal for a command whose program the shell works out when it runs
pub(super) const BUILT: &str = "kelpie cannot check a command whose program the shell works \
                                out when it runs, from a variable or a substitution: write the \
                                program's name out.";

/// The refusal for a script given to a shell the guard does not read
pub(super) fn other_shell(name: &str) -> String {
    format!("kelpie cannot check a `{name}` script: run the commands directly, or with `bash -c`.")
}

/// A command's program, by name: `/usr/bin/git` is `git`
pub(super) fn program(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Whether a command word is built when the command runs, as `$G` or `$(echo git)`
pub(super) fn built(word: &str) -> bool {
    word.contains(['$', '`'])
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
            "ksh < f",
            "script -qc x f",
            "source /dev/stdin",
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
            assert!(sets(&w(line), redirects_git), "{line}");
        }
        for line in ["export GIT_PAGER=cat", "A=1", "GIT_DIR=/x git log"] {
            assert!(!sets(&w(line), redirects_git), "{line}");
        }
        let words = w("GIT_PAGER=cat git log");
        assert!(!unwrap(&words).unwrap().unwrap().git_redirected);
    }

    #[test]
    fn a_shell_runs_its_text_its_stdin_or_a_file() {
        let shell_of = |line: &str| format!("{:?}", shell(&w(line)));
        for (line, runs) in [
            ("bash -c x", "Text([\"x\"])"),
            ("sh <<< x", "Text([\"x\"])"),
            ("sh 2>/dev/null <<<x", "Text([\"x\"])"),
            ("bash", "Stdin"),
            ("bash -s a b", "Stdin"),
            ("bash < f", "Stdin"),
            ("bash -o pipefail", "Stdin"),
            ("bash -", "Stdin"),
            ("bash <<", "Stdin"),
            ("bash f.sh", "Unread"),
            ("bash f.sh < in", "Unread"),
            ("bash 2> err f.sh", "Unread"),
            ("bash --version", "Unread"),
        ] {
            assert_eq!(shell_of(line), runs, "{line}");
        }
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
