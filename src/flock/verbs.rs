//! `shep kelpie <verb>`: the command line, read into a trigger for a project

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::{Checkout, Launch, add, control, rule};
use crate::adapters::Gh;
use crate::runner::{ProjectName, ProjectPaths};
use crate::shepherd;
use crate::state::ids::RulingIds;

/// The verbs [`main`] runs: every trigger a runner takes but the relay's,
/// and `add` also registers a checkout
pub const VERBS: [&str; 9] = [
    "add", "start", "pause", "status", "rule", "rework", "adopt", "gate", "drop",
];

/// What the verbs take, as their help says
pub const USAGE: &str = "\
usage: shep kelpie add [<project>]       registers this checkout as a project
       shep kelpie add <issue>           puts an issue on the project's board
       shep kelpie start | pause
       shep kelpie status                every project, or one with -p
       shep kelpie rule [<id> <answer>]
       shep kelpie rework <pr> | adopt <pr>
       shep kelpie gate [<issue>] | drop [<issue>]

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
    let kelpie_home_set = std::env::var_os("KELPIE_HOME").map(PathBuf::from);
    let kelpie_home = kelpie_home_set
        .clone()
        .unwrap_or_else(|| home.join(".kelpie"));
    // A ruling's answer starts after its id and first word.
    let (named, args) = split_project(args, (command == "rule").then_some(2))?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if command == "rule" {
        return answer(shep_home, &kelpie_home, named.as_ref(), &args).await;
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
            let old = ProjectPaths::under(&kelpie_home, &name).settings;
            let place = add::Place {
                checkout: &checkout,
                home: &home,
                old_settings: &old,
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
                _ => control::pause(&client, &name).await,
            }
        }
        ("status", []) if named.is_none() => control::status(&client).await,
        ("status", []) => send("status", None).await,
        ("rework" | "adopt", [number]) => send(command, Some(number)).await,
        ("gate" | "drop", []) => send(command, None).await,
        ("gate" | "drop", [issue]) => send(command, Some(issue)).await,
        _ => Err(USAGE.to_owned()),
    }
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
    control::send(&client, &project, "rule", Some(&params)).await?;
    let (id, said) = params.split_once(' ').unwrap_or((&params, ""));
    Ok(vec![format!("ruling {id} on {project}: {said}")])
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
        assert_eq!(moved("rule -p koji 14 yes"), words("rule -p koji 14 yes"));
        assert_eq!(moved("-p koji runner x"), words("-p koji runner x"));
        assert_eq!(moved("-p"), words("-p"));
    }
}
