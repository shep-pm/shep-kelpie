//! `shep kelpie rule`: a ruling answered in plain words
//!
//! A ruling is named by the number of its issue or pull request: `rule 427`,
//! `rule #427` and `rule koji#427`. The answer is read by the ruling's kind
//! (see [`read_answer`]) from the project's state file, then sent to its
//! runner as `rule`'s own grammar. With no arguments in a terminal, `rule`
//! lists the rulings waiting and asks which to answer and how.

use std::io::{BufRead, Write};

use crate::home::ProjectName;
use crate::runner::{Wants, read_answer};
use crate::state::Held;
use crate::state::Projects;
use crate::state::Ruling;
use crate::state::RulingKind;
use crate::ticket::TicketId;

/// What `shep kelpie rule` takes, as its help says
pub const HELP: &str = "\
usage: shep kelpie rule <ruling> yes
       shep kelpie rule <ruling> no <note>
       shep kelpie rule <ruling> rework <note>   for a merge ruling
       shep kelpie rule <ruling> <text>          for a ruling that asks for text
       shep kelpie rule                          lists the rulings waiting, and asks

A <ruling> is its pull request's number, or its ticket's id before there is
one: 427, #427, koji#427, k12 or koji#k12. A number that two projects have a
ruling on needs the project.

Quotes are optional. In zsh a note with ?, *, ! or an apostrophe still
needs them: shep kelpie rule 427 no \"it's the wrong flag\". `-p` goes
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
    projects: &Projects,
    named: Option<&ProjectName>,
    args: &[&str],
    ask: Option<(&mut dyn BufRead, &mut dyn Write)>,
) -> Result<Answering, String> {
    prepare_in(projects, named, None, args, ask)
}

/// [`prepare`] from a clone of `here`, whose rulings a number is looked for
/// among first
///
/// # Errors
///
/// As [`prepare`].
pub fn prepare_in(
    projects: &Projects,
    named: Option<&ProjectName>,
    here: Option<&ProjectName>,
    args: &[&str],
    ask: Option<(&mut dyn BufRead, &mut dyn Write)>,
) -> Result<Answering, String> {
    let mut open = Vec::new();
    let mut unread = Vec::new();
    let mut why = String::new();
    for (project, state) in projects.states() {
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
        here,
    };
    prepare_from(waiting, named, args, ask).map_err(|e| e + &why)
}

// The rulings `rule` can see, the projects whose rulings it cannot, and the
// project of the clone it runs in.
#[derive(Clone, Copy)]
struct Waiting<'a> {
    open: &'a [(String, Ruling)],
    unread: &'a [String],
    here: Option<&'a ProjectName>,
}

fn prepare_from(
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
                "{}\n\n`shep kelpie rule <ruling> <answer>` answers one",
                listing(open)
            )),
        };
    };
    let reference =
        Reference::of(id).ok_or_else(|| format!("{id:?} is not a ruling's id\n\n{HELP}"))?;
    let (project, ruling) = locate(waiting, named, &reference)?;
    if words.is_empty() {
        return Err(format!("ruling {} takes {}", ruling.id, forms(&ruling)));
    }
    Ok((project, params(&ruling, &words.join(" "))?))
}

/// Whether `text` names a ruling with no project before it, so the clone it is
/// typed in can say which project's
pub fn names_ruling(text: &str) -> bool {
    Reference::of(text).is_some_and(|r| r.project.is_none())
}

// How an argument names a ruling: by its pull request's number, or its
// ticket's id before there is one, with a `#` or a project before it or not.
struct Reference {
    project: Option<ProjectName>,
    ticket: TicketId,
}

impl Reference {
    // `14`, `#14`, `koji#14` or `k12`.
    fn of(text: &str) -> Option<Self> {
        let Some((project, digits)) = text.rsplit_once('#') else {
            let ticket = ticket(text)?;
            return Some(Self {
                project: None,
                ticket,
            });
        };
        let project = match project {
            "" => None,
            name => Some(ProjectName::try_from(name).ok()?),
        };
        Some(Self {
            project,
            ticket: ticket(digits)?,
        })
    }

    // As typed, without the project when it was not typed.
    fn label(&self) -> String {
        match self.project.as_ref().map(ProjectName::as_str) {
            Some(name) => qualified(name, self.ticket),
            None => self.ticket.shown(),
        }
    }
}

// A ticket's id after a project's name, the way `rule` reads it back:
// `shep-kelpie#427`, `shep-kelpie#k12`. `Reference::of` sees a project only
// where the text has a `#`, and a local id carries none of its own.
fn qualified(project: &str, ticket: TicketId) -> String {
    format!("{project}#{ticket}")
}

