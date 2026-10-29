//! A ruling's comment in one short format
//!
//! Every kind renders the same way: a title, what happened, a table of what
//! each option does, the project manager's words, and short ways to answer.
//! The same options are what a reaction or a ticked box on the comment is
//! read against ([`thumbs`] and [`ticked`]), so what the comment offers and
//! what kelpie accepts cannot drift apart. The project manager's words are
//! its pick and why, where it gave any.

use super::super::contributions::{
    ADOPT_ANYWAY, CLOSE_WITH_THANKS, KEEP_WAITING, LEAVE_OPEN, flagged_lines,
};
use super::super::gate::short;
use super::super::ruling::{needs_pass, stuck_comment};
use super::super::words::{NO_NOTE, Wants};
use super::super::{Action, Answer};
use super::merge_notes;
use crate::state::{Held, Ruling, RulingKind, Stuck};
use crate::ticket::TicketId;
use crate::work_item::Phase;

/// What the project manager's `no` is in a table, when it has no more to say
const SENDS_BACK: &str = "Sends the worker back with a note, and the review reads its change again";

/// What picking an option of a worker's question does, when the worker wrote no effect
const SENDS_ANSWER: &str = "Sends the worker this answer";

/// The candidate answer that leaves a product call to the maintainer
///
/// The project manager can pick it, and that pick decides nothing: it is no
/// answer to send the worker.
pub(in crate::runner) const ASK_THE_MAINTAINER: &str = "Ask the maintainer";

/// What picking [`ASK_THE_MAINTAINER`] does, in a table
const LEAVES_IT: &str = "Sends the worker nothing: the maintainer answers in their own words";

/// What a no does to a reviewer's test that still fails
const ENDS_THE_CHECK: &str = "Sends the worker your note, ends the check and the review goes on";

/// What a no does to a work item that cannot go on
const STOPS: &str = "Stops the work item, keeping its branch and pull request on the forge";

/// What a no does to a work item over its budget
const RAISES: &str = "Sends the worker your note, and raises the work item's cap as a yes does";
const RAISES_AND_REVIEWS: &str = "Sends the worker your note, raises the work item's cap as a yes does, and has the pull request reviewed again";

/// One way to answer a ruling, as its table lists it
struct Choice {
    /// What the table calls it
    name: String,
    /// What the table says answering it does
    does: String,
    /// The answer ticking it gives
    answer: Answer,
}

/// What a ruling's comment says before it says how to answer
struct Card {
    /// What is wrong, in plain words, where the worker or the project manager
    /// wrote it: the comment opens with it
    problem: Option<String>,
    title: String,
    /// What happened, when the title leaves something unsaid
    facts: Option<String>,
    /// The options a ruling lists, A first
    choices: Vec<Choice>,
    /// What the board shows of picking an option, in the project manager's words
    board: Option<String>,
}

/// The comment a ruling is posted as, through the App
///
/// `mention` is the login whose reaction, tick or reply answers it. With none
/// the comment says only that the terminal does.
pub(super) fn ruling_comment(mention: Option<&str>, ruling: &Ruling) -> String {
    let card = Card::for_ruling(ruling);
    let mut blocks = card.opening();
    blocks.push(table(&card.choices, &ruling.kind));
    blocks.extend(card.on_the_board());
    blocks.extend(pm_words(ruling, &card.choices));
    blocks.push(match mention {
        Some(login) => answering(login, ruling.number(), &ruling.kind, &card.choices),
        None => unmentioned(ruling.number(), &ruling.kind, &card.choices),
    });
    blocks.join("\n\n")
}

/// What the project manager is shown of `ruling`: its title, what happened
/// and the table of its options, as the comment opens, and how many options
/// there are
pub(in crate::runner) fn brief(ruling: &Ruling) -> (String, usize) {
    let card = Card::for_ruling(ruling);
    let mut blocks = card.opening();
    blocks.push(table(&card.choices, &ruling.kind));
    blocks.extend(card.on_the_board());
    (blocks.join("\n\n"), card.choices.len())
}

