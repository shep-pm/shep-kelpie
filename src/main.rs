//! `kelpie --schema` and `--version`: shep's probes, answered for lookout
//!
//! `kelpie runner <project>`: a project's runner, run as a sheep
//! `kelpie`, started by shep as the adopted dog: the lease dog
//! `shep kelpie lease ...`: the maintainer's lease commands
//!
//! `shep kelpie add`, `start`, `pause`, `status`, `rule`, `rework`, `adopt`,
//! `gate`, `drop`, `timings` and `issue`: a project in the maintainer's own
//! flock, the one `-p` names or whose repo holds the folder it runs in.
//!
//! `shep kelpie doctor [<project>] [--test-alert]`: checks what the projects need
//! on this machine, and changes nothing. Run as `shep kelpie doctor`.
//!
//! `shep kelpie upgrade --ref <git ref> | --release <version> | --binary <path>`,
//! `shep kelpie upgrade --rollback`: installs a new kelpie over the one the
//! adopted dog runs and restarts the dog and each runner onto it, between
//! merges, or puts the previous build back. Run as `shep kelpie upgrade ...`.
//!
//! `kelpie version [--json]`: the build's version and the shep version it is
//! made with, which `upgrade` reads from a build before it installs it.
//!
//! `kelpie confine <folder>...`: the hook that holds a worker's file tools
//! to its folders. Claude Code runs it; it is not for the maintainer.
//!
//! `kelpie guard <git common dir> <worktree>`: the hook on every worker's
//! Bash calls that keeps the home folder's path and freeform pull request
//! titles out of what it publishes. Claude Code runs it, like `confine`.
//! On Codex it takes `--pin-folder`, and the command it answers with runs
//! it again with `--judge-here` when Codex runs the command in another folder.
//! With `--issues=<label>`, an `--agent=<name>` per implementer and
//! `--ledger=<file>` it is the issue writer's hook instead, which runs only
//! the commands it lists. With `--record` too it is the PostToolUse hook
//! that records what the session filed and read in that ledger.
//!
//! `shep kelpie settings move <project> [<sheep>]`: moves a project's settings
//! file, and kelpie's own, into their tables on kelpie's shepherd.
//!
//! `shep kelpie tools install`: installs the sandbox runtime every agent runs
//! in, under kelpie's home.
//!
//! `shep kelpie totp [--rotate]`, run as `shep kelpie totp [--rotate]` where kelpie
//! is not on the PATH: prints the authenticator secret that answers a ruling
//! from ntfy, as a URI and a QR code to scan, drawing it the first time, or
//! afresh with `--rotate`. `shep kelpie totp --unlock` turns answers from
//! ntfy back on after too many wrong codes.

#![forbid(unsafe_code)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use shep_kelpie::confine::{Verdict, judge};
use shep_kelpie::guard::{self, Checkout};
use shep_kelpie::runner::{ProjectName, ProjectPaths};
use shep_kelpie::settings::moving;
use shep_kelpie::settings::source::Files;
use shep_kelpie::tools::Tools;
use shep_kelpie::{shep_home, shepherd};

/// A PreToolUse hook's exit code that refuses the tool call
const REFUSE: u8 = 2;

