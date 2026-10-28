//! Settling a worker's question or a follow-up ruling on the project manager's answer
//!
//! The item must be parked on a question ruling now. Its text answers the
//! ruling as the maintainer's would, led by `PM answered:`, and its
//! escalation puts the ruling to the maintainer with the recommendation
//! added. Anything else is dropped, and the ruling goes out as it stands. An
//! item parked on a follow-up ruling takes an escalation alone through
//! `answer`: its text is dropped, and so is an escalation whose choice is
//! neither `yes` nor `no`. Its own `follow_up` key decides the ruling.

use super::act::{WORDS, plain};
use super::answer::{Call, Decide, How, Settle};
use super::maintainer::Settled;
use crate::board::briefing::ticket_ref;
use crate::board::{READY, TRIAGE};
use crate::ports::Finding;
use crate::runner::ruling::question;
use crate::runner::thread_replies::Settler;
use crate::runner::voice::{ASK_THE_MAINTAINER, BOARD, PROBLEM, asks_the_maintainer, effect_of};
use crate::runner::{Answer as Ruled, RuleError, Runner};
use crate::state::{RulingKind, StateError};
use crate::ticket::TicketId;
use crate::work_item::Phase;

/// How many settled questions `status` keeps
const KEPT: usize = 5;

/// The most options a question the project manager drafted candidates for lists
const MOST_OPTIONS: usize = 3;

/// What leads the text of an answer the project manager gave
const ANSWERED: &str = "PM answered:";

// What a work item is parked on
enum Asked {
    /// A question ruling not yet posted to the maintainer
    On(TicketId),
    /// A question ruling the maintainer was alerted of
    Alerted(TicketId),
    /// Nothing that asks a question
    Not,
}

impl Runner {
    /// Settles the question `settle` names, and says what it did, or why
    /// it was dropped
    ///
    /// # Errors
    ///
    /// [`StateError`] when what it did cannot be saved.
    pub(super) fn settle_question(
        &mut self,
        settle: &Settle,
    ) -> Result<Result<String, String>, StateError> {
        let ticket = settle.item;
        let named = ticket_ref(ticket);
        let id = match self.asked_on(ticket) {
            Asked::On(id) => id,
            Asked::Alerted(id) => {
                return Ok(Err(format!(
                    "answer {named}: ruling {id} already went to the maintainer"
                )));
            }
            Asked::Not => {
                return Ok(Err(format!(
                    "answer {named}: not a work item parked on a question or follow-up ruling"
                )));
            }
        };
        let why = plain(&self.shown(settle.why.clone()), WORDS);
        let (done, outcome, recommends) = match &settle.how {
            How::Text(_) if self.follow_up_ruling(id) => {
                return Ok(Err(format!(
                    "answer {named}: ruling {id} is a follow-up, which the project manager \
                     decides with `follow_up` or passes on with a recommendation, not with text"
                )));
            }
            How::Text(text) => {
                let by = Settler::Pm {
                    answer: text.clone(),
                    why: why.clone(),
                    unread: false,
                };
                match self.rule_by(id, Ruled::Text(format!("{ANSWERED} {text}")), by) {
                    Ok(()) => {}
                    Err(RuleError::State(e)) => return Err(e),
                    Err(e) => return Ok(Err(format!("answer {named}: {e}"))),
                }
                (format!("answered ruling {id} on {named}"), "answered", None)
            }
            How::Escalate(choice)
                if self.follow_up_ruling(id) && !Self::yes_or_no(choice.as_deref()) =>
            {
                return Ok(Err(format!(
                    "answer {named}: ruling {id} is a follow-up, whose recommendation is `yes` or \
                     `no`"
                )));
            }
            How::Escalate(choice) => {
                if let Some((letter, listed)) =
                    choice.as_deref().and_then(|c| self.past_the_list(id, c))
                {
                    return Ok(Err(format!(
                        "answer {named}: choice {letter} is past the {listed} options the \
                         question lists"
                    )));
                }
                let choice = (choice.as_deref()).map(|c| plain(&self.shown(c.to_owned()), WORDS));
                let words = |text: &Option<String>| {
                    (text.as_ref()).map(|t| plain(&self.shown(t.clone()), WORDS))
                };
                let candidates = (settle.candidates.iter())
                    .map(|c| plain(&self.shown(c.clone()), WORDS))
                    .collect();
                let passed = Passed {
                    choice: choice.as_deref(),
                    why: &why,
                    problem: words(&settle.problem),
                    board: words(&settle.board),
                    candidates,
                };
                self.recommend(id, &passed)?;
                (
                    format!("put ruling {id} on {named} to the maintainer"),
                    "passed on",
                    choice,
                )
            }
        };
        let said = recommends
            .as_ref()
            .map_or_else(String::new, |c| format!(", recommending {c}"));
        let line = format!("ruling {id} on {named}: the project manager {outcome} it{said}: {why}");
        let settled = Settled {
            ruling: id,
            issue: ticket,
            how: outcome,
            recommends,
            why,
        };
        self.keep_settled(settled, line)?;
        Ok(Ok(done))
    }