/// A ruling's comment once it is settled, to edit over the one it was posted as
///
/// `line` says how it was settled and that nothing waits on the maintainer.
/// The question stays under it, with the project manager's words, and no option, tick,
/// reaction or reply is offered.
pub(in crate::runner) fn settled_comment(ruling: &Ruling, line: &str) -> String {
    let card = Card::for_ruling(ruling);
    let mut blocks = vec![line.to_owned()];
    blocks.extend(card.opening());
    blocks.extend(pm_words(ruling, &card.choices));
    blocks.join("\n\n")
}

/// What each option of a ruling of `kind` does when kelpie words nothing more,
/// in the order the card lists them
pub(in crate::runner) fn phrases(kind: &RulingKind) -> Vec<String> {
    (Card::of(kind).choices.into_iter())
        .map(|c| c.does)
        .collect()
}

/// The option key a `answer` is to a ruling of `kind`, if it is one of its options
///
/// A no or a rework is the option whatever its note says, and an answer in
/// words is the option that says the same.
pub(in crate::runner) fn chosen(kind: &RulingKind, answer: &Answer) -> Option<char> {
    let same = |a: &Answer, b: &Answer| match (a, b) {
        (Answer::Yes, Answer::Yes)
        | (Answer::No(_), Answer::No(_))
        | (Answer::Rework(_), Answer::Rework(_)) => true,
        (Answer::Do(a), Answer::Do(b)) => a == b,
        (Answer::Text(a), Answer::Text(b)) => a.trim() == b.trim(),
        (Answer::Trust(a), Answer::Trust(b)) => a == b,
        _ => false,
    };
    let at = Card::of(kind)
        .choices
        .iter()
        .position(|c| same(&c.answer, answer))?;
    ('A'..='Z').nth(at)
}

/// The answer option `key` of a ruling of `kind` gives, a no or a rework
/// carrying `note`; none when the ruling lists no such option
pub(in crate::runner) fn answer_for(kind: &RulingKind, key: char, note: &str) -> Option<Answer> {
    let at = ('A'..='Z').position(|letter| letter == key)?;
    let choice = Card::of(kind).choices.into_iter().nth(at)?;
    if leaves_it(&choice) {
        return None;
    }
    Some(match choice.answer {
        Answer::No(_) => Answer::No(note.to_owned()),
        Answer::Rework(_) => Answer::Rework(note.to_owned()),
        other => other,
    })
}

/// What option `key` of a ruling of `kind` is called in its table, if it is listed
pub(in crate::runner) fn named(kind: &RulingKind, key: char) -> Option<String> {
    let at = ('A'..='Z').position(|letter| letter == key)?;
    Card::of(kind).choices.into_iter().nth(at).map(|c| c.name)
}

/// The answer a 👍 (`up`) or a 👎 gives to a ruling of `kind`, if it takes one
pub(in crate::runner) fn thumbs(kind: &RulingKind, up: bool) -> Option<Answer> {
    let card = Card::of(kind);
    match (Wants::of(kind), up) {
        (Wants::Answer, _) => None,
        (_, true) => card.choices.into_iter().next().map(|c| c.answer),
        (_, false) => (card.choices.len() > 1).then(|| Answer::No(NO_NOTE.to_owned())),
    }
}

/// The answer the one box ticked in `body` gives to a ruling of `kind`
///
/// `None` when no box is ticked, when several are, or when the ruling lists
/// no boxes to tick.
pub(in crate::runner) fn ticked(kind: &RulingKind, body: &str) -> Option<Answer> {
    let card = Card::of(kind);
    if !boxes(kind) {
        return None;
    }
    // Only the boxes kelpie wrote count: the lines after the one that says to
    // tick, not a task line in the facts a worker's question carries.
    let lines: Vec<&str> = body.lines().collect();
    let offered = lines
        .iter()
        .rposition(|line| line.trim_end().ends_with("ick one:"))?;
    let mut ticked = lines[offered + 1..].iter().filter_map(|line| {
        let key = line.trim().strip_prefix("- [")?;
        let (mark, key) = key.split_once("] ")?;
        matches!(mark, "x" | "X").then(|| key.trim().to_owned())
    });
    let (Some(key), None) = (ticked.next(), ticked.next()) else {
        return None;
    };
    let at = ('A'..='Z').position(|letter| key == letter.to_string())?;
    (card.choices.into_iter().nth(at))
        .filter(|c| !leaves_it(c))
        .map(|c| c.answer)
}

