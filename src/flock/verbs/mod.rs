//! `shep kelpie <verb>`: the command line, read into a trigger for a project

mod check;

use std::ffi::OsString;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::control::{Format, Reach};
use super::glob::Named;
use super::{Checkout, Launch, add, attach, control, help, pm, rule, set, ticket, usage, watch};
use crate::adapters::Gh;
use crate::home::{ProjectName, ProjectPaths};
use crate::settings::source::{self, Loaded};
use crate::shepherd::{self, Connection, Dial, Socket};
use crate::state::Projects;
use crate::ticket::TicketId;
use crate::upgrade::restart::{Interrupt, Patience};

/// The verbs [`main`] runs: every trigger a runner takes, `add`, which also
/// registers a checkout, `issue`, which runs the issue check itself, and
/// `attach` and `pm`, which run a worker's or the project manager's session
/// here, `usage`, which reads the usage ledger here, `watch`, which
/// follows the runner's log and the sessions' transcripts here, `ticket`, which
/// files and reads a local project's tickets here, and `set` and `get`, which
/// change and read one setting of the project's table here, or of every project a glob matches
pub const VERBS: [&str; 24] = [
    "add", "start", "pause", "finish", "status", "rule", "rework", "adopt", "gate", "drop",
    "timings", "issue", "attach", "tell", "pm", "drain", "undrain", "usage", "watch", "board",
    "set", "get", "triage", "ticket",
];

/// The verbs that print the runner's JSON instead of plain text with `--json`
const JSON_VERBS: [&str; 10] = [
    "add", "finish", "status", "gate", "drop", "timings", "tell", "drain", "undrain", "board",
];

/// What `issue "<request>"` answers: it names the `issue` skill and `issue check`
const WRITER_GONE: &str = "`shep kelpie issue \"<request>\"` went with the issue writer: write \
     the issue with kelpie's `issue` skill, which every harness has, and `shep kelpie issue \
     check <ticket>` checks it";

/// What the verbs take, as their help says
pub const USAGE: &str = "\
usage: shep kelpie add [<project>] [--base <branch>]
                                         registers this checkout as a project, working
                                         against <branch>, the repo's default by default
       shep kelpie add <ticket>          puts a ticket on the project's board
       shep kelpie start | pause         runs the project, or stops it once its calls end
       shep kelpie finish [--all]        ends the open work items, picks nothing new, stops
                                         (--all: of every project)
       shep kelpie status [--json]       every project, or one with -p, as a short
                                         block each, or the runner's JSON object
       shep kelpie rule [<id> <answer>]
       shep kelpie rework <pr> | adopt <pr>
       shep kelpie gate [<ticket>] | drop [<ticket>]
       shep kelpie triage <issue>        triages an issue next, past the pick rule
       shep kelpie board                 the ready issues in the order the board takes them
       shep kelpie board move <issue> <position> | board top <issue>
                                         puts an issue at a place in that list, or first
       shep kelpie timings [<n>]         where the last n finished items' time went
       shep kelpie issue check <ticket>  whether an issue holds every part an
                                         implementer builds from, each missing one named
       shep kelpie issue check --draft <file> [--label <label>]...
                                         the same for an issue not yet filed, its title
                                         the file's first line, with no shepherd
       shep kelpie attach <ticket>       steers its worker's session here
       shep kelpie tell \"<note>\"         a note for the project manager's next wake
       shep kelpie pm                    steers the project manager's session here
       shep kelpie drain | undrain       holds back every new call, or lets them start
       shep kelpie get [<key>]           the project's settings, or one by its dotted key
       shep kelpie set <key> <value>     changes one setting, such as git.merging auto or
                                         worker.allowed_domains '[\"crates.io\"]'
       shep kelpie get -p '<glob>' <key> | set -p '<glob>' <key> <value>
                                         the same across the projects whose names match,
                                         such as -p 'shep*'; `set` writes all or none
       shep kelpie ticket                the forms of the local tickets' verbs, below
       shep kelpie ticket new \"<title>\" [--body <text>] [--label <label>]... [--parent <id>] [--blocked-by <id>]...
                                         files a local ticket and prints its id
       shep kelpie ticket list [--all] | ticket show <id> | ticket close <id> [--not-planned] [--duplicate]
                                         reads a local ticket, or closes it
       shep kelpie ticket edit <id> [--title <text>] [--body <text>] [--label <label>]... [--unlabel <label>]... [--parent <id>] [--no-parent] [--blocked-by <id>]... [--unblock <id>]...
                                         changes a local ticket
       shep kelpie watch [<issue> | pm] [--back <n>] [--full]
                                         follows the runner's steps and the sessions'
                                         transcripts live, from the end or n lines back
       shep kelpie usage [<project>] [--since <date>]
                                         units and dollars per merged pull request
       shep kelpie usage <project> --import <steps file>
                                         adds a runner's logged calls to its ledger
       shep kelpie usage <project> --item <issue> [--json]
                                         one work item's calls and their transcripts
       shep kelpie usage <project> --context [--since <date>] [--json]
                                         request sizes and cache shares per role
       shep kelpie usage <project> --reads [--since <date>] [--json]
                                         what workers read before their first edit
       shep kelpie usage [<project>] --openmetrics
                                         the ledger as OpenMetrics counters, for promtool
       shep kelpie usage --forge [--since <date>]
                                         GitHub calls per minute, the busiest minutes and
                                         each used-up answer