    /// Settles the follow-up ruling `decide` names: its findings are filed
    /// as the maintainer's yes would, or dropped as their no would. Says what
    /// it did, or why it was dropped.
    ///
    /// # Errors
    ///
    /// [`StateError`] when what it did cannot be saved.
    pub(super) fn settle_follow_up(
        &mut self,
        decide: &Decide,
    ) -> Result<Result<String, String>, StateError> {
        let ticket = decide.item;
        let named = ticket_ref(ticket);
        let id = match self.asked_on(ticket) {
            Asked::On(id) if self.follow_up_ruling(id) => id,
            Asked::On(id) => {
                return Ok(Err(format!(
                    "follow_up {named}: ruling {id} is a worker's question, not a follow-up"
                )));
            }
            Asked::Alerted(id) => {
                return Ok(Err(format!(
                    "follow_up {named}: ruling {id} already went to the maintainer"
                )));
            }
            Asked::Not => {
                return Ok(Err(format!(
                    "follow_up {named}: not a work item parked on a follow-up ruling"
                )));
            }
        };
        if self.refused_ruling(id) {
            return Ok(Err(format!(
                "follow_up {named}: ruling {id} asks whether to try the forge again, which is \
                 the maintainer's to decide"
            )));
        }
        let why = plain(&self.shown(decide.why.clone()), WORDS);
        let found = self.findings_of(id);
        let count = match found.len() {
            1 => "1 finding".to_owned(),
            many => format!("{many} findings"),
        };
        let at = plain(&self.shown(found.join(", ")), WORDS);
        let (answer, verb, label) = match decide.call {
            Call::File { unread: false } => (Ruled::Yes, "filing", Some(READY)),
            Call::File { unread: true } => (Ruled::Yes, "filing", Some(TRIAGE)),
            Call::Drop => (Ruled::No(why.clone()), "dropping", None),
        };
        let as_label = label.map_or_else(String::new, |l| format!(" as {l}"));
        let by = Settler::Pm {
            answer: format!(
                "{}, {verb} {count}{as_label}: {at}",
                if label.is_some() { "yes" } else { "no" }
            ),
            why: why.clone(),
            unread: decide.call == Call::File { unread: true },
        };
        match self.rule_by(id, answer, by) {
            Ok(()) => {}
            Err(RuleError::State(e)) => return Err(e),
            Err(e) => return Ok(Err(format!("follow_up {named}: {e}"))),
        }
        let how = if label.is_some() { "filed" } else { "dropped" };
        let line = format!(
            "ruling {id} on {named}: the project manager {how} {count}{as_label}: {at}: {why}"
        );
        let settled = Settled {
            ruling: id,
            issue: ticket,
            how,
            recommends: None,
            why,
        };
        self.keep_settled(settled, line)?;
        Ok(Ok(format!(
            "{verb} {count} of ruling {id} on {named}{as_label}: {at}"
        )))
    }

    // Whether follow-up ruling `id` was raised after the forge refused the
    // findings, and so asks about trying again, not about the findings.
    fn refused_ruling(&self, id: TicketId) -> bool {
        let ruling = self.state.rulings.iter().find(|r| r.id == id);
        ruling.is_some_and(|r| {
            matches!(
                r.kind,
                RulingKind::FollowUp {
                    refused: Some(_),
                    ..
                }
            )
        })
    }