// Whether the comment lists boxes to tick: a merge ruling's and a question's
// options, where a yes or a no is a reaction
fn boxes(kind: &RulingKind) -> bool {
    match Wants::of(kind) {
        Wants::Merge => true,
        Wants::Answer => !Card::of(kind).choices.is_empty(),
        Wants::YesOrNo => false,
    }
}

impl Card {
    // The card of `ruling`: its kind's, with the effect kelpie worded when it
    // raised the ruling where it worded one
    fn for_ruling(ruling: &Ruling) -> Self {
        let mut card = Self::of(&ruling.kind);
        for (choice, effect) in card.choices.iter_mut().zip(&ruling.effects) {
            if !effect.trim().is_empty() {
                choice.does.clone_from(effect);
            }
        }
        card
    }

    // What the comment opens with: the problem, then the title and the facts
    fn opening(&self) -> Vec<String> {
        let mut blocks: Vec<String> = self.problem.clone().into_iter().collect();
        blocks.push(format!("### {}", self.title));
        blocks.extend(self.facts.clone());
        blocks
    }

    // The project manager's note on what the board shows, after the table
    fn on_the_board(&self) -> Option<String> {
        let note = self.board.as_ref()?;
        Some(format!("**On the board**\n{note}"))
    }

    fn of(kind: &RulingKind) -> Self {
        match kind {
            RulingKind::Merge { head, .. } => {
                let facts = merge_notes(kind).join(" ");
                let mut choices = vec![
                    choice("Merge", "Merges it into main", Answer::Yes),
                    choice(
                        "Send back",
                        if needs_pass(kind) {
                            "Sends the worker back for a change the whole review reads again"
                        } else {
                            "Sends the worker back for a fix that goes to CI and back to you"
                        },
                        Answer::No(NO_NOTE.to_owned()),
                    ),
                ];
                if !needs_pass(kind) {
                    let does = "Sends the worker back for a change the whole review reads again";
                    choices.push(choice("Rework", does, Answer::Rework(NO_NOTE.to_owned())));
                }
                Self {
                    problem: None,
                    title: format!("Merge this pull request at {} into main?", short(head)),
                    facts: (!facts.is_empty()).then_some(facts),
                    choices,
                    board: None,
                }
            }
            RulingKind::Stuck(reason) => {
                let (title, yes, no) = stuck(reason);
                let mut choices = yes_no(yes, no);
                choices.extend(acting(reason));
                Self {
                    problem: None,
                    title: title.to_owned(),
                    facts: (!matches!(reason, Stuck::AdoptFailed { .. }))
                        .then(|| stuck_comment(reason)),
                    choices,
                    board: None,
                }
            }
            RulingKind::Question { asked, .. } => {
                let Asking {
                    problem,
                    stem,
                    options,
                    board,
                } = split_question(asked);
                let (title, facts) = match stem.split_once('\n') {
                    Some((title, rest)) => (title.trim(), rest.trim()),
                    None => (stem.as_str(), ""),
                };
                let title = match title.is_empty() {
                    true => "The worker asks a question",
                    false => title,
                };
                Self {
                    problem,
                    title: title.to_owned(),
                    facts: (!facts.is_empty()).then(|| facts.to_owned()),
                    choices: (options.into_iter())
                        .map(|(option, effect)| {
                            let answer = Answer::Text(unticked(&option).to_owned());
                            let does = match asks_the_maintainer(&option) {
                                true => LEAVES_IT,
                                false => SENDS_ANSWER,
                            };
                            choice(&option, effect.as_deref().unwrap_or(does), answer)
                        })
                        .collect(),
                    board,
                }
            }
            RulingKind::AgentFiles { .. } => Self {
                problem: None,
                board: None,
                title: "Accept changes to agents' own files?".to_owned(),
                facts: Some(super::said(kind)),
                choices: yes_no(
                    "Accepts the files at that head and kelpie carries on",
                    Some(STOPS),
                ),
            },
            RulingKind::ForeignChange { .. } => Self {
                problem: None,
                board: None,
                title: "Accept a change made outside kelpie?".to_owned(),
                facts: Some(super::said(kind)),
                choices: yes_no("Accepts the change and kelpie carries on", Some(SENDS_BACK)),
            },
            RulingKind::AgentFit(fit) => Self {
                problem: None,
                board: None,
                title: format!(
                    "`{}` does worse at `{}` than `{}`. Change its `kinds`?",
                    fit.worse, fit.work_kind, fit.better
                ),
                facts: Some(super::said(kind)),
                choices: yes_no(
                    "Changes no file: the `kinds` line above is yours to write into the agent file",
                    Some("Changes no file and sends no note: the work item has finished"),
                ),
            },
            RulingKind::Contribution(Held::Security { head, lines }) => Self {
                problem: None,
                board: None,
                title: format!("Trust this contribution at {}?", short(head)),
                facts: Some(flagged_lines(lines)),
                choices: vec![
                    choice(
                        "Yes",
                        "Trusts the head and lets it go on",
                        Answer::Trust(short(head).to_owned()),
                    ),
                    choice(
                        "No",
                        "Leaves the head unpassed, so nothing runs on it and nothing is posted",
                        Answer::No(NO_NOTE.to_owned()),
                    ),
                ],
            },
            RulingKind::Contribution(Held::OverBudget { cap }) => Self {
                problem: None,
                board: None,
                title: "This contribution is over its budget. Raise the cap?".to_owned(),
                facts: Some(format!(
                    "The calls on it have cost more than its cap of ${:.2}.",
                    cap.usd()
                )),
                choices: yes_no(
                    "Doubles the cap, and again until it covers the spend, and the next call starts",
                    Some("Doubles the cap the same, and nothing is posted"),
                ),
            },
            RulingKind::Contribution(Held::Question { asked }) => Self {
                problem: None,
                board: None,
                title: "A contributor asks for a decision".to_owned(),
                facts: Some(asked.clone()),
                choices: Vec::new(),
            },
            RulingKind::Contribution(Held::Merge { head, rework_off }) => Self {
                problem: None,
                board: None,
                title: format!("Merge this contribution at {} into main?", short(head)),
                facts: None,
                choices: {
                    let mut choices = yes_no(
                        "Merges the head, and the representative thanks the contributor",
                        Some(
                            "The representative posts your note for the contributor, if you gave one",
                        ),
                    );
                    // Kelpie cannot push to the fork, so no worker can change it.
                    if !rework_off {
                        choices.push(choice(
                            "Rework",
                            "Takes the pull request over: a worker changes it, then the project's reviewers read what it pushed",
                            Answer::Rework(NO_NOTE.to_owned()),
                        ));
                    }
                    choices
                },
            },
            RulingKind::Contribution(Held::Waiting) => Self {
                problem: None,
                board: None,
                title: "A contributor said no twice and has gone quiet".to_owned(),
                facts: Some(
                    "Kelpie asked the contributor whether it may take the pull request \
                     over, and the answer was no, twice."
                        .to_owned(),
                ),
                choices: vec![
                    choice(
                        "Keep waiting",
                        "Starts the wait over, and kelpie asks again after it",
                        Answer::Text(KEEP_WAITING.to_owned()),
                    ),
                    choice(
                        "Adopt anyway",
                        "Records the release as the maintainer's, so kelpie takes it over",
                        Answer::Text(ADOPT_ANYWAY.to_owned()),
                    ),
                    choice(
                        "Close with thanks",
                        "The representative thanks the contributor and the pull request is closed",
                        Answer::Text(CLOSE_WITH_THANKS.to_owned()),
                    ),
                ],
            },
            RulingKind::Contribution(Held::NoEdits) => Self {
                problem: None,
                board: None,
                title: "A contributor has gone quiet and their fork cannot be edited".to_owned(),
                facts: Some(
                    "Kelpie can only finish a pull request by pushing to its branch, and \
                     the fork does not allow edits by maintainers, has been deleted, or \
                     belongs to an organization, whose forks GitHub does not let the \
                     upstream's maintainers push to."
                        .to_owned(),
                ),
                choices: vec![
                    choice(
                        "Leave it open",
                        "Kelpie follows the pull request no further, and posts nothing on it",
                        Answer::Text(LEAVE_OPEN.to_owned()),
                    ),
                    choice(
                        "Close with thanks",
                        "The representative thanks the contributor and the pull request is closed",
                        Answer::Text(CLOSE_WITH_THANKS.to_owned()),
                    ),
                ],
            },
            RulingKind::Contribution(Held::CannotAdopt { why }) => Self {
                problem: None,
                board: None,
                title: "A pull request kelpie took over cannot be adopted".to_owned(),
                facts: Some(format!(
                    "Kelpie cannot start a work item on it: {why}. Retarget it at the base \
                     branch, or fix what its issue is labelled for, and the next poll adopts it."
                )),
                choices: vec![
                    choice(
                        "Leave it open",
                        "Kelpie follows the pull request no further, and posts nothing on it",
                        Answer::Text(LEAVE_OPEN.to_owned()),
                    ),
                    choice(
                        "Close with thanks",
                        "The representative thanks the contributor and the pull request is closed",
                        Answer::Text(CLOSE_WITH_THANKS.to_owned()),
                    ),
                ],
            },
            RulingKind::FollowUp { findings, refused } => {
                let count = match findings.len() {
                    1 => "1 unfixed finding".to_owned(),
                    n => format!("{n} unfixed findings"),
                };
                let (title, yes, left) = match refused {
                    Some(_) => (
                        format!("Try filing {count} again?"),
                        "Tries filing each finding as an issue again",
                        ", and the forge would not take them as issues",
                    ),
                    None => (
                        format!("File {count} as issues?"),
                        "Files each finding as an issue on the board",
                        "",
                    ),
                };
                let held = match findings.len() {
                    1 => "1 confirmed finding".to_owned(),
                    n => format!("{n} confirmed findings"),
                };
                Self {
                    problem: None,
                    board: None,
                    title,
                    facts: Some(format!(
                        "This pull request merged with {held} left unfixed{left}."
                    )),
                    choices: yes_no(yes, Some("Drops the findings")),
                }
            }
        }
    }
}

