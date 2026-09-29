//! `kelpie runner <project>`: a project's runner, run as a sheep
//! `kelpie dog`: the kelpie dog, run as a sheep
//! `kelpie lease ...`: the maintainer's lease commands
//!
//! `kelpie confine <folder>...`: the hook that holds a worker's file tools
//! to its folders. Claude Code runs it; it is not for the maintainer.
//!
//! `kelpie relay-yes <project> <id>`, `kelpie relay-answer <project>
//! <params>`: what the relay's own settings gate on. Both send
//! `relay-rule <params>` to the project's runner on the shepherd `SHEP_HOME`
//! names; the relay runs them, never the maintainer.
//!
//! `kelpie relay-gate <kelpie>`: the hook that refuses the relay every
//! other tool call. Claude Code runs it, like `confine`.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kelpie::confine::{Verdict, judge};
use kelpie::relay::gate;
use kelpie::relay::rule::{self, Ruling};
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
            hook(judge(std::io::stdin().lock(), &folders))
        }
        [role, kelpie_path] if role == "relay-gate" => {
            hook(gate::judge(std::io::stdin().lock(), kelpie_path))
        }
        [role, project, id] if role == "relay-yes" => {
            with_shep_home(role, |home| rule::send(home, project, Ruling::Yes(id)))
        }
        [role, project, params] if role == "relay-answer" => with_shep_home(role, |home| {
            rule::send(home, project, Ruling::NoOrAnswer(params))
        }),
        _ => {
            eprintln!(
                "usage: kelpie runner <project>\n       kelpie dog\n{}\n       kelpie confine <folder>...\n       kelpie relay-yes <project> <id>\n       kelpie relay-answer <project> <params>\n       kelpie relay-gate <kelpie>",
                kelpie::lease::cli::USAGE
            );
            ExitCode::from(2)
        }
    }
}

// A PreToolUse hook's answer: a refusal's reason goes to Claude on stderr.
fn hook(verdict: Verdict) -> ExitCode {
    match verdict {
        Verdict::Allow => ExitCode::SUCCESS,
        Verdict::Refuse(why) => {
            eprintln!("{why}");
            ExitCode::from(REFUSE)
        }
    }
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
