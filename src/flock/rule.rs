//! `shep kelpie rule`: a ruling answered in plain words
//!
//! A ruling's id names its project, so `rule <id> <answer>` needs none. The
//! answer is read by the ruling's kind (see [`read_answer`]) from the
//! project's state file, then sent to its runner as `rule`'s own grammar.
//! With no arguments in a terminal, `rule` lists the rulings waiting and asks
//! which to answer and how.

use std::io::{BufRead, Write};

use crate::relay::Wants;
use crate::runner::{ProjectName, read_answer};
use crate::state::Ruling;
use crate::state::ids::RulingIds;

/// What `shep kelpie rule` takes, as its help says
pub const HELP: &str = "\
usage: shep kelpie rule <id> yes
       shep kelpie rule <id> no <note>
       shep kelpie rule <id> <text>      for a ruling that asks for text
       shep kelpie rule                  lists the rulings waiting, and asks

Quotes are optional. In zsh a note with ?, *, ! or an apostrophe still
needs them: shep kelpie rule 14 no \"it's the wrong flag\". `-p` goes
before the answer: after its first word, and after a `--`, it is the
answer's own.";

/// A ruling to answer: its project, and `rule`'s params
pub type Answering = (ProjectName, String);

/// Reads `args` as a ruling and its answer, or asks for both on `ask` when
/// there are none
///
/// `named` limits it to that project's rulings.
///
/// # Errors
///
/// A message when the ruling is not waiting, the answer does not fit it, or
/// nothing was chosen. Without `ask` and with no arguments, the rulings
/// waiting.
pub fn prepare(
    ids: &RulingIds,
    named: Option<&ProjectName>,
    args: &[&str],
    ask: Option<(&mut dyn BufRead, &mut dyn Write)>,
) -> Result<Answering, String> {
    let mut open = Vec::new();
    let mut unread = Vec::new();
    let mut why = String::new();
    for (project, state) in ids.states() {
        if named.is_some_and(|n| n.as_str() != project) {
            continue;
        }
        match state {
            Ok(state) => open.extend(state.rulings.into_iter().map(|r| (project.clone(), r))),
            Err(e) => {
                why.push_str(&format!("\n{project}'s rulings cannot be read: {e}"));
                unread.push(project);
            }
        }
    }
    let waiting = Waiting {
        open: &open,
        unread: &unread,
    };
    prepare_from(ids, waiting, named, args, ask).map_err(|e| e + &why)
}

// The rulings `rule` can see, and the projects whose rulings it cannot.
#[derive(Clone, Copy)]
struct Waiting<'a> {
    open: &'a [(String, Ruling)],
    unread: &'a [String],
}

fn prepare_from(
    ids: &RulingIds,
    waiting: Waiting<'_>,
    named: Option<&ProjectName>,
    args: &[&str],
    ask: Option<(&mut dyn BufRead, &mut dyn Write)>,
) -> Result<Answering, String> {
    let open = waiting.open;
    let [id, words @ ..] = args else {
        if open.is_empty() {
            return Err("no rulings are waiting".into());
        }
        return match ask {
            Some((input, output)) => pick(open, input, output),
            None => Err(format!(
                "{}\n\n`shep kelpie rule <id> <answer>` answers one",
                listing(open)
            )),
        };
    };
    let id = number(id).ok_or_else(|| format!("{id:?} is not a ruling's id\n\n{HELP}"))?;
    let (project, ruling) = find(ids, waiting, named, id)?;
    if words.is_empty() {
        return Err(format!("ruling {id} takes {}", forms(&ruling)));
    }
    Ok((project, params(&ruling, &words.join(" "))?))
}

// Ruling `id` among those waiting, on `named` or the project it was given
// to. An id with no claim may be on a project whose rulings cannot be
// read, so then only `named` finds it.
fn find(
    ids: &RulingIds,
    waiting: Waiting<'_>,
    named: Option<&ProjectName>,
    id: u64,
) -> Result<(ProjectName, Ruling), String> {
    let Waiting { open, unread } = waiting;
    // `open` holds only `named`'s rulings when it is given.
    let owner = named
        .map(|n| n.as_str().to_owned())
        .or_else(|| ids.owner(id));
    if owner.is_none() && !unread.is_empty() {
        return Err(format!(
            "ruling {id} does not say which project it is on, and {} rulings cannot be read, \
             so name the project: `shep kelpie rule -p <project> {id} <answer>`",
            unread
                .iter()
                .map(|p| format!("{p}'s"))
                .collect::<Vec<_>>()
                .join(" and ")
        ));
    }
    let on: Vec<&(String, Ruling)> = open
        .iter()
        .filter(|(project, r)| r.id == id && owner.as_ref().is_none_or(|o| o == project))
        .collect();
    match on.as_slice() {
        [(project, ruling)] => {
            let project = ProjectName::try_from(project.as_str()).map_err(|e| e.to_string())?;
            Ok((project, ruling.clone()))
        }
        [] => Err(format!(
            "no ruling {id} is waiting: `shep kelpie rule` lists those that are"
        )),
        more => Err(format!(
            "ruling {id} is waiting on {}, so name one: `shep kelpie rule -p <project> {id} <answer>`",
            more.iter()
                .map(|(project, _)| project.as_str())
                .collect::<Vec<_>>()
                .join(" and ")
        )),
    }
}