// The title of a stuck ruling, and what a yes does and a no does
fn stuck(reason: &Stuck) -> (&'static str, &'static str, Option<&'static str>) {
    match reason {
        Stuck::Rebase { .. } => (
            "This branch cannot be rebased onto main",
            "Kelpie looks again once you have fixed the branch",
            Some(SENDS_BACK),
        ),
        Stuck::StillRed { .. } => (
            "CI is still red. Look again?",
            "Kelpie looks at CI again",
            Some(SENDS_BACK),
        ),
        Stuck::CheckQueued { .. } => (
            "A check is stuck queued. Rerun its run?",
            "Reruns the run once more and kelpie waits again",
            Some("Leaves the item waiting for you to act on the forge"),
        ),
        Stuck::NoChecks { .. } => (
            "No check has appeared on this head. Go on without one?",
            "Passes this head as skipped, and kelpie goes on",
            Some("Waits another while for a check to appear"),
        ),
        Stuck::MergeRefused { .. } => (
            "Merging was refused twice. Try again?",
            "Kelpie looks again and merges once every gate passes",
            Some(SENDS_BACK),
        ),
        Stuck::Closed => (
            "This pull request was closed. Drop the work item?",
            "Drops the work item and keeps its branch on the forge",
            Some(SENDS_BACK),
        ),
        Stuck::FixNotPushed { .. } => (
            "A fix ended without a push. Send the same fix turn again?",
            "Sends the worker the same fix turn again: the findings, or the failing tests it was sent back to",
            Some(SENDS_BACK),
        ),
        Stuck::TestStillFails { .. } => (
            "A reviewer's test still fails after two fixes. Send the worker back?",
            "Sends the worker one more fix turn on the same tests",
            Some(ENDS_THE_CHECK),
        ),
        Stuck::Unpushed { .. } => (
            "Work is not pushed. Give the worker another turn?",
            "Gives the worker another turn to push or discard its work",
            Some(SENDS_BACK),
        ),
        Stuck::PushRefused { .. } => (
            "The fork refused the worker's commits. Push them again?",
            "Pushes the worker's commits to the contributor's branch again",
            Some(STOPS),
        ),
        Stuck::TurnTimeout { .. } => (
            "The worker ran past its turn's ceiling. Resume it?",
            "Resumes the worker's session for another turn",
            Some(STOPS),
        ),
        Stuck::TurnFailed { setup: true, .. } => (
            "Kelpie could not set up the worker's turn. Try again once it is fixed?",
            "Tries that step again",
            Some(STOPS),
        ),
        Stuck::TurnFailed { .. } => (
            "The worker's turn failed. Try again?",
            "Tries that step again",
            Some(STOPS),
        ),
        Stuck::OverBudget {
            phase: Phase::Review(_),
            ..
        } => (
            "The work item is over its budget. Raise the cap?",
            "Doubles the work item's cap, and again until it covers the spend, and the review round starts",
            Some(RAISES_AND_REVIEWS),
        ),
        Stuck::OverBudget { .. } => (
            "The work item is over its budget. Raise the cap?",
            "Doubles the work item's cap, and again until it covers the spend, and the worker carries on",
            Some(RAISES),
        ),
        Stuck::AdoptFailed { .. } => (
            "The adopted pull request cannot start",
            "Kelpie tries again, as it does every poll",
            None,
        ),
    }
}

