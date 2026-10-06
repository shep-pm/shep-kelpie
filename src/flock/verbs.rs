//! `shep kelpie <verb>`: the command line, read into a trigger for a project

use std::ffi::OsStr;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::{Checkout, Launch, add, attach, control, pm, rule, usage};
use crate::adapters::{ClaudeCli, Gh};
use crate::agents::Agents;
use crate::issues::{self, Mode, Project, Writer};
use crate::ports::SandboxError;
use crate::runner::{ProjectName, ProjectPaths};
use crate::settings::source;
use crate::shepherd;
use crate::state::ids::RulingIds;
use crate::tools::Tools;
use crate::upgrade::restart::{Interrupt, Patience};

/// The verbs [`main`] runs: every trigger a runner takes, `add`, which also
/// registers a checkout, `issue`, which runs the issue writer itself, and
/// `attach` and `pm`, which run a worker's or the project manager's session
/// here, and `usage`, which reads the usage ledger here
pub const VERBS: [&str; 17] = [
    "add", "start", "pause", "status", "rule", "rework", "adopt", "gate", "drop", "timings",
    "issue", "attach", "tell", "pm", "drain", "undrain", "usage",
];

/// What the verbs take, as their help says
pub const USAGE: &str = "\
usage: shep kelpie add [<project>]       registers this checkout as a project
       shep kelpie add <issue>           puts an issue on the project's board
       shep kelpie start | pause         runs the project, or stops it once its calls end
       shep kelpie status                every project, or one with -p
       shep kelpie rule [<id> <answer>]
       shep kelpie rework <pr> | adopt <pr>
       shep kelpie gate [<issue>] | drop [<issue>]
       shep kelpie timings [<n>]         where the last n finished items' time went
       shep kelpie issue \"<request>\"     files issues for it, for you to read
       shep kelpie issue --interactive \"<request>\"
                                         plans them with you in claude
       shep kelpie attach <issue>        steers its worker's session here
       shep kelpie tell \"<note>\"         a note for the project manager's next wake
       shep kelpie pm                    steers the project manager's session here
       shep kelpie drain | undrain       holds back every new call, or lets them start
       shep kelpie usage [<project>] [--since <date>]
                                         units and dollars per merged pull request
       shep kelpie usage <project> --import <log file>
                                         adds a runner's logged calls to its ledger

`-p <project>` or `--project <project>` goes anywhere in the line, before a
ruling's answer. Without it, the project is the one whose repo holds this
folder.";

