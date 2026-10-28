//! What the representative words a triage comment from
//!
//! Kelpie's code drafts the facts and the representative puts them into
//! words. How a comment closes depends on who reads it: the maintainer on their
//! own issue, or anyone else, whose comment mentions the maintainer where it
//! hands the issue over.

use super::ready::is_issue_body;
use super::research::{Addressed, Found};
use crate::local_paths::Leak;

/// Who reads the comment, which settles how it closes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reader {
    /// The maintainer, on their own issue
    Maintainer,
    /// Someone else, and a line under the words mentions the maintainer
    Mentioned,
    /// Someone else, with no mention: a comment that asks, or no login to name
    Other,
}

/// What the comment is for
#[derive(Debug, Clone, Copy)]
pub(super) struct Wording {
    /// The check passes the rewrite, which makes the issue ready for the board
    pub(super) ready: bool,
    /// The comment asks its author the open questions
    pub(super) ask: bool,
    /// The pass reads the author's answers
    pub(super) answers: bool,
    /// The pass reads a body the author's own session rewrote
    pub(super) local: bool,
    /// The pass writes the settled answers under the author's text
    pub(super) settled: bool,
    /// The pass rewrites the maintainer's issue, which the check still refuses
    pub(super) rewrite: bool,
    /// A triage label put on by hand stands, so kelpie labels nothing
    pub(super) stood: bool,
    /// The research named no priority, so kelpie set the fallback itself
    pub(super) fallback: bool,
}

// What the representative words the comment of a duplicate from: the other
// item as `#n`, what it is, why the research says it covers this issue, and
// the close that follows the comment
pub(super) fn duplicate_facts(named: &Addressed) -> String {
    let why = named.why.trim();
    let reason = match why.is_empty() {
        true => String::new(),
        false => format!(": {why}"),
    };
    let mut facts = String::from("Kelpie read the issue against the repo's main branch.\n");
    facts.push_str(&format!(
        "\n#{} is an open {} that already covers it{reason}.\n",
        named.number,
        named.item.word()
    ));
    facts.push_str(&format!(
        "\nKelpie closes this issue as a duplicate of #{}, and puts no labels on it.\n",
        named.number
    ));
    facts
}

// The facts the comment is worded from: labels, related issues, files, tests,
// criteria, questions, and how the pass ends (ready, the ask, the answers
// round, or a rewritten body), told to the reader it is for
pub(super) fn facts(found: &Found, labels: &[String], wording: Wording, reader: Reader) -> String {
    let Wording {
        ready,
        ask,
        settled,
        fallback,
        ..
    } = wording;
    let mut facts = String::from("Kelpie read the issue against the repo's main branch.\n");
    if !labels.is_empty() {
        let when = match ask {
            true => " when this triage ends",
            false => "",
        };
        facts.push_str(&format!(
            "\nKelpie labels it: {}{when}.\n",
            labels.join(", ")
        ));
    }
    if let (true, Some(priority)) = (fallback, found.priority) {
        facts.push_str(&format!(
            "\nThe research ranked the issue nowhere, so kelpie set priority {priority} itself. \
             Say kelpie set it and the maintainer can change it.\n"
        ));
    }
    if !found.related.is_empty() {
        let related: Vec<String> = found.related.iter().map(|(n, _)| format!("#{n}")).collect();
        facts.push_str(&format!("\nRelated open issues: {}.\n", related.join(", ")));
    }
    let lists = [
        ("Files the work would touch", &found.files, "`"),
        ("Tests that would pin it", &found.tests, "`"),
        ("Draft acceptance criteria", &found.criteria, ""),
    ];
    for (heading, items, quote) in lists {
        if items.is_empty() {
            continue;
        }
        facts.push_str(&format!("\n{heading}:\n"));
        for item in items {
            facts.push_str(&format!("- {quote}{item}{quote}\n"));
        }
    }
    match found.open.is_empty() {
        true => facts.push_str("\nThere are no open questions.\n"),
        false => {
            facts.push_str("\nQuestions only the author can answer, each with the answer the research recommends:\n");
            for (i, q) in found.open.iter().enumerate() {
                let n = i + 1;
                let (question, recommended) = (q.question.trim(), q.recommended.trim());
                match recommended.is_empty() {
                    true => facts.push_str(&format!("{n}. {question}\n")),
                    false => {
                        facts.push_str(&format!("{n}. {question} (recommended: {recommended})\n"))
                    }
                }
            }
        }
    }
    match ready {
        true => {
            facts.push_str(
                "\nThe issue as kelpie rewrites it passes its issue check: it rewrites the issue \
                 body from this report and labels it `ready-for-agent` for its board.",
            );
            if let Some(priority) = found.priority {
                facts.push_str(&format!(
                    " Say the issue is ready for an agent, at priority {priority}."
                ));
            }
            facts.push('\n');
        }
        false => {
            if settled {
                facts.push_str(
                    "\nKelpie writes the answers its author gave under the author's own text in \
                     this issue's body, and never in place of it.\n",
                );
            }
            facts.push_str(&closing(found, wording, reader));
        }
    }
    facts
}