`--json` goes anywhere in the line of `add <issue>`, `finish`, `status`, `gate`,
`drop`, `timings`, `tell`, `drain`, `undrain` and `board`, and prints the runner's
JSON object in place of plain text.

`-p <project>` or `--project <project>` goes anywhere in the line, before a
ruling's answer. Without it, the project is the one whose repo holds this
folder, or whose GitHub repo is this clone's `origin`. Only `set` and `get` take a
glob of project names (`*`, `?`, `[...]`) in `-p`: quote it, or the shell expands
it against files.";

/// Runs `kelpie <command> <args>` for one of [`VERBS`]
pub fn main(command: &str, args: &[String]) -> ExitCode {
    main_with(&|name| std::env::var_os(name), command, args)
}

// `main`, with `env` giving each variable.
fn main_with(env: &dyn Fn(&str) -> Option<OsString>, command: &str, args: &[String]) -> ExitCode {
    if let Some(code) = without_shep_home(env, command, args) {
        return code;
    }
    let ran = crate::shep_home::from(env("SHEP_HOME"), crate::shep_home::FLOCK_FIX)
        .and_then(|shep_home| shepherd::block_on(run(&Socket, &shep_home, command, args)));
    match ran {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("shep kelpie {command}: {message}");
            ExitCode::FAILURE
        }
    }
}

// The exit code of the one verb that runs with no `SHEP_HOME`, the issue
// check, which a worker's sandbox runs. `None` for every other verb.
fn without_shep_home(
    env: &dyn Fn(&str) -> Option<OsString>,
    command: &str,
    args: &[String],
) -> Option<ExitCode> {
    check::asked(command, args).then(|| ExitCode::from(check::main(env, args)))
}