/// Runs `kelpie <command> <args>` for one of [`VERBS`]
pub fn main(command: &str, args: &[String]) -> ExitCode {
    let ran = crate::shep_home::required(crate::shep_home::FLOCK_FIX)
        .and_then(|shep_home| shepherd::block_on(run(&shep_home, command, args)));
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

async fn run(shep_home: &Path, command: &str, args: &[String]) -> Result<Vec<String>, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let kelpie_home_set = std::env::var_os(crate::home::KELPIE_VAR)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let kelpie_home = crate::home::kelpie_home_of(shep_home);
    // A ruling's answer starts after its id and first word.
    let (named, args) = split_project(args, (command == "rule").then_some(2))?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if command == "rule" {
        return answer(shep_home, &kelpie_home, named.as_ref(), &args).await;
    }
    if command == "usage" {
        return usage::usage(&kelpie_home, named, &args);
    }
    let here = std::env::current_dir().map_err(|e| format!("cannot read this folder: {e}"))?;
    let client = shepherd::connect(shep_home)
        .await
        .map_err(|e| e.describe(shep_home))?;
    // Named, or the one whose repo holds this folder.
    let project = async || match &named {
        Some(name) => Ok(name.clone()),
        None => control::project_here(&client, &here, &home).await,
    };
    let send = async |action: &str, params: Option<&str>| {
        control::send(&client, &project().await?, action, params).await
    };
    // `start`, `pause` and `add` took their project in place of `-p`.
    let positional = |name: Option<&&str>| match (name, &named) {
        (Some(_), Some(_)) => Err(format!("name the project once\n\n{USAGE}")),
        (Some(name), None) => ProjectName::try_from(*name)
            .map(Some)
            .map_err(|e| e.to_string()),
        (None, named) => Ok(named.clone()),
    };
    match (command, args.as_slice()) {
        ("add", [issue]) if !issue.is_empty() && issue.bytes().all(|b| b.is_ascii_digit()) => {
            send("add", Some(issue)).await
        }
        ("add", [] | [_]) => {
            let checkout = Checkout::of(&here)?;
            let name = match positional(args.first())? {
                Some(name) => name,
                None => ProjectName::try_from(checkout.forge.name()).map_err(|e| e.to_string())?,
            };
            let launch = Launch {
                kelpie: std::env::current_exe()
                    .map_err(|e| format!("cannot find kelpie itself: {e}"))?,
                shep_home: shep_home.to_owned(),
                kelpie_home: kelpie_home_set,
            };
            let folder = kelpie_home.join(name.as_str());
            let agents = kelpie_home.join(crate::agents::FOLDER);
            let place = add::Place {
                checkout: &checkout,
                home: &home,
                folder: &folder,
                agents: &agents,
            };
            add::add(&client, &Gh, &launch, &name, place).await
        }
        ("start" | "pause", [] | [_]) => {
            let name = match positional(args.first())? {
                Some(name) => name,
                None => project().await?,
            };
            match command {
                "start" => control::start(&client, &name).await,
                _ => {
                    let say = &mut |line: String| println!("{line}");
                    let patience = Patience::default();
                    control::pause(&client, &name, patience, Interrupt::Signals, say).await
                }
            }
        }
        ("status", []) if named.is_none() => control::status(&client).await,
        ("status", []) => send("status", None).await,
        ("rework" | "adopt", [number]) => send(command, Some(number)).await,
        ("gate" | "drop", []) => send(command, None).await,
        ("gate" | "drop", [issue]) => send(command, Some(issue)).await,
        ("timings", []) => send("timings", None).await,
        ("timings", [count]) => send("timings", Some(count)).await,
        ("drain" | "undrain", []) => send(command, None).await,
        ("attach", [issue]) => {
            let number = (issue.parse::<u64>().ok())
                .filter(|&n| n > 0 && issue.bytes().all(|b| b.is_ascii_digit()));
            let issue = number.ok_or_else(|| format!("{issue:?} is not an issue number"))?;
            attach::attach(&client, &project().await?, issue).await
        }
        ("tell", [_, ..]) => {
            let project = project().await?;
            control::send(&client, &project, "tell", Some(&args.join(" "))).await?;
            Ok(vec![format!(
                "told {project}'s project manager, which reads it on its next wake"
            )])
        }
        ("pm", []) => pm::pm(&client, &project().await?).await,
        ("issue", [_, ..]) => {
            let here = Here {
                client: &client,
                shep_home,
                kelpie_home: &kelpie_home,
                home: &home,
            };
            issue(here, &project().await?, &args).await
        }
        _ => Err(USAGE.to_owned()),
    }
}

// Where `issue` finds the project's settings and files.
struct Here<'a> {
    client: &'a shep_client::Client,
    shep_home: &'a Path,
    kelpie_home: &'a Path,
    home: &'a Path,
}

// `issue [--interactive] <request>`: the issue writer, run here rather than
// by the runner, since the maintainer waits on it or works in it.
async fn issue(here: Here<'_>, name: &ProjectName, args: &[&str]) -> Result<Vec<String>, String> {
    let (interactive, words) = match args {
        ["--interactive", words @ ..] => (true, words),
        words => (false, words),
    };
    let request = words.join(" ");
    if request.trim().is_empty() || request.starts_with('-') {
        return Err(USAGE.to_owned());
    }
    let tables = shepherd::read_tables_with(here.client, name.as_str()).await?;
    let paths = ProjectPaths::under(here.kelpie_home, here.shep_home, name);
    let loaded = source::load(&tables, name.as_str(), here.home, &paths.folder)
        .map_err(|e| e.to_string())?;
    let agents = Agents::load(&paths.agents).map_err(|e| e.to_string())?;
    let listed = (loaded.settings.role_agents(&agents)).map_err(|e| e.to_string())?;
    let writer = Writer::of(&agents)?;
    let kelpie = std::env::current_exe().map_err(|e| format!("cannot find kelpie itself: {e}"))?;
    let project = Project {
        settings: &loaded.settings,
        paths: &paths,
        agents: &listed,
        kelpie: &kelpie,
    };
    if interactive {
        issues::make_labels(&Gh, &project, Mode::Interactive)?;
        let mut claude = issues::interactive(&project, &writer, &request, OsStr::new("claude"))?;
        let status = claude.command.status();
        for file in &claude.files {
            let _ = std::fs::remove_file(file);
        }
        let status = status.map_err(|e| format!("cannot run claude: {e}"))?;
        return match status.success() {
            true => Ok(Vec::new()),
            false => Err(format!("claude ended with {status}")),
        };
    }
    let tools = Tools::under(here.kelpie_home);
    if !tools.sandbox().is_file() {
        return Err(SandboxError::Missing(tools.sandbox()).to_string());
    }
    let codex_home =
        (loaded.kelpie.codex_home(here.home, here.kelpie_home)).map_err(|e| e.to_string())?;
    let claude = ClaudeCli::default().in_runtime(tools, here.home.to_owned(), &codex_home);
    issues::headless(&project, &writer, &request, &Gh, &claude)
}