// The option a stuck ruling lists for an action kelpie can carry out, which
// says what kelpie will do
fn acting(reason: &Stuck) -> Option<Choice> {
    match reason {
        Stuck::StillRed {
            head, rerun: true, ..
        } => Some(choice(
            "Rerun",
            &format!(
                "Reruns the failed jobs of {} once more, and kelpie looks at CI again",
                short(head)
            ),
            Answer::Do(Action::Rerun),
        )),
        Stuck::TurnFailed { restore, .. } if !restore.is_empty() => Some(choice(
            "Restore",
            &format!(
                "Restores the uncommitted change to {} in the worktree, then resumes the turn",
                restore.join(", ")
            ),
            Answer::Do(Action::Restore),
        )),
        _ => None,
    }
}

fn choice(name: &str, does: &str, answer: Answer) -> Choice {
    Choice {
        name: name.to_owned(),
        does: does.to_owned(),
        answer,
    }
}

// A yes, and a no when the ruling takes one
fn yes_no(yes: &str, no: Option<&str>) -> Vec<Choice> {
    let no = no.map(|does| choice("No", does, Answer::No(NO_NOTE.to_owned())));
    [Some(choice("Yes", yes, Answer::Yes)), no]
        .into_iter()
        .flatten()
        .collect()
}

