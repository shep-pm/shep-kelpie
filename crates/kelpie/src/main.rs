//! `kelpie runner <project>`: a project's runner, run as a sheep
//! `kelpie dog`: the kelpie dog, run as a sheep
//! `kelpie lease ...`: the maintainer's lease commands
//!
//! `kelpie confine <folder>...`: the hook that holds a worker's file tools
//! to its folders. Claude Code runs it; it is not for the maintainer.
//!
//! `kelpie tools install`: installs the tools kelpie shows a work item's UI
//! with, under kelpie's home.
//!
//! `kelpie totp [--rotate]`: prints the authenticator secret that answers a
//! ruling from ntfy, as a URI and a QR code to scan, drawing it the first
//! time, or afresh with `--rotate`. `kelpie totp --unlock` turns answers from
//! ntfy back on after too many wrong codes.
//!
//! `kelpie shots-mcp <tools> <job>`: a worker's shots tool, an MCP server
//! Claude Code starts from the worker's MCP config.
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

use kelpie::adapters::ShotsCli;
use kelpie::confine::{Verdict, judge};
use kelpie::preview::Tools;
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
        [role, domains @ ..] if role == "browse-guard" => {
            hook(kelpie::browse::judge(std::io::stdin().lock(), domains))
        }
        [role, folders @ ..] if role == "confine" && !folders.is_empty() => {
            let folders: Vec<PathBuf> = folders.iter().map(PathBuf::from).collect();
            hook(judge(std::io::stdin().lock(), &folders))
        }
        [role, kelpie_path] if role == "relay-gate" => {
            hook(gate::judge(std::io::stdin().lock(), kelpie_path))
        }
        [command, sub] if command == "tools" && sub == "install" => install_tools(),
        [command] if command == "totp" => totp(false),
        [command, flag] if command == "totp" && flag == "--rotate" => totp(true),
        [command, flag] if command == "totp" && flag == "--unlock" => unlock(),
        [role, tools, job] if role == "shots-mcp" => {
            let shots = ShotsCli::new(Tools::at(PathBuf::from(tools)));
            stop_on_signal(shots.clone());
            let (stdin, stdout) = (std::io::stdin().lock(), std::io::stdout().lock());
            match kelpie::shots::mcp::serve(Path::new(job), &shots, stdin, stdout) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        [role, project, id] if role == "relay-yes" => {
            with_shep_home(role, |home| rule::send(home, project, Ruling::Yes(id)))
        }
        [role, project, params] if role == "relay-answer" => with_shep_home(role, |home| {
            rule::send(home, project, Ruling::NoOrAnswer(params))
        }),
        _ => {
            eprintln!(
                "usage: kelpie runner <project>\n       kelpie dog\n{}\n       kelpie confine <folder>...\n       kelpie browse-guard <domain>...\n       kelpie tools install\n       kelpie totp [--rotate | --unlock]\n       kelpie shots-mcp <tools> <job>\n       kelpie relay-yes <project> <id>\n       kelpie relay-answer <project> <params>\n       kelpie relay-gate <kelpie>",
                kelpie::lease::cli::USAGE
            );
            ExitCode::from(2)
        }
    }
}

// A signal ends the shots tool's run, dev server included, then the tool: its
// server runs in its own process group, which a signal to this one misses.
fn stop_on_signal(shots: ShotsCli) {
    std::thread::spawn(move || {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
        else {
            return;
        };
        runtime.block_on(async {
            let kinds = [
                SignalKind::terminate(),
                SignalKind::interrupt(),
                SignalKind::hangup(),
            ];
            let (Ok(mut term), Ok(mut int), Ok(mut hup)) =
                (signal(kinds[0]), signal(kinds[1]), signal(kinds[2]))
            else {
                return;
            };
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
                _ = hup.recv() => {}
            }
            shots.stop();
            std::process::exit(143);
        });
    });
}

// Kelpie's home is `KELPIE_HOME`, or `~/.kelpie`, as the runner reads it.
fn kelpie_home() -> Option<PathBuf> {
    std::env::var_os("KELPIE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".kelpie")))
}

fn totp(rotate: bool) -> ExitCode {
    let Some(home) = kelpie_home() else {
        eprintln!("HOME is not set");
        return ExitCode::FAILURE;
    };
    match kelpie::totp::show(&home.join("totp/secret"), rotate) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn unlock() -> ExitCode {
    let Some(home) = kelpie_home() else {
        eprintln!("HOME is not set");
        return ExitCode::FAILURE;
    };
    match kelpie::totp::answers::Answers::in_folder(home.join("totp")).unlock() {
        Ok(()) => {
            println!("answers from ntfy are on again");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cannot turn answers from ntfy back on: {e}");
            ExitCode::FAILURE
        }
    }
}

fn install_tools() -> ExitCode {
    let Some(home) = kelpie_home() else {
        eprintln!("HOME is not set");
        return ExitCode::FAILURE;
    };
    let tools = Tools::under(&home);
    match tools.install() {
        Ok(()) => {
            println!("installed kelpie's tools in {}", tools.dir().display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
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