// `rule`: the ruling and its answer read first, asking in a terminal, so
// the shepherd is reached only to send it.
async fn answer(
    shep_home: &Path,
    kelpie_home: &Path,
    named: Option<&ProjectName>,
    args: &[&str],
) -> Result<Vec<String>, String> {
    let ids = RulingIds::under(kelpie_home);
    let terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let (mut input, mut output) = (std::io::stdin().lock(), std::io::stdout());
    let ask = terminal.then_some((
        &mut input as &mut dyn BufRead,
        &mut output as &mut dyn Write,
    ));
    let (project, params) = rule::prepare(&ids, named, args, ask)?;
    let client = shepherd::connect(shep_home)
        .await
        .map_err(|e| e.describe(shep_home))?;
    let answers = ProjectPaths::under(kelpie_home, shep_home, &project).answers;
    control::rule(&client, &project, &params, &answers).await
}

/// `-p <project>`, `--project <project>` or `--project=<project>` from
/// anywhere in `args` before a `--`, and the rest in order
///
/// With `words_from`, everything after that many other arguments is words,
/// so a ruling's answer can carry `-p`.
///
/// # Errors
///
/// A message when the project is given twice, has no name after it, or is
/// not a project name.
pub fn split_project(
    args: &[String],
    words_from: Option<usize>,
) -> Result<(Option<ProjectName>, Vec<String>), String> {
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
        named = Some(ProjectName::try_from(name).map_err(|e| e.to_string())?);
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
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn the_project_goes_anywhere_in_the_line_before_a_double_dash() {
        let koji = ProjectName::try_from("koji").ok();
        for line in [
            "-p koji 3 7",
            "3 7 -p koji",
            "3 --project koji 7",
            "3 7 --project=koji",
        ] {
            let (named, rest) = split_project(&words(line), None).unwrap();
            assert_eq!(named, koji, "{line}");
            assert_eq!(rest, words("3 7"), "{line}");
        }
        let (named, rest) = split_project(&words("14 no -- drop the -p flag"), None).unwrap();
        assert_eq!(named, None);
        assert_eq!(rest, words("14 no drop the -p flag"));
        for refused in ["-p", "-p koji -p rotom", "-p ../koji"] {
            assert!(split_project(&words(refused), None).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_ruling_s_answer_keeps_a_p_as_its_own_words() {
        let koji = ProjectName::try_from("koji").ok();
        for line in ["-p koji 14 yes", "14 -p koji yes", "14 --project=koji yes"] {
            let (named, rest) = split_project(&words(line), Some(2)).unwrap();
            assert_eq!(named, koji, "{line}");
            assert_eq!(rest, words("14 yes"), "{line}");
        }
        let line = words("14 no pass -p koji through");
        let (named, rest) = split_project(&line, Some(2)).unwrap();
        assert_eq!(named, None);
        assert_eq!(rest, line);
    }

    #[test]
    fn a_verb_after_the_project_goes_first() {
        let moved = |line| verb_first(words(line));
        assert_eq!(moved("-p koji rule 14 yes"), words("rule -p koji 14 yes"));
        assert_eq!(moved("--project=koji gate"), words("gate --project=koji"));
        assert_eq!(moved("-p koji timings 5"), words("timings -p koji 5"));
        assert_eq!(
            moved("-p koji issue --interactive add a thing"),
            words("issue -p koji --interactive add a thing")
        );
        assert_eq!(moved("-p koji attach 7"), words("attach -p koji 7"));
        assert_eq!(moved("-p koji tell hold #4"), words("tell -p koji hold #4"));
        assert_eq!(moved("rule -p koji 14 yes"), words("rule -p koji 14 yes"));
        assert_eq!(moved("-p koji runner x"), words("-p koji runner x"));
        assert_eq!(moved("-p"), words("-p"));
    }
}