/// A worker's question, read
struct Asking {
    /// What is wrong, from its `Problem:` line
    problem: Option<String>,
    /// What it says, without the lines the worker and the project manager lead
    stem: String,
    /// The options it lists, each with the effect written after its ` => `
    options: Vec<(String, Option<String>)>,
    /// What the project manager's `PM board:` line says
    board: Option<String>,
}

// `option` without the backticks that wrap all of it, if they do; one that
// ends on code, such as ``Use `--check` ``, keeps its own.
fn unticked(option: &str) -> &str {
    let inner = option.strip_prefix('`').and_then(|o| o.strip_suffix('`'));
    inner.filter(|o| !o.contains('`')).unwrap_or(option)
}

/// Whether `option` is the candidate that leaves a question to the maintainer
pub(in crate::runner) fn asks_the_maintainer(option: &str) -> bool {
    let option = option.trim().trim_matches('`').trim_end_matches('.');
    option.eq_ignore_ascii_case(ASK_THE_MAINTAINER)
}

// Whether answering with `choice` sends the worker nothing.
fn leaves_it(choice: &Choice) -> bool {
    asks_the_maintainer(&choice.name)
}

// A worker's question without the project manager's lines, split into what it
// says and the options it lists, one `- ` line each at its end
fn split_question(asked: &str) -> Asking {
    let leads = |line: &str| {
        [RECOMMENDS, SAYS, PROBLEM, BOARD]
            .iter()
            .any(|l| line.starts_with(l))
    };
    let lines: Vec<&str> = asked.lines().filter(|line| !leads(line)).collect();
    let listed = |line: &&str| line.trim_start().starts_with("- ");
    let start = lines.iter().rposition(|l| !listed(l)).map_or(0, |i| i + 1);
    let (stem, list) = lines.split_at(start);
    Asking {
        problem: led_by(asked, PROBLEM),
        stem: stem.join("\n").trim().to_owned(),
        options: (list.iter())
            .map(|line| effect_of(line.trim_start().trim_start_matches("- ").trim()))
            .collect(),
        board: led_by(asked, BOARD),
    }
}

