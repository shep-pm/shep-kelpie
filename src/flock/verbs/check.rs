//! `shep kelpie issue check`, which exits 0 when the issue passes, 1 when it
//! fails, and 2 when it could not be checked
//!
//! The draft form, `issue check --draft <file> [--label <label>]...`, asks for
//! no `SHEP_HOME`, settings or network, so a worker can run it in its sandbox.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use super::{Here, USAGE, load, split_project};
use crate::adapters::Gh;
use crate::flock::control;
use crate::flock::glob::Named;
use crate::home::ProjectName;
use crate::issues::check::{self, Checked};
use crate::local_tickets::{LocalTickets, Routed};
use crate::shepherd::{self, Dial, Socket};
use crate::ticket::TicketId;

/// Whether `issue <args>` is the check, wherever `-p` sits in it
pub(super) fn asked(command: &str, args: &[String]) -> bool {
    let rest = split_project(args, None).map(|(_, rest)| rest);
    command == "issue" && rest.is_ok_and(|rest| rest.first().is_some_and(|w| w == "check"))
}

/// Runs `issue <args>` as the check, `env` giving each variable, and returns
/// its exit code
pub(super) fn main(env: &dyn Fn(&str) -> Option<OsString>, args: &[String]) -> u8 {
    let checked = split_project(args, None).and_then(|(named, rest)| match &rest[1..] {
        [flag, file, labels @ ..] if flag == "--draft" => {
            draft_labels(labels).and_then(|labels| check::draft(Path::new(file), &labels))
        }
        [text] => {
            let ticket = text.parse::<TicketId>();
            let ticket = ticket.map_err(|_| format!("{text:?} is not a ticket id"))?;
            let shep_home = env("SHEP_HOME");
            let shep_home = crate::shep_home::from(shep_home, crate::shep_home::FLOCK_FIX)?;
            let home = crate::home::user_home_from(env).ok_or(crate::home::HOME_UNSET)?;
            let (named, _) = Named::split(named, "issue")?;
            let at = Place {
                shep_home: &shep_home,
                home: &home,
                named,
            };
            shepherd::block_on(ticket_in(&Socket, at, ticket))
        }
        _ => Err(USAGE.to_owned()),
    });
    match checked {
        Ok(Checked::Passes(line)) => {
            println!("{line}");
            0
        }
        Ok(Checked::Fails(lines)) => {
            lines.iter().for_each(|line| eprintln!("{line}"));
            1
        }
        Err(why) => {
            eprintln!("shep kelpie issue check: {why}");
            2
        }
    }
}

// The labels `--label <label>` names, each once or more.
fn draft_labels(words: &[String]) -> Result<Vec<String>, String> {
    let pairs = words.chunks(2);
    pairs
        .map(|pair| match pair {
            [flag, label] if flag == "--label" => Ok(label.clone()),
            _ => Err(USAGE.to_owned()),
        })
        .collect()
}

// Where the ticket form finds the shepherd and the project.
struct Place<'a> {
    shep_home: &'a Path,
    home: &'a Path,
    named: Option<ProjectName>,
}

// The ticket as the project's own ticket source holds it, checked.
async fn ticket_in(dial: &dyn Dial, at: Place<'_>, ticket: TicketId) -> Result<Checked, String> {
    let client = shepherd::connect(dial, at.shep_home)
        .await
        .map_err(|e| e.describe(at.shep_home))?;
    let name = match at.named {
        Some(name) => name,
        None => {
            let here =
                std::env::current_dir().map_err(|e| format!("cannot read this folder: {e}"))?;
            control::project_here(&client, &here, at.home).await?
        }
    };
    let kelpie_home = crate::home::kelpie_home_of(at.shep_home);
    let here = Here {
        client: &client,
        shep_home: at.shep_home,
        kelpie_home: &kelpie_home,
        home: at.home,
    };
    let (paths, loaded) = load(&here, &name).await?;
    let remote = crate::runner::check_checkout(&loaded.settings).map_err(|e| e.to_string())?;
    let tickets = Routed::new(
        Arc::new(Gh::default()),
        LocalTickets::new(paths.tickets()),
        loaded.settings.tickets.source,
    );
    check::run(&tickets, &remote, ticket)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::process::ExitCode;

    use super::*;

    const BODY: &str = "A thing is true\nTouches: `src/board.rs`\n\n## What to build\n\n- A \
        thing\n\n## Acceptance criteria\n\n- [ ] It works\n";

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    // A worker's variables, as its profile sets them for every command it runs.
    fn worker_env(root: &Path) -> BTreeMap<String, String> {
        let path = |p: &str| root.join(p);
        let (worktree, build, common, git_dir) = (
            path("wt"),
            path("build"),
            path("git"),
            path("git/worktrees/wt"),
        );
        let (kelpie, skills, kelpie_home, home, shep_home, door) = (
            path("kelpie"),
            path("skills"),
            path("home/.kelpie/shep/kelpie"),
            path("home"),
            path("home/.kelpie/shep"),
            path("door.sock"),
        );
        let profile = crate::profile::WorkerProfile {
            worktree: &worktree,
            build: &build,
            git_common_dir: &common,
            git_dir: &git_dir,
            branch: "kelpie/7",
            base: "main",
            kelpie_pushes: false,
            kelpie: &kelpie,
            skills: &skills,
            kelpie_home: &kelpie_home,
            repo: &worktree,
            guard_hooks: &[],
            allowed_domains: &[],
            registries: &[],
            build_env: &BTreeMap::new(),
            home: &home,
            shep_home: &shep_home,
            door: &door,
            token: None,
            login_token: None,
        };
        profile.reach().fence.expect("a worker is fenced").env
    }

    #[test]
    fn a_draft_is_checked_with_only_a_workers_variables_and_no_shepherd() {
        let dir = tempfile::tempdir().unwrap();
        let env = worker_env(dir.path());
        assert!(!env.contains_key("SHEP_HOME"), "{env:?}");
        let lookup = |name: &str| env.get(name).map(OsString::from);
        let file = dir.path().join("issue.md");
        std::fs::write(&file, BODY).unwrap();
        let file = file.display().to_string();
        let mut args = words(&format!("check --draft {file} --label kind:feat --label"));
        args.push("priority: P4".to_owned());
        let verb = |args: &[String]| super::super::main_with(&lookup, "issue", args);
        assert_eq!(verb(&args), ExitCode::SUCCESS);
        let unlabelled = words(&format!("check --draft {file}"));
        assert_eq!(verb(&unlabelled), ExitCode::from(1));
        let gone = words(&format!("check --draft {file}.gone"));
        assert_eq!(verb(&gone), ExitCode::from(2));
        // Any other verb still needs the shepherd's home, and says so.
        let status = super::super::main_with(&lookup, "status", &[]);
        assert_eq!(status, ExitCode::FAILURE);
    }

    #[test]
    fn issue_check_takes_one_ticket_id_or_a_draft_and_nothing_else() {
        let none = |_: &str| None;
        for line in [
            "check",
            "check 7 8",
            "check seven",
            "check --draft",
            "check --draft f x",
        ] {
            assert_eq!(main(&none, &words(line)), 2, "{line}");
        }
        assert!(asked("issue", &words("-p koji check 7")));
        assert!(!asked("issue", &words("checks the board")));
        assert!(!asked("ticket", &words("check 7")));
    }
}