// The ruling `reference` names among those waiting.
fn locate(
    waiting: Waiting<'_>,
    named: Option<&ProjectName>,
    reference: &Reference,
) -> Result<(ProjectName, Ruling), String> {
    let Reference { project, ticket } = reference;
    let shown = ticket.shown();
    let on = match (project, named) {
        (Some(p), Some(n)) if p != n => {
            return Err(format!(
                "`-p {n}` and `{}` name different projects",
                reference.label()
            ));
        }
        (Some(p), _) | (None, Some(p)) => Some(p.as_str()),
        (None, None) => None,
    };
    // A ruling answers to its ticket, or to its pull request. Only a GitHub
    // ticket has a number, so a local id skips the pull request side: `None ==
    // None` would answer it to every ruling with no pull request yet.
    let github = ticket.github();
    let about =
        |r: &Ruling| r.issue == Some(*ticket) || github.is_some_and(|n| r.pull_request == Some(n));
    let mut found: Vec<&(String, Ruling)> = (waiting.open.iter())
        .filter(|(p, r)| about(r) && on.is_none_or(|on| on == p))
        .collect();
    // A number names the ruling that took it when the file was renamed, not
    // one an older kelpie left beside it under its counter id.
    let named: Vec<&str> = (found.iter())
        .filter(|(_, r)| !r.is_left_over())
        .map(|(p, _)| p.as_str())
        .collect();
    found.retain(|(p, r)| !r.is_left_over() || !named.contains(&p.as_str()));
    // A clone's own project answers before the others do.
    let here = waiting.here.map(ProjectName::as_str);
    let narrowed = on.is_none() && found.iter().any(|(p, _)| Some(p.as_str()) == here);
    if narrowed {
        found.retain(|(p, _)| Some(p.as_str()) == here);
    }
    // A project whose rulings cannot be read may have a ruling on the number
    // too, so with none named, nor a clone's own to answer first, the ones
    // that can be read are not all there are.
    let unread = (on.is_none() && !narrowed && !waiting.unread.is_empty()).then(|| {
        (waiting.unread.iter())
            .map(|p| format!("{p}'s"))
            .collect::<Vec<_>>()
            .join(" and ")
    });
    let name_one = format!(
        "so name the project: `shep kelpie rule {} <answer>`",
        qualified("<project>", *ticket)
    );
    match (found.as_slice(), unread) {
        ([(project, _)], Some(unread)) => Err(format!(
            "{shown} has a ruling on {project}, and {unread} rulings cannot be read, {name_one}"
        )),
        ([(project, ruling)], None) => {
            let project = ProjectName::try_from(project.as_str()).map_err(|e| e.to_string())?;
            Ok((project, ruling.clone()))
        }
        ([], Some(unread)) => Err(format!(
            "no ruling on {shown} is waiting, and {unread} rulings cannot be read, {name_one}"
        )),
        ([], None) => Err(format!(
            "no ruling on {} is waiting: a ruling is named by its pull request's number or \
             its ticket's id, and `shep kelpie rule` lists those that are waiting",
            reference.label()
        )),
        (more, _) => {
            let mut projects: Vec<&str> = more.iter().map(|(p, _)| p.as_str()).collect();
            projects.sort_unstable();
            projects.dedup();
            if let [project] = projects.as_slice() {
                return Err(format!(
                    "{} has {} rulings on {project}, which no answer can tell apart",
                    reference.label(),
                    more.len()
                ));
            }
            Err(format!(
                "{shown} has a ruling on {}, so name one: `shep kelpie rule {} <answer>`",
                projects.join(" and "),
                qualified("<project>", *ticket)
            ))
        }
    }
}

// `words` as `rule`'s params for `ruling`.
fn params(ruling: &Ruling, words: &str) -> Result<String, String> {
    let answer =
        read_answer(Wants::of(&ruling.kind), words).map_err(|why| refused(ruling, words, &why))?;
    Ok(answer.params(ruling.id))
}

// Why `ruling` refuses `words`: what it takes, and where the project flag goes
// when the answer holds one, since after the answer's first word a `-p` is one
// of its words.
fn refused(ruling: &Ruling, words: &str, why: &str) -> String {
    let id = ruling.id;
    let takes = format!(
        "ruling {id} was not answered: {why}. It takes {}",
        forms(ruling)
    );
    match holds_project_flag(words) {
        false => takes,
        true => format!(
            "{takes}. `-p` goes before the answer: `shep kelpie -p <project> rule {id} ...`"
        ),
    }
}

// Whether `words` holds the project flag as one of its own words
fn holds_project_flag(words: &str) -> bool {
    (words.split_whitespace()).any(|word| word == "-p" || word == "--project")
}

// The commands that answer `ruling`.
fn forms(ruling: &Ruling) -> String {
    let id = ruling.id;
    match Wants::of(&ruling.kind) {
        Wants::Answer => format!("your answer to the worker: `shep kelpie rule {id} <text>`"),
        Wants::Merge => format!(
            "{}, `shep kelpie rule {id} no <note>` or `shep kelpie rule {id} rework <note>`",
            yes(ruling)
        ),
        Wants::Trust | Wants::YesOrNo => {
            format!("{} or `shep kelpie rule {id} no <note>`", yes(ruling))
        }
    }
}

// The yes that answers `ruling`. A contribution's security ruling trusts only
// the head its yes names, so its form names that head.
fn yes(ruling: &Ruling) -> String {
    let id = ruling.id;
    match &ruling.kind {
        RulingKind::Contribution(Held::Security { head, .. }) => {
            format!(
                "`shep kelpie rule {id} yes {}`",
                head.get(..7).unwrap_or(head)
            )
        }
        _ => format!("`shep kelpie rule {id} yes`"),
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
        let id = match (ticket(id), only) {
            (Some(id), _) => id,
            (None, Some(only)) if line.is_empty() => only,
            _ => {
                say(output, "Type the number of one listed.\n");
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
        Wants::Merge => "yes, no <note> or rework <note>: ",
        Wants::Trust => "yes <head>, or no <note>: ",
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

// A ticket's id as written: `14` or `k12`, so `#14` on its own is refused
// rather than read as 14.
fn ticket(text: &str) -> Option<TicketId> {
    text.parse().ok()
}

#[cfg(test)]
mod tests;