    // The place of each finding of follow-up ruling `id`.
    fn findings_of(&self, id: TicketId) -> Vec<String> {
        let ruling = self.state.rulings.iter().find(|r| r.id == id);
        let Some(RulingKind::FollowUp { findings, .. }) = ruling.map(|r| &r.kind) else {
            return Vec::new();
        };
        let place = |f: &Finding| match f.line {
            0 => f.file.clone(),
            line => format!("{}:{line}", f.file),
        };
        findings.iter().map(place).collect()
    }

    // The question or follow-up ruling the work item for `ticket` is parked
    // on, if any. One the maintainer was alerted of is theirs to answer,
    // however late the project manager's word comes.
    fn asked_on(&self, ticket: TicketId) -> Asked {
        let Some(Phase::Ruling { id }) = self.state.item(ticket).map(|i| i.phase.clone()) else {
            return Asked::Not;
        };
        let held = |kind: &RulingKind| {
            matches!(
                kind,
                RulingKind::Question { .. } | RulingKind::FollowUp { .. }
            )
        };
        match self.state.rulings.iter().find(|r| r.id == id) {
            Some(r) if held(&r.kind) && r.alerted => Asked::Alerted(id),
            Some(r) if held(&r.kind) => Asked::On(id),
            _ => Asked::Not,
        }
    }

    // Whether ruling `id` is a follow-up, which has no worker waiting on text.
    fn follow_up_ruling(&self, id: TicketId) -> bool {
        let ruling = self.state.rulings.iter().find(|r| r.id == id);
        ruling.is_some_and(|r| matches!(r.kind, RulingKind::FollowUp { .. }))
    }

    // Whether `choice` is the yes or no a follow-up ruling asks for.
    fn yes_or_no(choice: Option<&str>) -> bool {
        choice.is_some_and(|c| {
            let c = c.trim();
            c.eq_ignore_ascii_case("yes") || c.eq_ignore_ascii_case("no")
        })
    }

    // The letter `choice` is and how many options the question of ruling `id`
    // lists, when it names a letter past them.
    fn past_the_list(&self, id: TicketId, choice: &str) -> Option<(char, usize)> {
        let ruling = self.state.rulings.iter().find(|r| r.id == id)?;
        let RulingKind::Question { asked, .. } = &ruling.kind else {
            return None;
        };
        let lines: Vec<&str> = asked.lines().collect();
        let listed = lines.len() - list_start(&lines);
        let (letter, at) = letter_of(choice)?;
        (listed > 0 && at >= listed).then_some((letter, listed))
    }

    // Adds the recommendation to ruling `id`, to be posted as it is.
    fn recommend(&mut self, id: TicketId, passed: &Passed<'_>) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let ruling = (next.rulings.iter_mut().find(|r| r.id == id)).expect("found by the caller");
        let issue = ruling.issue.expect("a question parks a work item");
        match &mut ruling.kind {
            RulingKind::Question { asked, .. } => {
                *asked = recommended(asked, passed);
                ruling.question = question(issue, ruling.pull_request, &ruling.kind);
            }
            // Its kind holds no words of the project manager's, so only the
            // question the maintainer reads does. It has no problem or board
            // note of its own to rewrite.
            RulingKind::FollowUp { .. } => {
                let passed = Passed {
                    choice: passed.choice,
                    why: passed.why,
                    problem: None,
                    board: None,
                    candidates: Vec::new(),
                };
                ruling.question = recommended(&ruling.question, &passed);
            }
            _ => {}
        }
        ruling.alerted = false;
        self.save(next)
    }

    // Keeps `settled` for `status`, and `line` in the notes and the log.
    fn keep_settled(&mut self, settled: Settled, line: String) -> Result<(), StateError> {
        self.pm.settled.push(settled);
        let over = self.pm.settled.len().saturating_sub(KEPT);
        self.pm.settled.drain(..over);
        // What it settled is what a later wake must remember.
        self.pm_note(&line);
        self.record(vec![line])
    }
}