// The rest of the last line of `text` that starts with `lead`
fn led_by(text: &str, lead: &str) -> Option<String> {
    let line = text
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix(lead))?
        .trim();
    (!line.is_empty()).then(|| line.to_owned())
}

/// The option a `- ` line of a question lists, and the effect written after
/// its ` => `, if one is. A ` => ` inside backticks is part of the option.
pub(in crate::runner) fn effect_of(line: &str) -> (String, Option<String>) {
    let mut ticks = 0;
    for (at, c) in line.char_indices() {
        match c {
            '`' => ticks += 1,
            '=' if ticks % 2 == 0 && line[..at].ends_with(' ') && line[at..].starts_with("=> ") => {
                let effect = line[at + 3..].trim();
                let option = line[..at].trim_end().to_owned();
                return (option, (!effect.is_empty()).then(|| effect.to_owned()));
            }
            _ => {}
        }
    }
    (line.to_owned(), None)
}

/// What leads the line where a worker says what is wrong, and where the
/// project manager rewrites it when it passes the question on
pub(in crate::runner) const PROBLEM: &str = "Problem:";

/// What leads the line where the project manager says what the board shows
pub(in crate::runner) const BOARD: &str = "PM board:";

/// What leads the project manager's pick in a ruling it left to the maintainer
pub(in crate::runner) const RECOMMENDS: &str = "PM recommends:";

/// What leads the project manager's reasons, in a question and in a ruling it
/// put to the maintainer
pub(in crate::runner) const SAYS: &str = "PM says:";

// The project manager's pick and why, where it gave any:
// `**Project manager: A**. Its why.`
fn pm_words(ruling: &Ruling, choices: &[Choice]) -> Option<String> {
    let says = after_last(&ruling.question, SAYS).map(super::inert);
    let picked = after_last(&ruling.question, RECOMMENDS).and_then(|pick| {
        let plain = |text: &str| text.trim().trim_matches('`').to_lowercase();
        let at = choices.iter().position(|c| plain(&c.name) == plain(pick))?;
        ('A'..='Z').nth(at)
    });
    Some(match (picked, says) {
        (None, None) => return None,
        (Some(key), None) => format!("**Project manager: {key}**"),
        (Some(key), Some(words)) => format!("**Project manager: {key}**. {words}"),
        (None, Some(words)) => format!("**Project manager**: {words}"),
    })
}

// The rest of the line after the last `lead` in `text`
fn after_last<'a>(text: &'a str, lead: &str) -> Option<&'a str> {
    let at = text.rfind(lead)? + lead.len();
    let line = text[at..].lines().next()?.trim();
    (!line.is_empty()).then_some(line)
}