async fn run(
    dial: &dyn Dial,
    shep_home: &Path,
    command: &str,
    args: &[String],
) -> Result<Vec<String>, String> {
    if help::asked(command, args) {
        return Ok(USAGE.lines().map(str::to_owned).collect());
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let kelpie_home_set = std::env::var_os(crate::home::KELPIE_VAR)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let kelpie_home = crate::home::kelpie_home_of(shep_home);
    // A ruling's answer starts after its id and first word.
    let (named, args) = split_project(args, (command == "rule").then_some(2))?;
    // Only `set` and `get` take a glob of projects.
    let (named, glob) = Named::split(named, command)?;
    // Only `add` takes a base: to every other verb `--base` is a word.
    let (base, args) = match command {
        "add" => split_base(&args)?,
        _ => (None, args),
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // Only these verbs have a JSON form, so `--json` is a word to every other verb.
    let json = JSON_VERBS.contains(&command) && args.contains(&"--json");
    let args: Vec<&str> = if json {
        args.into_iter().filter(|arg| *arg != "--json").collect()
    } else {
        args
    };
    // Only `finish` takes `--all`, and to every other verb it is a word.
    let all = command == "finish" && args.contains(&"--all");
    let args: Vec<&str> = if all {
        args.into_iter().filter(|arg| *arg != "--all").collect()
    } else {
        args
    };
    if command == "rule" {
        return answer(dial, shep_home, &home, &kelpie_home, named.as_ref(), &args).await;
    }
    if command == "usage" {
        return usage::usage(&kelpie_home, named, &args);
    }
    if command == "ticket" {
        let here = std::env::current_dir().map_err(|e| format!("cannot read this folder: {e}"))?;
        let project = match named {
            Some(name) => name,
            None => {
                let client = shepherd::connect(dial, shep_home)
                    .await
                    .map_err(|e| e.describe(shep_home))?;
                control::project_here(&client, &here, &home).await?
            }
        };
        let folder = ProjectPaths::under(&kelpie_home, shep_home, &project).tickets();
        return ticket::run(&folder, &args);
    }
    if command == "watch" {
        let ask = watch::read_args(&args)?;
        let here = std::env::current_dir().map_err(|e| format!("cannot read this folder: {e}"))?;
        // The shepherd is reached only to find the project from this folder.
        let project = match named {
            Some(name) => name,
            None => {
                let client = shepherd::connect(dial, shep_home)
                    .await
                    .map_err(|e| e.describe(shep_home))?;
                control::project_here(&client, &here, &home).await?
            }
        };
        watch::follow(&kelpie_home, &project, ask)?;
        return Ok(Vec::new());
    }
    let here = std::env::current_dir().map_err(|e| format!("cannot read this folder: {e}"))?;
    let client = shepherd::connect(dial, shep_home)
        .await
        .map_err(|e| e.describe(shep_home))?;
    // Named, or the one whose repo holds this folder.
    let project = async || match &named {
        Some(name) => Ok(name.clone()),
        None => control::project_here(&client, &here, &home).await,
    };
    if let Some(glob) = &glob {
        let homes = set::Homes {
            home: &home,
            kelpie_home: &kelpie_home,
            shep_home,
        };
        return match (command, args.as_slice()) {
            ("get", [key]) => set::get_all(&client, glob, key).await,
            ("set", [key, value]) => set::set_all(&client, glob, homes, key, value).await,
            ("get", []) => Err(format!("`-p {glob}` needs a key to get\n\n{USAGE}")),
            _ => Err(USAGE.to_owned()),
        };
    }
    let reach = Reach::new(&kelpie_home);
    let send = async |action: &str, params: Option<&str>| {
        control::send(&reach, &client, &project().await?, action, params).await
    };
    // The runner's JSON with `--json`, else its answer to `action` as plain lines.
    let print = async |action: &str, params: Option<&str>| {
        // The project is found before the action is sent, so a lookup that fails
        // cannot follow an action the runner already took.
        let name = project().await?;
        let answer = control::send(&reach, &client, &name, action, params).await?;
        Ok(match (json, answer.as_slice()) {
            (false, [body]) => control::plain_reply(name.as_str(), action, params, body),
            _ => answer,
        })
    };
    let format = if json { Format::Json } else { Format::Plain };
    // An action that changes the runner: one it takes late is no failure.
    let change = async |action: &str, params: Option<&str>| {
        let name = project().await?;
        control::change(&reach, &client, &name, action, params, format).await
    };
    // `start`, `pause`, `finish` and `add` take their project in place of `-p` too.
    let positional = |name: Option<&&str>| match (name, &named) {
        (Some(_), Some(_)) => Err(format!("name the project once\n\n{USAGE}")),
        (Some(name), None) => ProjectName::try_from(*name)
            .map(Some)
            .map_err(|e| e.to_string()),
        (None, named) => Ok(named.clone()),
    };
    match (command, args.as_slice()) {
        ("add", [ticket]) if base.is_some() && is_ticket(ticket) => Err(format!(
            "`--base` goes with a checkout, not a ticket\n\n{USAGE}"
        )),
        ("add", [ticket]) if is_ticket(ticket) => change("add", Some(ticket)).await,
        // Registering a checkout has no JSON form.
        ("add", _) if json => Err(USAGE.to_owned()),
        ("add", [] | [_]) => {
            let checkout = Checkout::of(&here)?;
            let name = match positional(args.first())? {
                Some(name) => name,
                None => ProjectName::try_from(checkout.forge.name()).map_err(|e| e.to_string())?,
            };
            not_a_ticket(&name)?;
            let launch = Launch {
                kelpie: std::env::current_exe()
                    .map_err(|e| format!("cannot find kelpie itself: {e}"))?,
                shep_home: shep_home.to_owned(),
                kelpie_home: kelpie_home_set,
            };
            let old_home = crate::home::old_home();
            let folder = kelpie_home.join(name.as_str());
            let agents = kelpie_home.join(crate::agents::FOLDER);
            let place = add::Place {
                checkout: &checkout,
                home: &home,
                folder: &folder,
                agents: &agents,
                old_home: old_home.as_deref(),
                base: base.as_deref(),
            };
            add::add(&client, &Gh::default(), &launch, &name, place).await
        }
        ("finish", []) if all && named.is_none() => {
            // What it waits on goes to stderr under `--json`, so stdout is the answers.
            let say = &mut |line: String| match json {
                true => eprintln!("{line}"),
                false => println!("{line}"),
            };
            let done = control::finish_all(&client, format, Patience::default(), say).await?;
            if done.refused.is_empty() {
                return Ok(done.lines);
            }
            done.lines.iter().for_each(|line| println!("{line}"));
            Err(done.refused.join("\n"))
        }
        ("finish", _) if all => Err(format!("name the project once\n\n{USAGE}")),
        ("start" | "pause" | "finish", [] | [_]) => {
            let name = match positional(args.first())? {
                Some(name) => name,
                None => project().await?,
            };
            match command {
                "start" => control::start(&client, &name).await,
                "finish" => {
                    // What it waits on goes to stderr under `--json`, so stdout is the object.
                    let say = &mut |line: String| match json {
                        true => eprintln!("{line}"),
                        false => println!("{line}"),
                    };
                    control::finish(&client, &name, format, Patience::default(), say).await
                }
                _ => {
                    let say = &mut |line: String| println!("{line}");
                    let patience = Patience::default();
                    control::pause(&client, &name, patience, Interrupt::Signals, say).await
                }
            }
        }
        ("status", []) if named.is_none() => control::status(&reach, &client, format).await,
        ("status", []) => {
            let answer = send("status", None).await?;
            Ok(match (json, answer.as_slice()) {
                (false, [body]) => {
                    let name = project().await?;
                    let installed = control::installed_build(&client).await;
                    control::plain_status(name.as_str(), body, installed.as_deref())
                }
                _ => answer,
            })
        }
        ("rework" | "adopt" | "triage", [number]) => change(command, Some(number)).await,
        ("gate" | "drop", []) => change(command, None).await,
        ("gate" | "drop", [issue]) => change(command, Some(issue)).await,
        ("board", []) => print("board", None).await,
        ("board", ["top", issue]) if is_number(issue) => {
            change("board", Some(&format!("top {issue}"))).await
        }
        ("board", ["move", issue, position]) if is_number(issue) && is_number(position) => {
            change("board", Some(&format!("move {issue} {position}"))).await
        }
        ("timings", []) => print("timings", None).await,
        ("timings", [count]) => print("timings", Some(count)).await,
        ("drain" | "undrain", []) => change(command, None).await,
        ("attach", [text]) => {
            let ticket = text.parse::<TicketId>();
            let ticket = ticket.map_err(|_| format!("{text:?} is not a ticket id"))?;
            attach::attach(&reach, &client, &project().await?, ticket).await
        }
        ("tell", [_, ..]) => {
            let project = project().await?;
            control::tell(&reach, &client, &project, &args.join(" "), format).await
        }
        ("pm", []) => pm::pm(&reach, &client, &project().await?).await,
        ("get", [] | [_]) => set::get(&client, &project().await?, args.first().copied()).await,
        ("set", [key, value]) => {
            let name = project().await?;
            let paths = ProjectPaths::under(&kelpie_home, shep_home, &name);
            let place = set::Place {
                home: &home,
                paths: &paths,
            };
            set::set(&client, &name, place, key, value).await
        }
        // `main` runs the check itself, so a form it did not take is the usage.
        ("issue", ["check", ..]) => Err(USAGE.to_owned()),
        ("issue", [_, ..]) => Err(WRITER_GONE.to_owned()),
        _ => Err(USAGE.to_owned()),
    }
}

// Whether `text` is digits only, as an issue or a position is.
fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

// A project's name may not read as a ticket id, since `add <name>` would add
// the ticket.
fn not_a_ticket(name: &ProjectName) -> Result<(), String> {
    if !is_ticket(name.as_str()) {
        return Ok(());
    }
    Err(format!(
        "{name:?} reads as a ticket id, so it cannot name a project: register this checkout \
         under another name with `add <project>`"
    ))
}

// Whether `text` is shaped like a ticket id, digits or `k` and digits, for the
// runner to refuse a zero or a leading zero.
fn is_ticket(text: &str) -> bool {
    is_number(text.strip_prefix('k').unwrap_or(text))
}

// Where `issue` finds the project's settings and files.
struct Here<'a> {
    client: &'a dyn Connection,
    shep_home: &'a Path,
    kelpie_home: &'a Path,
    home: &'a Path,
}

// The project's paths and settings, as its runner would load them.
async fn load(here: &Here<'_>, name: &ProjectName) -> Result<(ProjectPaths, Loaded), String> {
    let tables = shepherd::read_tables_with(here.client, name.as_str()).await?;
    let paths = ProjectPaths::under(here.kelpie_home, here.shep_home, name);
    let loaded = source::load(&tables, name.as_str(), here.home, &paths.folder)
        .map_err(|e| e.to_string())?;
    Ok((paths, loaded))
}

// `rule`: the ruling and its answer read first, asking in a terminal, so
// the shepherd is reached to send it, and before that only to find the
// project of the clone a number is typed in.
async fn answer(
    dial: &dyn Dial,
    shep_home: &Path,
    home: &Path,
    kelpie_home: &Path,
    named: Option<&ProjectName>,
    args: &[&str],
) -> Result<Vec<String>, String> {
    let projects = Projects::under(kelpie_home);
    // Not in a clone, or no shepherd to ask, is a number with no project near.
    let here = match (named, args.first()) {
        (None, Some(first)) if rule::names_ruling(first) => {
            let here = std::env::current_dir().ok();
            match (shepherd::connect(dial, shep_home).await, here) {
                (Ok(client), Some(here)) => control::project_here(&client, &here, home).await.ok(),
                _ => None,
            }
        }
        _ => None,
    };
    let terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let (mut input, mut output) = (std::io::stdin().lock(), std::io::stdout());
    let ask = terminal.then_some((
        &mut input as &mut dyn BufRead,
        &mut output as &mut dyn Write,
    ));
    let (project, params) = rule::prepare_in(&projects, named, here.as_ref(), args, ask)?;
    let client = shepherd::connect(dial, shep_home)
        .await
        .map_err(|e| e.describe(shep_home))?;
    let answers = ProjectPaths::under(kelpie_home, shep_home, &project).answers;
    control::rule(
        &Reach::new(kelpie_home),
        &client,
        &project,
        &params,
        &answers,
    )
    .await
}

/// `-p <project>`, `--project <project>` or `--project=<project>` in `args`, and the rest
///
/// The flag is read from anywhere before a `--`. A value with `*`, `?` or `[`
/// is a glob over the projects' names. With `words_from`, everything after that
/// many other arguments is words, so a ruling's answer can carry `-p`.
///
/// # Errors
///
/// A message when the project is given twice, has no name after it, or is
/// neither a project name nor a glob.
pub fn split_project(
    args: &[String],
    words_from: Option<usize>,
) -> Result<(Option<Named>, Vec<String>), String> {
    let mut named = None;
    let mut rest = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if words_from.is_some_and(|from| rest.len() >= from) {
            rest.push(arg.clone());
            continue;
        }
        let name = match arg.as_str() {
            "--" => {
                rest.extend(args.by_ref().cloned());
                break;
            }
            "-p" | "--project" => args
                .next()
                .ok_or_else(|| format!("{arg} takes a project's name"))?
                .as_str(),
            other => match other.strip_prefix("--project=") {
                Some(name) => name,
                None => {
                    rest.push(arg.clone());
                    continue;
                }
            },
        };
        if named.is_some() {
            return Err("name the project once".into());
        }
        named = Some(Named::parse(name)?);
    }
    Ok((named, rest))
}