// `words` as `rule`'s params for `ruling`.
fn params(ruling: &Ruling, words: &str) -> Result<String, String> {
    let answer = read_answer(Wants::of(&ruling.kind), words);
    let answer = answer.map_err(|why| {
        let id = ruling.id;
        format!(
            "ruling {id} was not answered: {why}. It takes {}",
            forms(ruling)
        )
    })?;
    Ok(answer.params(ruling.id))
}

// The commands that answer `ruling`.
fn forms(ruling: &Ruling) -> String {
    let id = ruling.id;
    match Wants::of(&ruling.kind) {
        Wants::Answer => format!("your answer to the worker: `shep kelpie rule {id} <text>`"),
        Wants::YesOrNo => {
            format!("`shep kelpie rule {id} yes` or `shep kelpie rule {id} no <note>`")
        }
    }
}

fn listing(open: &[(String, Ruling)]) -> String {
    let each = open.iter().map(|(project, ruling)| {
        let lines = ruling.question.lines();
        let indented = lines.map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("    {l}")
            }
        });
        let question = indented.collect::<Vec<_>>().join("\n");
        format!("Ruling {} on {project}:\n{question}", ruling.id)
    });
    each.collect::<Vec<_>>().join("\n\n")
}

// Lists `open`, then asks which ruling and the answer, until one fits. An
// empty answer or the input's end sends nothing.
fn pick(
    open: &[(String, Ruling)],
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<Answering, String> {
    let nothing = || "nothing was sent".to_owned();
    say(output, &format!("{}\n\n", listing(open)));
    let mut read = |output: &mut dyn Write, prompt: &str| {
        say(output, prompt);
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim().to_owned()),
        }
    };
    let (project, ruling) = loop {
        let only = (open.len() == 1).then(|| open[0].1.id);
        let prompt = match only {
            Some(id) => format!("Which ruling? [{id}] "),
            None => "Which ruling? ".to_owned(),
        };
        let line = read(output, &prompt).ok_or_else(nothing)?;
        let (id, rest) = line.split_once(' ').unwrap_or((&line, ""));
        let rest = rest.trim();
        let id = match (number(id), only) {
            (Some(id), _) => id,
            (None, Some(only)) if line.is_empty() => only,
            _ => {
                say(output, "Type the id of one listed.\n");
                continue;
            }
        };
        // What follows the id names a project when one holds it, and is
        // the answer otherwise.
        let with_id = open.iter().filter(|(_, r)| r.id == id);
        let on_project: Vec<_> = with_id.clone().filter(|(p, _)| p == rest).collect();
        let (chosen, words): (Vec<_>, &str) = match on_project.as_slice() {
            [_] => (on_project, ""),
            _ => (with_id.collect(), rest),
        };
        match chosen.as_slice() {
            [(project, ruling)] if words.is_empty() => break (project.clone(), ruling.clone()),
            [(project, ruling)] => match params(ruling, words) {
                Ok(params) => return answering(project, params),
                Err(why) => {
                    say(output, &format!("{why}\n"));
                    break (project.clone(), ruling.clone());
                }
            },
            [] => say(output, &format!("No ruling {id} is listed.\n")),
            _ => say(
                output,
                &format!("Ruling {id} is on more than one project: type `{id} <project>`.\n"),
            ),
        }
    };
    let prompt = match Wants::of(&ruling.kind) {
        Wants::Answer => "Your answer: ",
        Wants::YesOrNo => "yes, or no <note>: ",
    };
    loop {
        let words = read(output, prompt).ok_or_else(nothing)?;
        if words.is_empty() {
            return Err(nothing());
        }
        match params(&ruling, &words) {
            Ok(params) => return answering(&project, params),
            Err(why) => say(output, &format!("{why}\n")),
        }
    }
}

fn answering(project: &str, params: String) -> Result<Answering, String> {
    let project = ProjectName::try_from(project).map_err(|e| e.to_string())?;
    Ok((project, params))
}

// A prompt or a line for the terminal. One that cannot be written loses
// nothing: the answer is still read.
fn say(output: &mut dyn Write, text: &str) {
    let _ = write!(output, "{text}");
    let _ = output.flush();
}

// Digits only, so `#14` is refused rather than read as 14.
fn number(text: &str) -> Option<u64> {
    let n = text.parse::<u64>().ok()?;
    (n > 0 && text.bytes().all(|b| b.is_ascii_digit())).then_some(n)
}

#[cfg(test)]
mod tests;
