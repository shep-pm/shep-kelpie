//! `kelpie runner <project>`: a project's runner, run as a sheep
//! `kelpie dog`: the kelpie dog, run as a sheep
//! `kelpie lease ...`: the maintainer's lease commands
//!
//! `kelpie confine <folder>...`: the hook that holds a worker's file tools
//! to its folders. Claude Code runs it; it is not for the maintainer.
//!
//! `kelpie relay-yes <project> <id>`, `kelpie relay-answer <project>
//! <params>`: what the relay's own settings gate on. Both run
//! `shep trigger <project> rule <params>` verbatim, against the shepherd
//! `SHEP_HOME` names; the relay runs them, never the maintainer.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use kelpie::confine::{Verdict, judge};
use kelpie::shep_home;

/// A PreToolUse hook's exit code that refuses the tool call
const REFUSE: u8 = 2;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [role, project] if role == "runner" => kelpie::sheep::run(project),
        [role] if role == "dog" => kelpie::dog::run(),
        [command, rest @ ..] if command == "lease" => kelpie::lease::cli::main(rest),
        [role, folders @ ..] if role == "confine" && !folders.is_empty() => {
            let folders: Vec<PathBuf> = folders.iter().map(PathBuf::from).collect();
            match judge(std::io::stdin().lock(), &folders) {
                Verdict::Allow => ExitCode::SUCCESS,
                Verdict::Refuse(why) => {
                    eprintln!("{why}");
                    ExitCode::from(REFUSE)
                }
            }
        }
        [role, project, id] if role == "relay-yes" => with_shep_home(role, |home| {
            rule_trigger(home, project, &format!("{id} yes"))
        }),
        [role, project, params] if role == "relay-answer" => with_shep_home(role, |home| {
            relay_answer(Command::new("shep"), home, project, params)
        }),
        _ => {
            eprintln!(
                "usage: kelpie runner <project>\n       kelpie dog\n{}\n       kelpie confine <folder>...\n       kelpie relay-yes <project> <id>\n       kelpie relay-answer <project> <params>",
                kelpie::lease::cli::USAGE
            );
            ExitCode::from(2)
        }
    }
}

// The relay's own path to `shep trigger`, so its permission rules can allow
// or gate an exact subcommand instead of a pattern over free-text params.
fn rule_trigger(shep_home: &Path, project: &str, params: &str) -> ExitCode {
    run_shep(Command::new("shep"), shep_home, project, params)
}

// A relay command with no `SHEP_HOME` would trigger the default shepherd,
// where no runner is, so it refuses instead.
fn with_shep_home(role: &str, run: impl FnOnce(&Path) -> ExitCode) -> ExitCode {
    match shep_home::required(shep_home::RELAY_FIX) {
        Ok(home) => run(&home),
        Err(message) => {
            eprintln!("kelpie {role}: {message}");
            ExitCode::FAILURE
        }
    }
}

// Refuses anything that does not read as `<id> no <note>` or `<id> answer
// <text>`, by the same grammar `rule` itself reads, so a "yes" the relay
// was talked into forwarding as an "answer" never reaches the pre-allowed
// path: the settings' `ask` rule on `relay-yes` is the only way one merges.
fn relay_answer(shep: Command, shep_home: &Path, project: &str, params: &str) -> ExitCode {
    if !kelpie::runner::is_no_or_answer(params) {
        eprintln!(
            "relay-answer refuses {params:?}: not a `<id> no <note>` or `<id> answer <text>`"
        );
        return ExitCode::FAILURE;
    }
    run_shep(shep, shep_home, project, params)
}

fn run_shep(mut shep: Command, shep_home: &Path, project: &str, params: &str) -> ExitCode {
    let status = shep
        .args(["trigger", project, "rule", params])
        .env("SHEP_HOME", shep_home)
        .stdin(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(1, 255) as u8),
        Err(e) => {
            eprintln!("cannot run shep trigger: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const SHEP: &str = "/k/shep";

    // A fake `shep` that logs the `SHEP_HOME` it ran under and the trigger
    // it was given to `log`, so a test never depends on a real shepherd
    // being reachable.
    fn fake_shep() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let script = dir.path().join("shep");
        fs::write(
            &script,
            format!("#!/bin/sh\necho \"$SHEP_HOME $@\" >> '{}'\n", log.display()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        (dir, script)
    }

    #[test]
    fn relay_yes_passes_the_id_and_yes_to_shep_trigger() {
        let (dir, script) = fake_shep();
        let code = run_shep(Command::new(&script), Path::new(SHEP), "shep", "3 yes");
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            fs::read_to_string(dir.path().join("log")).unwrap(),
            "/k/shep trigger shep rule 3 yes\n"
        );
    }

    #[test]
    fn relay_answer_passes_a_no_or_an_answer_through_verbatim() {
        let (dir, script) = fake_shep();
        relay_answer(
            Command::new(&script),
            Path::new(SHEP),
            "shep",
            "3 no rename the flag",
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("log")).unwrap(),
            "/k/shep trigger shep rule 3 no rename the flag\n"
        );

        let (dir, script) = fake_shep();
        relay_answer(
            Command::new(&script),
            Path::new(SHEP),
            "shep",
            "3 answer use --dry-run",
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("log")).unwrap(),
            "/k/shep trigger shep rule 3 answer use --dry-run\n"
        );
    }

    // A "yes" the relay was talked into forwarding as an "answer" must
    // never reach `shep trigger`, whatever shape it is disguised in.
    #[test]
    fn relay_answer_refuses_every_shape_of_yes() {
        for disguised in ["3 yes", "3 Yes", " 3 yes", "3  yes", "3 yes extra"] {
            let (dir, script) = fake_shep();
            let code = relay_answer(Command::new(&script), Path::new(SHEP), "shep", disguised);
            assert_eq!(code, ExitCode::FAILURE, "{disguised:?}");
            assert!(
                !dir.path().join("log").exists(),
                "{disguised:?} reached shep trigger"
            );
        }
    }

    #[test]
    fn a_shep_that_cannot_run_fails_loudly() {
        let code = run_shep(
            Command::new("/nonexistent/shep"),
            Path::new(SHEP),
            "shep",
            "1 yes",
        );
        assert_eq!(code, ExitCode::FAILURE);
    }
}