fn table(choices: &[Choice], kind: &RulingKind) -> String {
    let mut rows = vec![
        "| | Option | If you pick it |".to_owned(),
        "|---|---|---|".to_owned(),
    ];
    for (key, choice) in ('A'..='Z').zip(choices) {
        rows.push(format!(
            "| **{key}** | {} | {} |",
            cell(&choice.name),
            cell(&choice.does)
        ));
    }
    if Wants::of(kind) == Wants::Answer {
        rows.push("| | Other | Reply with your own answer |".to_owned());
    }
    rows.join("\n")
}

// A table cell: one line, with no bar to end it early
fn cell(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    line.replace('|', "\\|")
}

fn answering(login: &str, number: TicketId, kind: &RulingKind, choices: &[Choice]) -> String {
    let mut lines = vec![format!("@{login}")];
    let tick = |lines: &mut Vec<String>| {
        lines.extend(
            ('A'..='Z')
                .zip(choices)
                .map(|(key, _)| format!("- [ ] {key}")),
        );
    };
    match Wants::of(kind) {
        Wants::Merge => {
            lines.push("- 👍 or 👎 answers, or tick one:".to_owned());
            tick(&mut lines);
            lines.push(match needs_pass(kind) {
                true => format!("- Or reply `{number} no <note>`"),
                false => format!("- Or reply `{number} no <note>` or `{number} rework <note>`"),
            });
        }
        Wants::Answer => {
            let reply = format!(
                "with your answer, and start it with `{number}` if other rulings wait on this thread"
            );
            if choices.is_empty() {
                lines.push(format!("- Reply {reply}"));
            } else {
                lines.push("- Tick one:".to_owned());
                tick(&mut lines);
                lines.push(format!("- Or reply {reply}"));
            }
        }
        Wants::YesOrNo if choices.len() == 1 => {
            lines.push("- 👍 answers yes".to_owned());
            lines.push(format!("- Or reply `{number} yes`"));
        }
        Wants::YesOrNo => {
            lines.push("- 👍 answers yes, 👎 answers no".to_owned());
            let mut replies = vec![format!("`{number} yes`"), format!("`{number} no <note>`")];
            replies.extend(actions(choices).map(|word| format!("`{number} {word}`")));
            lines.push(format!("- Or reply {}", or_list(&replies)));
        }
    }
    lines.join("\n")
}

// The words that choose the options of `choices` that name an action
fn actions(choices: &[Choice]) -> impl Iterator<Item = &'static str> + '_ {
    choices.iter().filter_map(|c| match c.answer {
        Answer::Do(action) => Some(action.word()),
        _ => None,
    })
}

// `items` as "a or b", or "a, b or c"
fn or_list(items: &[String]) -> String {
    match items {
        [rest @ .., last] if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => items.join(""),
    }
}

// A ruling that mentions no one is answered at the terminal only.
fn unmentioned(number: TicketId, kind: &RulingKind, choices: &[Choice]) -> String {
    let rule = format!("shep kelpie rule {number}");
    let forms = match Wants::of(kind) {
        Wants::Answer => format!("`{rule} <your answer>`"),
        Wants::Merge if needs_pass(kind) => format!("`{rule} yes` or `{rule} no <note>`"),
        Wants::Merge => format!("`{rule} yes`, `{rule} no <note>` or `{rule} rework <note>`"),
        Wants::YesOrNo if choices.len() == 1 => format!("`{rule} yes`"),
        Wants::YesOrNo => {
            let mut forms = vec![format!("`{rule} yes`"), format!("`{rule} no <note>`")];
            forms.extend(actions(choices).map(|word| format!("`{rule} {word}`")));
            or_list(&forms)
        }
    };
    format!(
        "This ruling mentions no one, so a reaction, a tick or a reply here answers nothing. \
         Answer it at the terminal: {forms}."
    )
}

#[cfg(test)]
mod tests;
