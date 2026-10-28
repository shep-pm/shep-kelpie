//! Rulings: raising one, and the maintainer's answer
//!
//! A ruling is saved before its comment is posted, so a comment that fails
//! loses nothing: the ruling stays in status and in the log. Its question
//! carries the exact triggers that answer it. What a yes does depends on
//! the ruling; a no's note, or an answer to the worker's question, is
//! always the worker's next turn.

use std::fmt;

use super::contributions;
use super::gate::short;
use super::report::{Begin, StepReport};
use super::review::pushed::push_prompt;
use super::talk::with_thread_lines;
use super::thread_replies::Settler;
use super::{Names, Runner};
use crate::board::HUMAN;
use crate::ports::Timestamp;
use crate::request::{RuleAction, RuleAnswer};
use crate::settings::Merging;
use crate::state::{Fix, Held, ProjectState, Resume, Ruling, RulingKind, StateError, Stuck};
use crate::ticket::TicketId;
use crate::work_item::{Known, Phase, Review, ReviewStage, Turn, WorkItem, foreign_change};
use crate::worktree::{self};

mod act;
mod history;
pub(super) use history::{Answerer, Record, pm_answers, records};
mod post;
pub(super) use post::PostJob;
mod prompts;
mod question;
mod queued;
mod stuck;
mod test_fails;
use prompts::{
    answer_prompt, declined_prompt, failed_again, note_prompt, noted_prompt, rework_prompt,
};
pub(super) use question::question;
use stuck::fix_turns_had;
pub(in crate::runner) use stuck::stuck_comment;

/// The prompt for a turn resumed after the maintainer accepts a timed-out
/// turn's ruling with a yes
const TIMEOUT_CONTINUE: &str = "Kelpie stopped your last turn: it ran past its ceiling. \
                                Commit any work that builds now, \
                                then carry on from where you left off.";

/// The prompt for a turn resumed after the maintainer accepts a failed
/// turn's ruling with a yes
const FAILED_CONTINUE: &str = "Your last turn failed before it finished. \
                               Carry on with the work item from where you left off.";

/// The maintainer's answer to a ruling
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Go ahead: what that means depends on the ruling
    Yes,
    /// Do not, and send the worker this note. On a merge ruling its fix
    /// goes to CI and back to the ruling.
    No(String),
    /// On a merge ruling only: send the worker this note, and its change
    /// through a new pass of the review
    Rework(String),
    /// The answer to the worker's question
    Text(String),
    /// On a contribution's ruling only: a yes that names the head it trusts,
    /// as many characters of its hash as the maintainer read. A yes meant for
    /// one head must not trust the next, and every ruling on a pull request
    /// has the same number.
    Trust(String),
    /// A ruling's option that names an action: kelpie carries it out, then
    /// the work item goes on as a yes would send it
    Do(Action),
}

/// What kelpie does, in code, when an option that names it is chosen
///
/// A ruling lists such an option only when it can be carried out, and its
/// text says what kelpie will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Rerun the failed jobs of the head, for a still-red ruling whose failed
    /// logs name no file the branch changed
    Rerun,
    /// Restore the uncommitted change to an agent file in the worktree, for a
    /// failed turn
    Restore,
}

impl From<RuleAction> for Action {
    fn from(action: RuleAction) -> Self {
        match action {
            RuleAction::Rerun => Self::Rerun,
            RuleAction::Restore => Self::Restore,
        }
    }
}

impl From<Action> for RuleAction {
    fn from(action: Action) -> Self {
        match action {
            Action::Rerun => Self::Rerun,
            Action::Restore => Self::Restore,
        }
    }
}

impl From<RuleAnswer> for Answer {
    fn from(answer: RuleAnswer) -> Self {
        match answer {
            RuleAnswer::Yes => Self::Yes,
            RuleAnswer::No { note } => Self::No(note),
            RuleAnswer::Rework { note } => Self::Rework(note),
            RuleAnswer::Text { text } => Self::Text(text),
            RuleAnswer::Trust { head } => Self::Trust(head),
            RuleAnswer::Do { action } => Self::Do(action.into()),
        }
    }
}

impl From<Answer> for RuleAnswer {
    fn from(answer: Answer) -> Self {
        match answer {
            Answer::Yes => Self::Yes,
            Answer::No(note) => Self::No { note },
            Answer::Rework(note) => Self::Rework { note },
            Answer::Text(text) => Self::Text { text },
            Answer::Trust(head) => Self::Trust { head },
            Answer::Do(action) => Self::Do {
                action: action.into(),
            },
        }
    }
}