// `asked` with the project manager's `choice` on a line of its own, and
// first in the `- ` list that ends it if it has one: a choice it lists moves
// up, and one it does not is added. Its `problem` replaces the worker's
// `Problem:` line, and its `board` note goes under its recommendation.
fn recommended(asked: &str, passed: &Passed<'_>) -> String {
    let Passed {
        choice,
        why,
        problem,
        board,
        candidates,
    } = passed;
    let (choice, why) = (*choice, *why);
    let lines: Vec<&str> = asked.lines().collect();
    let (head, list) = lines.split_at(list_start(&lines));
    let head: Vec<&str> = head
        .iter()
        .copied()
        .filter(|line| problem.is_none() || !line.starts_with(PROBLEM))
        .filter(|line| board.is_none() || !line.starts_with(BOARD))
        .collect();
    let mut placed = false;
    let list: Vec<String> = {
        let mut list: Vec<String> = list.iter().map(|l| (*l).to_owned()).collect();
        let listed = list.len();
        let drafted = draft(&mut list, candidates);
        let named = choice.map(plain_choice);
        let at = match choice.and_then(letter_of) {
            Some((_, at)) if at < listed => Some(at),
            _ => named.and_then(|n| list.iter().position(|line| plain_choice(line) == n)),
        };
        match (choice, at) {
            (Some(_), Some(at)) => {
                let line = list.remove(at);
                list.insert(0, line);
                placed = true;
            }
            (Some(choice), None) if !list.is_empty() && !drafted => {
                list.insert(0, format!("- {choice}"));
                placed = true;
            }
            _ => {}
        }
        list
    };
    let shown = match (choice, list.first()) {
        (Some(_), Some(first)) if placed => {
            Some(effect_of(first.trim_start().trim_start_matches("- ")).0)
        }
        (Some(choice), _) => Some(choice.to_owned()),
        (None, _) => None,
    };
    let mut out: Vec<String> = problem
        .iter()
        .map(|p| format!("{PROBLEM} {p}"))
        .chain(head.iter().map(|l| (*l).to_owned()))
        .collect();
    out.extend(shown.map(|c| format!("PM recommends: {c}")));
    out.extend(board.iter().map(|b| format!("{BOARD} {b}")));
    if !why.is_empty() {
        out.push(format!("PM says: {why}"));
    }
    out.extend(list);
    out.join("\n")
}

// Adds `candidates` to `list`, the `- ` lines a question lists, when it lists
// fewer than two, up to [`MOST_OPTIONS`] and none that repeats a line. A full
// list gives its last candidate up for `Ask the maintainer`. Says whether it
// added any.
fn draft(list: &mut Vec<String>, candidates: &[String]) -> bool {
    let before = list.len();
    if before >= 2 {
        return false;
    }
    for candidate in candidates {
        let text = match asks_the_maintainer(candidate) {
            true => ASK_THE_MAINTAINER,
            false => candidate,
        };
        let seen = list.iter().any(|l| plain_choice(l) == plain_choice(text));
        if seen {
            continue;
        }
        if list.len() < MOST_OPTIONS {
            list.push(format!("- {text}"));
        } else if text == ASK_THE_MAINTAINER {
            // A full list still leaves the product call to the maintainer, in
            // the place of the last candidate drafted.
            list[MOST_OPTIONS - 1] = format!("- {text}");
        }
    }
    list.len() > before
}

// Where the `- ` list that ends `lines` starts
fn list_start(lines: &[&str]) -> usize {
    let listed = |line: &str| line.trim_start().starts_with("- ");
    lines.iter().rposition(|l| !listed(l)).map_or(0, |i| i + 1)
}

// The letter `choice` is, with its place in the list the card shows: `A` is
// the first. Anything but one capital letter is not one.
fn letter_of(choice: &str) -> Option<(char, usize)> {
    let mut chars = choice.trim().chars();
    let letter = chars.next().filter(char::is_ascii_uppercase)?;
    chars
        .next()
        .is_none()
        .then(|| (letter, usize::from(letter as u8 - b'A')))
}

// A choice as it is compared: without its `- `, the effect written after it,
// its backticks or its case.
fn plain_choice(text: &str) -> String {
    let text = text.trim();
    let text = text.strip_prefix("- ").unwrap_or(text);
    let (option, _) = effect_of(text.trim());
    option.trim_matches('`').to_lowercase()
}

// What the project manager adds to a question it passes on
struct Passed<'a> {
    /// The choice it recommends
    choice: Option<&'a str>,
    /// Why
    why: &'a str,
    /// The problem in its own words, to replace the worker's
    problem: Option<String>,
    /// What the board shows of picking an option
    board: Option<String>,
    /// The answers it drafts for a question that lists fewer than two options
    candidates: Vec<String>,
}