fn main() -> ExitCode {
    shep_kelpie::schema::probe();
    let args = shep_kelpie::flock::verb_first(std::env::args().skip(1).collect());
    match args.as_slice() {
        [role, project] if role == "runner" => shep_kelpie::sheep::run(project),
        [command, rest @ ..] if command == "lease" => shep_kelpie::lease::cli::main(rest),
        [role, git_common_dir, worktree, local @ ..] if role == "guard" => {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let checkout = Checkout {
                git_common_dir: Path::new(git_common_dir),
                worktree: Path::new(worktree),
            };
            let (issues, local) = guard::issue_rules(local);
            let (record, local) = guard::record_flag(&local);
            let (pin, local) = guard::pin_flag(&local);
            let (here, local) = guard::here_flag(&local);
            let paths = match guard::local_paths(home.as_deref(), &local) {
                Ok(paths) => paths,
                Err(why) => return hook(Verdict::Refuse(why)),
            };
            let mut input = Vec::new();
            if let Err(e) = std::io::stdin().lock().read_to_end(&mut input) {
                return hook(Verdict::Refuse(format!(
                    "kelpie cannot read this tool call: {e}"
                )));
            }
            if let Some(rules) = issues.as_ref().filter(|_| record) {
                guard::record_issues(&input[..], rules);
                return ExitCode::SUCCESS;
            }
            if let Some(rules) = issues {
                return hook(guard::judge_issues(
                    &input[..],
                    home.as_deref(),
                    paths,
                    &rules,
                ));
            }
            if here {
                let folder = std::env::current_dir()
                    .map_err(|e| format!("kelpie cannot tell which folder this runs in: {e}"));
                match folder.and_then(|f| guard::here(&input, &f)) {
                    Ok(moved) => input = moved,
                    Err(why) => return hook(Verdict::Refuse(why)),
                }
            }
            // The pinned command judges again by this program's full path,
            // never by a name the worker's PATH could answer.
            let again = || -> Result<Vec<String>, String> {
                let me = std::env::current_exe()
                    .ok()
                    .filter(|p| p.is_absolute())
                    .ok_or("kelpie cannot tell where its own program is")?;
                Ok([me.to_string_lossy().into_owned(), role.clone()]
                    .into_iter()
                    .chain([git_common_dir.clone(), worktree.clone()])
                    .chain(local.iter().cloned())
                    .chain([guard::HERE_FLAG.to_owned()])
                    .collect())
            };
            match guard::judge(&input[..], home.as_deref(), paths, checkout) {
                Verdict::Allow if pin => match again()
                    .and_then(|again| guard::pinned(&input, Path::new(worktree), &again))
                {
                    Ok(answer) => {
                        println!("{answer}");
                        ExitCode::SUCCESS
                    }
                    Err(why) => hook(Verdict::Refuse(why)),
                },
                verdict => hook(verdict),
            }
        }
        [command, rest @ ..] if shep_kelpie::flock::VERBS.contains(&command.as_str()) => {
            shep_kelpie::flock::main(command, rest)
        }
        [command, rest @ ..] if command == "doctor" => shep_kelpie::doctor::main(rest),
        [command, rest @ ..] if command == "upgrade" => shep_kelpie::upgrade::main(rest),
        [command] if command == "version" => version(false),
        [command, flag] if command == "version" && flag == "--json" => version(true),
        [role, folders @ ..] if role == "confine" && !folders.is_empty() => {
            let folders: Vec<PathBuf> = folders.iter().map(PathBuf::from).collect();
            hook(judge(std::io::stdin().lock(), &folders))
        }
        [command, sub] if command == "tools" && sub == "install" => install_tools(),
        [command] if command == "totp" => totp(false),
        [command, flag] if command == "totp" && flag == "--rotate" => totp(true),
        [command, flag] if command == "totp" && flag == "--unlock" => unlock(),
        [command, sub, project, sheep @ ..]
            if command == "settings" && sub == "move" && sheep.len() < 2 =>
        {
            move_settings(project, sheep.first().unwrap_or(project))
        }
        // `shep kelpie` with no verb also sets SHEP_DOG_NAME; only the
        // shepherd's own start sets SHEP_NAME.
        [] if ["SHEP_DOG_NAME", "SHEP_NAME"]
            .iter()
            .all(|key| std::env::var_os(key).is_some()) =>
        {
            shep_kelpie::dog::run()
        }
        _ => {
            eprintln!(
                "usage: shep-kelpie add [<project>] | add <issue>\n       shep-kelpie start | pause | status\n       shep-kelpie rule [<id> <answer>]\n       shep-kelpie rework <pr> | adopt <pr>\n       shep-kelpie gate [<issue>] | drop [<issue>]\n       shep-kelpie doctor [<project>] [--test-alert]\n       shep-kelpie upgrade --ref <git ref> | --release <version> | --binary <path> | --rollback\n       shep-kelpie version [--json]\n       shep-kelpie runner <project>\n{}\n       shep-kelpie confine <folder>...\n       shep-kelpie guard <git common dir> <worktree>\n       shep-kelpie settings move <project> [<sheep>]\n       shep-kelpie tools install\n       shep-kelpie totp [--rotate | --unlock]\n\nAdopted as `kelpie`, the same verbs run as `shep kelpie <verb>`, and `--` reaches `lease run`.\n\n{}\n\n{}",
                shep_kelpie::lease::cli::USAGE,
                shep_kelpie::flock::USAGE,
                shep_kelpie::flock::rule::HELP
            );
            ExitCode::from(2)
        }
    }
}

fn version(json: bool) -> ExitCode {
    let build = shep_kelpie::upgrade::build::Build::this();
    if json {
        let line = serde_json::to_string(&build).expect("a build serializes");
        println!("{line}");
    } else {
        println!("shep-kelpie {} (shep {})", build.kelpie, build.shep);
    }
    ExitCode::SUCCESS
}

// Kelpie's home as the runner works it out.
fn kelpie_home() -> Option<PathBuf> {
    shep_kelpie::home::kelpie_home()
        .inspect_err(|e| eprintln!("{e}"))
        .ok()
}

fn totp(rotate: bool) -> ExitCode {
    let Some(home) = kelpie_home() else {
        return ExitCode::FAILURE;
    };
    match shep_kelpie::totp::show(&home.join("totp/secret"), rotate) {
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
        return ExitCode::FAILURE;
    };
    match shep_kelpie::totp::answers::Answers::in_folder(home.join("totp")).unlock() {
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

fn move_settings(project: &str, sheep: &str) -> ExitCode {
    let Some(home) = std::env::var_os("HOME") else {
        eprintln!("HOME is not set");
        return ExitCode::FAILURE;
    };
    let project = match ProjectName::try_from(project) {
        Ok(project) => project,
        Err(e) => {
            eprintln!("shep kelpie settings move: {e}");
            return ExitCode::from(2);
        }
    };
    with_shep_home("settings move", shep_home::MOVE_FIX, |shep_home| {
        let kelpie_home = shep_kelpie::home::kelpie_home_of(shep_home);
        let paths = ProjectPaths::under(&kelpie_home, shep_home, &project);
        let files = Files {
            project: project.as_str(),
            sheep,
            settings: &paths.settings,
            kelpie_settings: &paths.kelpie_settings,
        };
        let moved = shepherd::block_on(moving::move_files(shep_home, files, Path::new(&home)));
        match moved {
            Ok(lines) => {
                for line in lines {
                    println!("{line}");
                }
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("shep kelpie settings move: {message}");
                ExitCode::FAILURE
            }
        }
    })
}

fn install_tools() -> ExitCode {
    let Some(home) = kelpie_home() else {
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

// A command with no `SHEP_HOME` would reach the default shepherd, where no
// runner is, so it refuses instead.
fn with_shep_home(role: &str, fix: &str, run: impl FnOnce(&Path) -> ExitCode) -> ExitCode {
    match shep_home::required(fix) {
        Ok(home) => run(&home),
        Err(message) => {
            eprintln!("kelpie {role}: {message}");
            ExitCode::FAILURE
        }
    }
}