/// Why an answer was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleError {
    /// No pending ruling has this id
    NoSuchRuling(TicketId),
    /// The ruling is the worker's question, and was given a yes or a no
    WantsAnswer(TicketId),
    /// The ruling is not a question, and was given an answer
    NotAQuestion(TicketId),
    /// The ruling is not a merge ruling, and was given a rework
    NotAMerge(TicketId),
    /// The ruling asks for an issue's label, which no note can give, and
    /// was given a no
    YesOnly(TicketId),
    /// The `ready-for-human` label could not come off this pull request
    Unlabel(u64, String),
    /// The threads that held a merge ruling could not be read from the forge
    Threads(u64, String),
    /// The worktree could not be brought to the head a yes accepted
    Adopt(String, String),
    /// A ruling the project manager answered could not be reopened
    Reopen(TicketId, String),
    /// The ruling lists no option for this action
    NotOffered(TicketId, Action),
    /// The action an option names could not be carried out
    Act(TicketId, String),
    /// A yes does not fit this ruling's head: it names none or another, or the
    /// ruling is not one that takes a head
    Head(TicketId, String),
    /// The answer is none of the ruling's options
    Choice(TicketId, String),
    /// The answer could not be saved
    State(StateError),
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchRuling(id) => {
                write!(f, "ruling {id} is already answered, or was never raised")
            }
            Self::WantsAnswer(id) => write!(
                f,
                "ruling {id} is the worker's question, so it takes an answer, not a yes or no"
            ),
            Self::NotAQuestion(id) => write!(
                f,
                "ruling {id} is not a question, so it takes a yes, or a no with a note"
            ),
            Self::NotAMerge(id) => write!(
                f,
                "ruling {id} is not a merge ruling, so it takes a yes, or a no with a note"
            ),
            Self::YesOnly(id) => write!(
                f,
                "ruling {id} waits on an issue's `agent:` label, which no worker can take a \
                 note for: label the issue, then answer yes"
            ),
            Self::Unlabel(number, e) => {
                write!(f, "cannot take the `{HUMAN}` label off #{number}: {e}")
            }
            Self::Threads(number, e) => {
                write!(f, "cannot read the review threads on #{number}: {e}")
            }
            Self::Adopt(head, e) => {
                write!(f, "cannot bring the worktree to {}: {e}", short(head))
            }
            Self::Reopen(id, why) => write!(f, "ruling {id} cannot be reopened: {why}"),
            Self::NotOffered(id, action) => {
                write!(f, "ruling {id} does not offer to {}", action.verb())
            }
            Self::Act(id, why) | Self::Head(id, why) | Self::Choice(id, why) => {
                write!(f, "ruling {id}: {why}")
            }
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for RuleError {}

/// What an answer does to the work item parked on its ruling
enum Move {
    /// It goes on to this phase
    Phase(Phase),
    /// The worker takes a turn with this prompt, under this phase; once
    /// that turn ends with no further question, `force` (when given)
    /// replaces the ordinary rule of a known pull request going to CI
    Turn {
        prompt: String,
        phase: Phase,
        force: Option<Phase>,
    },
    /// A no on a work item over its budget: its note goes ahead of the turn
    /// that was due, which otherwise goes on as it stood
    Noted { note: String, phase: Phase },
    /// A yes on a failed turn: the turn goes back as it stood, under its phase
    Retry { turn: Turn, phase: Phase },
    /// A yes on a foreign change: kelpie adopts it. A new head goes
    /// through the review, and anything else back to CI.
    Accept(Known),
    /// A no on a head moved from `from` to `to`: the worker builds on `to`,
    /// taking a turn with this prompt
    Decline {
        prompt: String,
        from: String,
        to: String,
    },
}

impl Runner {
    /// Answers ruling `id`. A no's note, or an answer, is the worker's next turn.
    ///
    /// # Errors
    ///
    /// [`RuleError`] when no such ruling is pending, the answer does not fit
    /// it, or the answer cannot be saved. The ruling stays pending then, and
    /// answering again is safe: a worktree already at an accepted head is left.
    pub fn rule(&mut self, id: TicketId, answer: Answer) -> Result<(), RuleError> {
        self.looks.poll_reset();
        self.rule_by(id, answer, Settler::Terminal)
    }