/// `--base <branch>` or `--base=<branch>` from anywhere in `args` before a
/// `--`, and the rest in order
///
/// # Errors
///
/// A message when the base is given twice or has no branch after it.
pub fn split_base(args: &[String]) -> Result<(Option<String>, Vec<String>), String> {
    let mut named = None;
    let mut rest = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let branch = match arg.as_str() {
            "--" => {
                rest.push(arg.clone());
                rest.extend(args.by_ref().cloned());
                break;
            }
            "--base" => args
                .next()
                .ok_or_else(|| "--base takes a branch's name".to_owned())?
                .as_str(),
            other => match other.strip_prefix("--base=") {
                Some(branch) => branch,
                None => {
                    rest.push(arg.clone());
                    continue;
                }
            },
        };
        if named.is_some() {
            return Err("name the base once".into());
        }
        named = Some(branch.to_owned());
    }
    Ok((named, rest))
}

/// `args` with a verb that follows leading project flags moved first, so
/// `shep kelpie -p koji rule 14 yes` reaches `rule`
pub fn verb_first(mut args: Vec<String>) -> Vec<String> {
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        match arg.as_str() {
            "-p" | "--project" => at += 2,
            flag if flag.starts_with("--project=") => at += 1,
            verb if at > 0 && VERBS.contains(&verb) => {
                let verb = args.remove(at);
                args.insert(0, verb);
                break;
            }
            _ => break,
        }
    }
    args
}

#[cfg(test)]
mod tests;
