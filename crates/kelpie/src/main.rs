//! `kelpie runner <project>`: a project's runner, run as a sheep
//! `kelpie dog`: the kelpie dog, run as a sheep
//! `kelpie lease ...`: the maintainer's lease commands
//!
//! `kelpie confine <folder>...`: the hook that holds a worker's file tools
//! to its folders. Claude Code runs it; it is not for the maintainer.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use kelpie::confine::{Verdict, judge};

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
        _ => {
            eprintln!(
                "usage: kelpie runner <project>\n       kelpie dog\n{}\n       kelpie confine <folder>...",
                kelpie::lease::cli::USAGE
            );
            ExitCode::from(2)
        }
    }
}