    // Applies an answer, and says whether it was taken, not after the branch moved.
    // `by` says who gave it, which the record of it says.
    pub(super) fn apply(
        &mut self,
        id: TicketId,
        answer: Answer,
        by: Answerer,
        unread: bool,
    ) -> Result<bool, RuleError> {
        let at = self.state.rulings.iter().position(|r| r.id == id);
        let at = at.ok_or(RuleError::NoSuchRuling(id))?;
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let ruling = next.rulings.remove(at);
        let act = act::plan(id, &answer, &ruling.kind)?;
        let asked = act::asked(&ruling, &answer, by);
        // Kept in the rulings history once the answer is saved.
        let record = Record::of(&ruling, &answer, now, by);
        // Whether the findings a merged pull request left are filed or dropped.
        let follow_up = match (&answer, &ruling.kind) {
            (Answer::Yes, RulingKind::FollowUp { .. }) => Some(true),
            (Answer::No(_), RulingKind::FollowUp { .. }) => Some(false),
            _ => None,
        };
        // A yes to the ruling that asks before filing is the findings' triage.
        // One to a refusal only tries again, and triages nothing.
        let triaged = matches!(
            (&answer, &ruling.kind),
            (Answer::Yes, RulingKind::FollowUp { refused: None, .. })
        );
        // ... unless the project manager asks for them to be filed unread, as
        // `needs-triage`.
        let unread = triaged && unread;
        let accepts = match (&answer, &ruling.kind) {
            (Answer::Yes, RulingKind::AgentFiles { head, .. }) => Some(head.clone()),
            _ => None,
        };
        // A merge ruling's no whose fix goes to CI with no pass of the review.
        // Any answer to a merge ruling replaces what an earlier no left.
        let merge = matches!(ruling.kind, RulingKind::Merge { .. });
        // A yes merges past the threads that held it, so a catch-up with
        // `main` that withdraws the yes does not ask about them again.
        let noted_from = match (&answer, &ruling.kind) {
            (Answer::No(_), RulingKind::Merge { head, .. }) if !needs_pass(&ruling.kind) => {
                Some(head.clone())
            }
            _ => None,
        };
        // A yes to a merge vouches for the head it names, so a catch-up with
        // `main` before the merge carries that on and asks nothing again.
        let merged_head = match (&answer, &ruling.kind) {
            (Answer::Yes, RulingKind::Merge { head, .. }) => Some(head.clone()),
            _ => None,
        };
        // These ask the maintainer to fix the branch, so a yes vouches for its head.
        let vouches = matches!(
            (&answer, &ruling.kind),
            (
                Answer::Yes,
                RulingKind::Stuck(Stuck::Rebase { .. } | Stuck::StillRed { .. })
            )
        );
        let mid_review = matches!(
            (&answer, &ruling.kind),
            (
                Answer::Yes,
                RulingKind::Stuck(
                    Stuck::StillRed {
                        review: Some(_),
                        ..
                    } | Stuck::Rebase {
                        review: Some(_),
                        ..
                    }
                )
            )
        );
        // A yes passes the head that got no check as skipped.
        let unchecked = match (&answer, &ruling.kind) {
            (Answer::Yes, RulingKind::Stuck(Stuck::NoChecks { head, .. })) => Some(head.clone()),
            _ => None,
        };
        // Any answer to a refused merge gives the next one its catch-up again.
        let refusal_answered = matches!(ruling.kind, RulingKind::Stuck(Stuck::MergeRefused { .. }));
        // Either answer to a work item over its budget lifts its cap, as far as
        // its next turn needs to start.
        let raises = matches!(
            (&answer, &ruling.kind),
            (
                Answer::Yes | Answer::No(_),
                RulingKind::Stuck(Stuck::OverBudget { .. })
            )
        );
        // An answered question gives the next turn that stops short its nudge again.
        let answered = matches!(ruling.kind, RulingKind::Question { .. });
        // Only the ruling the work item is parked on moves it. Any other,
        // which nothing leaves behind today, is answered by clearing it.
        let parked_on = |item: &WorkItem| item.phase == Phase::Ruling { id };
        let parked = next.work_items.iter().find(|item| parked_on(item));
        // The answer moves only the work item it parks, whichever is open.
        self.focus = parked.map(|item| item.issue);
        // A ruling raised before the ids were kept (state version 24) names
        // its threads and holds no ids, so the forge says which hold it now.
        let accepted = match (&answer, &ruling.kind) {
            (Answer::Yes, RulingKind::Merge { held_by, .. }) if !held_by.is_empty() => {
                held_by.clone()
            }
            (
                Answer::Yes,
                RulingKind::Merge {
                    open_threads: Some(_),
                    ..
                },
            ) => match ruling.pull_request {
                Some(number) => {
                    (self.threads_open(number))
                        .map_err(|e| RuleError::Threads(number, e))?
                        .held_by
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        };
        let head_moved = match (parked, &ruling.kind) {
            (Some(item), RulingKind::ForeignChange { known: seen, .. }) => {
                match (&item.known.head, &seen.head) {
                    (Some(from), Some(to)) if from != to => Some((from.clone(), to.clone())),
                    _ => None,
                }
            }
            _ => None,
        };
        if let (Some((_, to)), Answer::Yes | Answer::No(_), Some(item)) =
            (&head_moved, &answer, parked)
            && let RulingKind::ForeignChange { known: seen, .. } = &ruling.kind
        {
            let tip =
                worktree::origin_head(&self.settings.git.checkout, &item.branch, &item.remote)
                    .map_err(|e| RuleError::Adopt(to.clone(), e.to_string()))?;
            if tip != *to {
                let change = foreign_change(&item.known, &seen.labels, seen.ready, &tip);
                let issue = item.issue;
                return self
                    .ask_again(next, issue, ruling.pull_request, change, now)
                    .map(|()| false);
            }
        }
        let worktree = parked.map(|item| item.worktree.clone());
        contributions::trust(&mut next, &answer, &ruling)?;
        contributions::keep_answer(&mut next, &answer, &ruling);
        contributions::rule_merge(&mut next, &answer, &ruling);
        contributions::settle(&mut next, &answer, &ruling, now)?;
        // A contribution's cap lifts, and its next call may start at the next look.
        let per_item = self.settings.budget.per_item_usd;
        if contributions::raise_budget(&mut next, &answer, &ruling, per_item) {
            self.looks.board_moved();
        }
        let moved = decide(id, answer, ruling, now, head_moved)?;
        let did = (act.as_ref())
            .map(|act| self.carry_out(id, act, worktree.as_deref()))
            .transpose()?;
        if let Some(did) = &did {
            next.record_event(now, format!("{asked}: {did}"));
        }
        if let Some(item) = next.work_items.iter_mut().find(|item| parked_on(item)) {
            if let (Some(filed), Some(pending)) = (follow_up, item.follow_ups.as_mut()) {
                if filed {
                    pending.ruled |= triaged;
                    pending.unread |= unread;
                    pending.first_refused = None;
                } else {
                    pending.findings.clear();
                }
            }
            if accepts.is_some() {
                item.claude_files_accepted = accepts;
            }
            if unchecked.is_some() {
                item.unchecked = unchecked;
            }
            if let Some(act::Act::Rerun { head }) = act {
                item.reran = Some(head);
            }
            if merge {
                item.threads_accepted.extend(accepted);
                item.noted_from = noted_from;
                item.late_from = None;
                if let Some(head) = merged_head {
                    item.send_unread(head);
                }
                self.late_reads.remove(&item.issue);
            }
            let regate = match (vouches, self.settings.git.merging) {
                (true, Merging::Auto) => regate(&self.settings.git.checkout, item)?,
                // The round it returns to reads the worktree, so a fix the
                // maintainer pushed is adopted before it, as under `auto`.
                (true, Merging::Ask) if mid_review => regate(&self.settings.git.checkout, item)?,
                (true, Merging::Ask) => {
                    item.known.head = None;
                    false
                }
                (false, _) => false,
            };
            if refusal_answered {
                item.merge_refused = false;
            }
            if raises {
                item.raise_budget(self.settings.budget.per_item_usd);
            }
            if answered {
                item.sent_back = false;
                item.handover_refused = false;
            }
            let worker = match moved {
                Move::Phase(phase) => {
                    item.phase = phase;
                    None
                }
                Move::Turn {
                    prompt,
                    phase,
                    force,
                } => Some((Turn::Next { prompt }, phase, force)),
                Move::Retry { turn, phase } => Some((turn, phase, None)),
                Move::Noted { note, phase } => match &item.turn {
                    Turn::Next { prompt } => Some((
                        Turn::Next {
                            prompt: noted_prompt(&note, prompt),
                        },
                        phase,
                        None,
                    )),
                    // No session has begun for a first turn to resume, so no note can reach it.
                    other => {
                        if matches!(other, Turn::Due) {
                            self.notes.push(format!(
                                "ruling {id}: the worker's session has not begun, so your note was not sent"
                            ));
                        }
                        item.phase = phase;
                        None
                    }
                },
                Move::Accept(known) => {
                    let (from, to) = (item.known.head.take(), known.head.clone());
                    item.phase = match (from, to) {
                        (Some(from), Some(to)) if from != to => {
                            let (repo, branch) = (&self.settings.git.checkout, &item.branch);
                            worktree::adopt(repo, &item.worktree, branch, &from, &to, &item.remote)
                                .map_err(|e| RuleError::Adopt(to, e.to_string()))?;
                            Phase::Review(Review::first())
                        }
                        _ => Phase::Ci {
                            head: None,
                            since: now,
                        },
                    };
                    item.known = known;
                    None
                }
                Move::Decline { prompt, from, to } => {
                    let (repo, branch) = (&self.settings.git.checkout, &item.branch);
                    worktree::adopt(repo, &item.worktree, branch, &from, &to, &item.remote)
                        .map_err(|e| RuleError::Adopt(to.clone(), e.to_string()))?;
                    item.known.head = Some(to);
                    let review = Some(Phase::Review(Review::first()));
                    Some((Turn::Next { prompt }, Phase::Implement, review))
                }
            };
            if regate {
                item.phase = Phase::Review(Review::first());
            }
            if let Some((turn, phase, force)) = worker {
                // The requests a question interrupted are still owed their replies.
                let owed = item.talked.head.is_some() && !item.threads_sent.is_empty();
                item.turn = match turn {
                    Turn::Next { prompt } if owed => Turn::Next {
                        prompt: with_thread_lines(prompt),
                    },
                    turn => turn,
                };
                item.phase = phase;
                // A turn the ruling interrupted may still owe the review.
                item.resume = force.or(item.resume.take());
                // The worker's turn again, so the hand-back label comes off.
                if let Some(number) = item.pull_request
                    && item.known.labels.iter().any(|l| l == HUMAN)
                {
                    let repo = &self.remote;
                    let off = self.ports.forge.set_label(repo, number, HUMAN, false);
                    off.map_err(|e| RuleError::Unlabel(number, e.to_string()))?;
                    item.known.labels.retain(|l| l != HUMAN);
                }
            }
        }
        self.save(next).map_err(RuleError::State)?;
        if let Err(why) = record.append(&self.paths.rulings) {
            self.notes.push(format!(
                "ruling {id} is not kept in the rulings history: {why}"
            ));
        }
        Ok(true)
    }

    // The branch moved again while the ruling waited, so the answer was about
    // a head that is gone. What stands now is asked about afresh, or, when
    // nothing outside kelpie is left, the gate looks again.
    fn ask_again(
        &mut self,
        mut next: ProjectState,
        issue: TicketId,
        number: Option<u64>,
        change: Option<(Known, String)>,
        now: Timestamp,
    ) -> Result<(), RuleError> {
        let Some((known, description)) = change else {
            if let Some(item) = next.item_mut(issue) {
                item.phase = Phase::Ci {
                    head: None,
                    since: now,
                };
            }
            return self.save(next).map_err(RuleError::State);
        };
        let kind = RulingKind::ForeignChange { description, known };
        let (id, _) = park(self.names(), &mut next, issue, number, kind);
        self.save(next).map_err(RuleError::State)?;
        // A comment that fails loses nothing: the ruling is saved and alerted.
        if let Some(why) = self.post_ruling(number, id) {
            self.notes
                .push(format!("cannot post ruling {id} on its thread: {why}"));
        }
        Ok(())
    }

    // Saves the ruling and parks the worker on it, then posts it on pull
    // request `number`.
    pub(super) fn raise(&mut self, number: u64, kind: RulingKind) -> Result<Begin, StateError> {
        let mut next = self.state.clone();
        let issue = self.current().expect("a ruling is about a work item").issue;
        let (id, question) = park(self.names(), &mut next, issue, Some(number), kind);
        self.save(next)?;
        self.word_effects(id)?;
        let comment_failed = self.post_ruling(Some(number), id);
        Ok(Begin::Report(StepReport::Ruling {
            issue,
            pull_request: number,
            id,
            question,
            comment_failed,
        }))
    }

    pub(super) fn update(&mut self, change: impl FnOnce(&mut WorkItem)) -> Result<(), StateError> {
        let mut next = self.state.clone();
        change(
            self.current_in(&mut next)
                .expect("a change to the work item in flight"),
        );
        self.save(next)
    }
}

// Under `auto` no merge ruling follows a yes that vouches for the branch,
// so a head the gates never saw is adopted and goes back through them.
// Returns whether it does; an unchanged head keeps its place.
fn regate(repo: &std::path::Path, item: &mut WorkItem) -> Result<bool, RuleError> {
    let known = item.known.head.clone();
    let tip = worktree::origin_head(repo, &item.branch, &item.remote)
        .map_err(|e| RuleError::Adopt(known.clone().unwrap_or_default(), e.to_string()))?;
    let from = match known {
        Some(known) => known,
        None => worktree::head(repo, &item.worktree)
            .map_err(|e| RuleError::Adopt(tip.clone(), e.to_string()))?,
    };
    if from == tip && item.known.head.is_some() {
        return Ok(false);
    }
    worktree::adopt(
        repo,
        &item.worktree,
        &item.branch,
        &from,
        &tip,
        &item.remote,
    )
    .map_err(|e| RuleError::Adopt(tip.clone(), e.to_string()))?;
    item.known.head = Some(tip);
    Ok(true)
}

/// Adds a ruling to `next` and parks the work item for `issue` on it
///
/// Returns the ruling's id and question.
pub(super) fn park(
    names: Names,
    next: &mut ProjectState,
    issue: TicketId,
    pull_request: Option<u64>,
    kind: RulingKind,
) -> (TicketId, String) {
    // A work item parks on one ruling at a time, so its pull request's number
    // names it, or its ticket's id before there is one.
    let id = pull_request.and_then(TicketId::from_issue).unwrap_or(issue);
    let item = next
        .item_mut(issue)
        .expect("a ruling is about an open work item");
    item.phase = Phase::Ruling { id };
    item.counts.rulings = item.counts.rulings.saturating_add(1);
    let text = question(issue, pull_request, &kind);
    next.rulings.push(Ruling {
        issue: Some(issue),
        pull_request,
        raised: Some(names.now),
        ..Ruling::new(id, text.clone(), kind)
    });
    (id, text)
}

// What a reader of the pull request is told of a ruling: what happened and
// that it waits on the maintainer, with no command and nothing of kelpie's.
// A merge ruling says nothing, since `ready-for-human` already does.
fn comment(kind: &RulingKind) -> Option<String> {
    match kind {
        RulingKind::Merge { .. } => return None,
        // Merged and done: nothing on the pull request waits on the maintainer.
        RulingKind::FollowUp { .. } => return None,
        _ => {}
    }
    Some(format!(
        "{}\n\nWaiting on the maintainer.",
        super::voice::said(kind)
    ))
}

// A yes, a no or an answer that does not fit the ruling is refused.
fn decide(
    id: TicketId,
    answer: Answer,
    ruling: Ruling,
    now: Timestamp,
    head_moved: Option<(String, String)>,
) -> Result<Move, RuleError> {
    // The declined commit stays on `origin`, so the worker must build on it
    // or its push is refused.
    if let (Answer::No(note), Some((from, to))) = (&answer, head_moved) {
        return Ok(Move::Decline {
            prompt: declined_prompt(ruling.pull_request, &to, note),
            from,
            to,
        });
    }
    let warned = needs_pass(&ruling.kind);
    let phase = match (answer, ruling.kind) {
        (Answer::Text(text), RulingKind::Question { resume, .. }) => {
            // A question resumes exactly where it interrupted the review;
            // one asked before the review ever started, with a pull request
            // already open, starts it once answered instead of the ordinary
            // rule of going straight to CI.
            let (phase, force) = match resume {
                Resume::Nothing => (Phase::Implement, None),
                Resume::ReviewFirst => (Phase::Implement, Some(Phase::Review(Review::first()))),
                Resume::Review(review) => (Phase::Review(review), None),
            };
            return Ok(Move::Turn {
                prompt: answer_prompt(&text),
                phase,
                force,
            });
        }
        // No work item is parked on it, so an answer only clears it. For a
        // decision the representative words the text for the contributor; a
        // waiting ruling's text is a choice that kelpie carries out.
        (
            Answer::Text(_),
            RulingKind::Contribution(Held::Question { .. } | Held::Waiting | Held::NoEdits),
        ) => Phase::Implement,
        (
            _,
            RulingKind::Question { .. }
            | RulingKind::Contribution(Held::Question { .. } | Held::Waiting | Held::NoEdits),
        ) => {
            return Err(RuleError::WantsAnswer(id));
        }
        (Answer::Text(_), _) => return Err(RuleError::NotAQuestion(id)),
        // A merge ruling's no is a fix of what the maintainer read, which
        // goes to CI and back to this ruling. A rework is a change that needs
        // the whole review again, and so is a no on a ruling that warns of
        // what only a pass clears.
        (Answer::No(note), RulingKind::Merge { .. }) if warned => {
            return Ok(Move::Turn {
                prompt: format!(
                    "{}\nOnce you push it, the whole review reads your change again.\n",
                    note_prompt(ruling.pull_request, &note)
                ),
                phase: Phase::Implement,
                force: Some(Phase::Review(Review::first())),
            });
        }
        (Answer::No(note), RulingKind::Merge { .. }) => {
            return Ok(Move::Turn {
                prompt: note_prompt(ruling.pull_request, &note),
                phase: Phase::Implement,
                force: None,
            });
        }
        (Answer::Rework(note), RulingKind::Merge { .. }) => {
            return Ok(Move::Turn {
                prompt: rework_prompt(ruling.pull_request, &note),
                phase: Phase::Implement,
                force: Some(Phase::Review(Review::first())),
            });
        }
        (
            Answer::Rework(_),
            RulingKind::Contribution(Held::Merge {
                rework_off: true, ..
            }),
        ) => {
            return Err(RuleError::Choice(
                id,
                "the pull request's fork cannot be pushed to, so it takes `yes` or `no <note>`"
                    .to_owned(),
            ));
        }
        // A contribution's rework is carried out by the merge stage, which
        // releases the record and queues it for adoption. No work item is
        // parked on it, so here the answer only clears the ruling.
        (Answer::Rework(_), RulingKind::Contribution(Held::Merge { .. })) => Phase::Implement,
        (Answer::Rework(_), _) => return Err(RuleError::NotAMerge(id)),
        (
            Answer::Yes | Answer::No(_),
            RulingKind::Stuck(Stuck::CheckQueued { review, .. } | Stuck::NoChecks { review, .. }),
        ) => queued::waiting_again(review, now),
        // The jobs are rerun, so CI is looked at again, in the round it
        // interrupted when it did.
        (
            Answer::Do(Action::Rerun),
            RulingKind::Stuck(Stuck::StillRed {
                review: Some(mut review),
                ..
            }),
        ) => {
            // The checks read red for a moment after a rerun starts.
            if let Some(ci) = review.ci.as_deref_mut() {
                ci.since = now;
            }
            Phase::Review(review)
        }
        (Answer::Do(Action::Rerun), RulingKind::Stuck(Stuck::StillRed { .. })) => Phase::Ci {
            head: None,
            since: now,
        },
        (Answer::No(_), RulingKind::Stuck(Stuck::AdoptFailed { .. })) => {
            return Err(RuleError::YesOnly(id));
        }
        // A fix turn resumes in its round, which checks it pushed.
        (Answer::Yes, RulingKind::Stuck(Stuck::TurnTimeout { phase })) => {
            return Ok(Move::Turn {
                prompt: TIMEOUT_CONTINUE.to_owned(),
                phase: phase.unwrap_or(Phase::Implement),
                force: None,
            });
        }
        // A turn that had started is resumed with a prompt of its own, and
        // the yes starts its ceiling afresh: the time it spent failing and
        // waiting is not held against it.
        (
            Answer::Yes | Answer::Do(Action::Restore),
            RulingKind::Stuck(Stuck::TurnFailed { phase, retry, .. }),
        ) => {
            let turn = match retry {
                Turn::Running { prompt, .. } => Turn::Next {
                    prompt: prompt.map_or_else(|| FAILED_CONTINUE.to_owned(), failed_again),
                },
                other => other,
            };
            return Ok(Move::Retry { turn, phase });
        }
        // The cap doubles on either answer, in `apply`, and the turn that was due
        // goes on in its phase. A no sends the worker its note in that turn.
        (Answer::Yes, RulingKind::Stuck(Stuck::OverBudget { phase, .. })) => phase,
        // A review round has no worker turn due to take a note, so a no there is
        // a fix turn like any other ruling's, and the pull request is reviewed again.
        (Answer::No(note), RulingKind::Stuck(Stuck::OverBudget { phase, .. }))
            if !matches!(phase, Phase::Review(_)) =>
        {
            return Ok(Move::Noted {
                note: note_prompt(ruling.pull_request, &note),
                phase,
            });
        }
        // A worker cannot write agents' own files or change a fork's settings, so a
        // note would not help it.
        (
            Answer::No(_),
            RulingKind::Stuck(
                Stuck::TurnTimeout { .. } | Stuck::TurnFailed { .. } | Stuck::PushRefused { .. },
            )
            | RulingKind::AgentFiles { .. },
        ) => Phase::Done {
            merged: false,
            closed: false,
        },
        (Answer::Yes, RulingKind::AgentFiles { phase, .. }) => phase,
        (Answer::Yes, RulingKind::ForeignChange { known, .. }) => return Ok(Move::Accept(known)),
        // The pull request is merged, so a no has no worker to send a note to.
        (Answer::Yes | Answer::No(_), RulingKind::FollowUp { .. }) => Phase::Done {
            merged: true,
            closed: false,
        },
        // No work item is parked on it, so an answer only clears it.
        (Answer::Trust(_) | Answer::No(_), RulingKind::Contribution(_))
        | (Answer::Yes, RulingKind::Contribution(Held::OverBudget { .. } | Held::Merge { .. })) => {
            Phase::Implement
        }
        // `trust` has refused these before `decide` is asked.
        (Answer::Trust(_), _) | (Answer::Yes, RulingKind::Contribution(_)) => {
            return Err(RuleError::Head(id, "takes no such yes".to_owned()));
        }
        (Answer::Do(action), _) => return Err(RuleError::NotOffered(id, action)),
        // The test is ended: the note is the worker's turn, and the pass goes on.
        (Answer::No(note), RulingKind::Stuck(Stuck::TestStillFails { fix, head, .. })) => {
            let (prompt, phase) = test_fails::no(ruling.pull_request, fix, &head, &note);
            return Ok(Move::Turn {
                prompt,
                phase,
                force: None,
            });
        }
        // A no's fix is new code, unreviewed: it goes through a pass of the
        // review again before CI, whatever other ruling this answers.
        (Answer::No(note), _) => {
            return Ok(Move::Turn {
                prompt: note_prompt(ruling.pull_request, &note),
                phase: Phase::Implement,
                force: Some(Phase::Review(Review::first())),
            });
        }
        (Answer::Yes, RulingKind::Merge { head, .. }) => Phase::Merge {
            head,
            readied: None,
            auto: false,
        },
        // A red run, or a catch-up, that interrupted a review round goes back
        // to that round, which no reviewer has finished reading.
        (
            Answer::Yes,
            RulingKind::Stuck(
                Stuck::StillRed {
                    review: Some(review),
                    ..
                }
                | Stuck::Rebase {
                    review: Some(review),
                    ..
                },
            ),
        ) => Phase::Review(review),
        (
            Answer::Yes,
            RulingKind::Stuck(
                Stuck::Rebase { .. }
                | Stuck::StillRed { .. }
                | Stuck::MergeRefused { .. }
                | Stuck::PushRefused { .. },
            ),
        ) => Phase::Ci {
            head: None,
            since: now,
        },
        (Answer::Yes, RulingKind::Stuck(Stuck::Closed)) => Phase::Done {
            merged: false,
            closed: false,
        },
        // No work item is parked on it, so an answer only clears it.
        (Answer::Yes, RulingKind::Stuck(Stuck::AdoptFailed { .. })) => Phase::Implement,
        // The turn ends under the same round, which checks the worktree again.
        (
            Answer::Yes,
            RulingKind::Stuck(Stuck::Unpushed {
                files,
                head,
                pushed,
                review,
            }),
        ) => {
            return Ok(Move::Turn {
                prompt: push_prompt(&files, &head, &pushed),
                phase: Phase::Review(Review {
                    stage: ReviewStage::Pushing,
                    ..review
                }),
                force: None,
            });
        }
        // The fix ends under the same round, which runs the tests again.
        (Answer::Yes, RulingKind::Stuck(Stuck::TestStillFails { fix, prompt, .. })) => {
            let (prompt, phase) = test_fails::yes(fix, prompt);
            return Ok(Move::Turn {
                prompt,
                phase,
                force: None,
            });
        }
        // The fix ends under the same round, which checks the head again.
        (Answer::Yes, RulingKind::Stuck(Stuck::FixNotPushed { fix, prompt, .. })) => {
            let Fix::Review(review) = fix;
            return Ok(Move::Turn {
                prompt,
                phase: Phase::Review(review),
                force: None,
            });
        }
    };
    Ok(Move::Phase(phase))
}

// Whether a merge ruling warns of what only a new pass of the review clears:
// a pass no reviewer read, a listed bot's threads left open, or a head no
// round read and no fix turn of kelpie's pushed.
pub(super) fn needs_pass(kind: &RulingKind) -> bool {
    matches!(
        kind,
        RulingKind::Merge { unreviewed, open_threads, unread_head, .. }
            if unreviewed.is_some() || open_threads.is_some() || *unread_head
    )
}

#[cfg(test)]
mod no_fix;
#[cfg(test)]
mod tests;