// How a comment that is not ready ends: what happens next, and who is told
fn closing(found: &Found, wording: Wording, reader: Reader) -> String {
    let Wording {
        ask,
        answers,
        local,
        rewrite,
        stood,
        fallback,
        ..
    } = wording;
    let next = match (ask, answers || local) {
        (true, _) => {
            "\nKelpie posts these questions under your words, each with the answer the research \
             recommends, and under them a collapsed prompt the author can paste into an agent \
             session of their own. The author answers them all in one comment, whatever they \
             write, or rewrites the issue body from their own session. Kelpie then reads the \
             issue again, and what is still open goes to the maintainer. With no reply the \
             maintainer triages the issue as it stands.\n"
        }
        (false, true) => {
            "\nThis is the last round of questions: what is still open goes to the maintainer.\n"
        }
        (false, false) => "\nThe maintainer triages it next.\n",
    };
    match reader {
        Reader::Maintainer => maintainers(found, ask, stood, rewrite, fallback),
        Reader::Mentioned => format!(
            "{next}Kelpie adds a line under your words that mentions the maintainer, so they are \
             notified. Promise nothing else of them.\n"
        ),
        Reader::Other => next.to_owned(),
    }
}

// What the representative is told of a pass that does not make the
// maintainer's own issue ready: it speaks to them, says what is theirs to
// settle, and recommends a priority
fn maintainers(found: &Found, ask: bool, stood: bool, rewrite: bool, fallback: bool) -> String {
    let mut theirs = Vec::new();
    if !found.open.is_empty() {
        theirs.push("the questions above");
    }
    if found.kind.is_none() && found.work.is_none() {
        theirs.push("the type");
    }
    if found.priority.is_none() {
        theirs.push("the priority");
    }
    if !rewrite && !found.body.as_deref().is_some_and(is_issue_body) {
        theirs.push("an agent-ready body that names `Touches:` and has acceptance criteria");
    }
    let ending = match (ask, stood) {
        (true, _) => {
            "Kelpie posts the questions under your words, each with the answer the research \
             recommends, and a collapsed prompt for an agent session of their own, and labels \
             nothing yet: their answers decide where it goes, and with no reply it ends at \
             `needs-triage`, waiting on them."
        }
        (false, true) => {
            "A triage label they put on by hand stands, so kelpie labels it no further."
        }
        (false, false) if rewrite => {
            "Kelpie's issue check still refuses the rewrite, so kelpie writes it under their \
             own text in the body, the issue ends at `needs-triage`, waiting on them, and \
             kelpie says what the check refused."
        }
        (false, false) => "It ends at `needs-triage`, waiting on them.",
    };
    let mut said = format!(
        "\nThe author is the maintainer: speak to them as \"you\". Kelpie did not make it ready \
         for the board, so never call it ready or ready to be assigned. {ending}"
    );
    if !theirs.is_empty() {
        said.push_str(&format!(
            " Say what is still theirs to settle: {}.",
            theirs.join(", ")
        ));
    }
    if let Some(priority) = found.priority.filter(|_| !fallback) {
        said.push_str(&format!(" Say kelpie would recommend priority {priority}."));
    }
    said.push('\n');
    said
}

pub(super) fn refused_facts(facts: &str, leak: Leak) -> String {
    format!("{facts}\nThe last wording named {leak}. Leave it out.\n")
}
