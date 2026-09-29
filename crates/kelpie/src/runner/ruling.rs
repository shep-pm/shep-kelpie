//! Rulings: raising one, and the maintainer's answer
//!
//! A ruling is saved before its comment is posted, so a comment that fails
//! loses nothing: the ruling stays in status and in the log. Its question
//! carries the exact triggers that answer it. What a yes does depends on
//! the ruling; a no's note, or an answer to the worker's question, is
//! always the worker's next turn.

use std::fmt;

use super::Runner;
use super::gate::short;
use super::report::{Begin, StepReport};
use super::rework::HUMAN;
use crate::ports::Timestamp;
use crate::settings::MergeAuthority;
use crate::state::{Fix, ProjectState, Resume, Ruling, RulingKind, StateError};
use crate::work_item::{CodeRabbitStage, Known, Phase, Review, Turn, WorkItem, foreign_change};
use crate::worktree;

/// The prompt for a turn resumed after the maintainer accepts a timed-out
/// turn's ruling with a yes
const TIMEOUT_CONTINUE: &str = "Kelpie stopped your last turn: it ran past its ceiling. \
                                Carry on with the work item from where you left off.";

/// The prompt for a turn resumed after the maintainer accepts a failed
/// turn's ruling with a yes
const FAILED_CONTINUE: &str = "Your last turn failed before it finished. \
                               Carry on with the work item from where you left off.";

/// The maintainer's answer to a ruling
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Go ahead: what that means depends on the ruling
    Yes,
    /// Do not, and send the worker this note
    No(String),
    /// The answer to the worker's question
    Text(String),
}

/// Why an answer was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleError {
    /// No pending ruling has this id
    NoSuchRuling(u64),
    /// The ruling is the worker's question, and was given a yes or a no
    WantsAnswer(u64),
    /// The ruling is not a question, and was given an answer
    NotAQuestion(u64),
    /// The `ready-for-human` label could not come off this pull request
    Unlabel(u64, String),
    /// The worktree could not be brought to the head a yes accepted
    Adopt(String, String),
    /// The answer could not be saved
    State(StateError),
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchRuling(id) => write!(f, "no ruling {id} is pending"),
            Self::WantsAnswer(id) => write!(
                f,
                "ruling {id} is the worker's question, so it takes an answer, not a yes or no"
            ),
            Self::NotAQuestion(id) => write!(
                f,
                "ruling {id} is not a question, so it takes a yes, or a no with a note"
            ),
            Self::Unlabel(number, e) => {
                write!(f, "cannot take the `{HUMAN}` label off #{number}: {e}")
            }
            Self::Adopt(head, e) => {
                write!(f, "cannot bring the worktree to {}: {e}", short(head))
            }
            Self::State(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RuleError {}

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
    /// A yes on a failed turn: the turn goes back as it stood, under its phase
    Retry { turn: Turn, phase: Phase },
    /// A yes on a foreign change: kelpie adopts it. A new head goes
    /// through the qwen-review loop, and anything else back to CI.
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
    pub fn rule(&mut self, id: u64, answer: Answer) -> Result<(), RuleError> {
        let at = self.state.rulings.iter().position(|r| r.id == id);
        let at = at.ok_or(RuleError::NoSuchRuling(id))?;
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let ruling = next.rulings.remove(at);
        let lifts_cap = matches!(
            (&answer, &ruling.kind),
            (Answer::Yes, RulingKind::CodeRabbitCap { .. })
        );
        let accepts = match (&answer, &ruling.kind) {
            (Answer::Yes, RulingKind::ClaudeFiles { head, .. }) => Some(head.clone()),
            _ => None,
        };
        // These ask the maintainer to fix the branch, so a yes vouches for its head.
        let vouches = matches!(
            (&answer, &ruling.kind),
            (
                Answer::Yes,
                RulingKind::Rebase { .. } | RulingKind::StillRed { .. }
            )
        );
        // Any answer to a refused merge gives the next one its catch-up again.
        let refusal_answered = matches!(ruling.kind, RulingKind::MergeRefused { .. });
        // Only the ruling the work item is parked on moves it. Any other,
        // which nothing leaves behind today, is answered by clearing it.
        let parked_on = |item: &WorkItem| item.phase == Phase::Ruling { id };
        let parked = next.work_item.as_ref().filter(|item| parked_on(item));
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
            let tip = worktree::origin_head(&self.settings.repo, &item.branch)
                .map_err(|e| RuleError::Adopt(to.clone(), e.to_string()))?;
            if tip != *to {
                let change = foreign_change(&item.known, &seen.labels, seen.ready, &tip);
                return self.ask_again(next, ruling.pull_request, change, now);
            }
        }
        let moved = decide(id, answer, ruling, now, head_moved)?;
        if let Some(item) = next.work_item.as_mut().filter(|item| parked_on(item)) {
            item.coderabbit.cap_cleared |= lifts_cap;
            if accepts.is_some() {
                item.claude_files_accepted = accepts;
            }
            let regate = match (vouches, self.settings.merge_authority) {
                (true, MergeAuthority::Auto) => regate(&self.settings.repo, item)?,
                (true, MergeAuthority::Ask) => {
                    item.known.head = None;
                    false
                }
                (false, _) => false,
            };
            if refusal_answered {
                item.merge_refused = false;
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
                Move::Accept(known) => {
                    let (from, to) = (item.known.head.take(), known.head.clone());
                    item.phase = match (from, to) {
                        (Some(from), Some(to)) if from != to => {
                            let (repo, branch) = (&self.settings.repo, &item.branch);
                            worktree::adopt(repo, &item.worktree, branch, &from, &to)
                                .map_err(|e| RuleError::Adopt(to, e.to_string()))?;
                            item.coderabbit.satisfied = false;
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
                    let (repo, branch) = (&self.settings.repo, &item.branch);
                    worktree::adopt(repo, &item.worktree, branch, &from, &to)
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
                item.turn = turn;
                item.phase = phase;
                // A turn the ruling interrupted may still owe the review loop.
                item.resume = force.or(item.resume.take());
                // The worker's turn again, so the hand-back label comes off.
                if let Some(number) = item.pull_request
                    && item.known.labels.iter().any(|l| l == HUMAN)
                {
                    let repo = &self.settings.forge;
                    let off = self.ports.forge.set_label(repo, number, HUMAN, false);
                    off.map_err(|e| RuleError::Unlabel(number, e.to_string()))?;
                    item.known.labels.retain(|l| l != HUMAN);
                }
            }
        }
        self.save(next).map_err(RuleError::State)
    }

    // The branch moved again while the ruling waited, so the answer was about
    // a head that is gone. What stands now is asked about afresh, or, when
    // nothing outside kelpie is left, the gate looks again.
    fn ask_again(
        &mut self,
        mut next: ProjectState,
        number: Option<u64>,
        change: Option<(Known, String)>,
        now: Timestamp,
    ) -> Result<(), RuleError> {
        let Some((known, description)) = change else {
            if let Some(item) = next.work_item.as_mut() {
                item.phase = Phase::Ci {
                    head: None,
                    since: now,
                };
            }
            return self.save(next).map_err(RuleError::State);
        };
        let kind = RulingKind::ForeignChange { description, known };
        let (_, id, _) = park(self.project.as_str(), &mut next, number, kind);
        self.save(next).map_err(RuleError::State)?;
        // A comment that fails loses nothing: the ruling is saved and alerted.
        let _ = self.post_ruling(number, id);
        Ok(())
    }

    // Saves the ruling and parks the worker on it, then posts it on pull
    // request `number`.
    pub(super) fn raise(&mut self, number: u64, kind: RulingKind) -> Result<Begin, StateError> {
        let mut next = self.state.clone();
        let (issue, id, question) = park(self.project.as_str(), &mut next, Some(number), kind);
        self.save(next)?;
        let comment_failed = self.post_ruling(Some(number), id);
        Ok(Begin::Report(StepReport::Ruling {
            issue,
            pull_request: number,
            id,
            question,
            comment_failed,
        }))
    }

    // Posts a saved ruling on its pull request, if it has one and has
    // anything to say there, and returns why the comment failed, if it did.
    // The pull request carries no question: the webhook and the relay do.
    pub(super) fn post_ruling(&self, number: Option<u64>, id: u64) -> Option<String> {
        let number = number?;
        let ruling = self.state.rulings.iter().find(|r| r.id == id)?;
        let comment = comment(&ruling.kind)?;
        let posted = self
            .ports
            .forge
            .comment(&self.settings.forge, number, &comment);
        posted.err().map(|e| e.to_string())
    }

    pub(super) fn update(&mut self, change: impl FnOnce(&mut WorkItem)) -> Result<(), StateError> {
        let mut next = self.state.clone();
        change(
            next.work_item
                .as_mut()
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
    let tip = worktree::origin_head(repo, &item.branch)
        .map_err(|e| RuleError::Adopt(known.clone().unwrap_or_default(), e.to_string()))?;
    let from = match known {
        Some(known) => known,
        None => worktree::head(repo, &item.worktree)
            .map_err(|e| RuleError::Adopt(tip.clone(), e.to_string()))?,
    };
    if from == tip && item.known.head.is_some() {
        return Ok(false);
    }
    worktree::adopt(repo, &item.worktree, &item.branch, &from, &tip)
        .map_err(|e| RuleError::Adopt(tip.clone(), e.to_string()))?;
    item.known.head = Some(tip);
    item.coderabbit.satisfied = false;
    Ok(true)
}

/// Adds a ruling to `next` and parks its work item on it
///
/// Returns the work item's issue, and the ruling's id and question.
pub(super) fn park(
    project: &str,
    next: &mut ProjectState,
    pull_request: Option<u64>,
    kind: RulingKind,
) -> (u64, u64, String) {
    let id = next.last_ruling + 1;
    let item = next
        .work_item
        .as_mut()
        .expect("a ruling is about a work item");
    item.phase = Phase::Ruling { id };
    let issue = item.issue;
    let text = question(project, id, issue, pull_request, &kind);
    next.last_ruling = id;
    next.rulings.push(Ruling {
        id,
        question: text.clone(),
        pull_request,
        kind,
        alerted: false,
    });
    (issue, id, text)
}

// What a reader of the pull request is told of a ruling: what happened and
// that it waits on the maintainer, with no command and nothing of kelpie's.
// A merge ruling says nothing, since `ready-for-human` already does.
fn comment(kind: &RulingKind) -> Option<String> {
    let said = match kind {
        RulingKind::Merge { .. } => return None,
        RulingKind::Rebase { reason } => {
            format!("This branch could not be rebased onto main: {reason}.")
        }
        RulingKind::StillRed { head, checks } => format!(
            "CI failed again at {} and no fix was pushed: {}.",
            short(head),
            checks.join(", ")
        ),
        // The forge's own words stay off a public pull request.
        RulingKind::MergeRefused { head, .. } => {
            format!("Merging at {} was refused twice.", short(head))
        }
        RulingKind::Closed => "This pull request was closed without merging.".to_owned(),
        RulingKind::ReviewGuard { review } => format!(
            "The review of this pull request has run {} rounds without settling.",
            review.round.saturating_sub(1)
        ),
        RulingKind::FixNotPushed { .. } => {
            "A fix for review findings ended without a push, so those findings still hold."
                .to_owned()
        }
        RulingKind::CodeRabbitCap { rounds, held, .. } => format!(
            "CodeRabbit has run {rounds} rounds here, its cap, \
             and {held} of its findings still hold."
        ),
        RulingKind::CodeRabbitSilent { head } => {
            format!("CodeRabbit never reviewed {}.", short(head))
        }
        RulingKind::Question { asked, .. } => asked.clone(),
        RulingKind::TurnTimeout { .. } => {
            "The work on this pull request ran too long and was stopped.".to_owned()
        }
        RulingKind::TurnFailed { .. } => {
            "The work on this pull request hit an error and stopped.".to_owned()
        }
        RulingKind::ClaudeFiles { files, .. } => format!(
            "This pull request changes Claude Code's own files: {}.",
            files.join(", ")
        ),
        RulingKind::ForeignChange { description, .. } => {
            format!("This pull request was changed: {description}.")
        }
    };
    Some(format!("{said}\n\nWaiting on the maintainer."))
}

// A yes, a no or an answer that does not fit the ruling is refused.
fn decide(
    id: u64,
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
    let phase = match (answer, ruling.kind) {
        (Answer::Text(text), RulingKind::Question { resume, .. }) => {
            // A question resumes exactly where it interrupted the qwen-review
            // loop; one asked before the loop ever started, with a pull
            // request already open, starts it once answered instead of the
            // ordinary rule of going straight to CI.
            let (phase, force) = match resume {
                Resume::Nothing => (Phase::Implement, None),
                Resume::ReviewFirst => (Phase::Implement, Some(Phase::Review(Review::first()))),
                Resume::Review(review) => (Phase::Review(review), None),
                Resume::CodeRabbitFix { head } => (fixing(Some(head)), None),
            };
            return Ok(Move::Turn {
                prompt: answer_prompt(&text),
                phase,
                force,
            });
        }
        (_, RulingKind::Question { .. }) => return Err(RuleError::WantsAnswer(id)),
        (Answer::Text(_), _) => return Err(RuleError::NotAQuestion(id)),
        // A fix turn resumes in its round, which checks it pushed.
        (Answer::Yes, RulingKind::TurnTimeout { phase }) => {
            return Ok(Move::Turn {
                prompt: TIMEOUT_CONTINUE.to_owned(),
                phase: phase.unwrap_or(Phase::Implement),
                force: None,
            });
        }
        // A turn that had started is resumed with a prompt of its own, and
        // the yes starts its ceiling afresh: the time it spent failing and
        // waiting is not held against it.
        (Answer::Yes, RulingKind::TurnFailed { phase, retry, .. }) => {
            let turn = match retry {
                Turn::Running { .. } => Turn::Next {
                    prompt: FAILED_CONTINUE.to_owned(),
                },
                other => other,
            };
            return Ok(Move::Retry { turn, phase });
        }
        // A worker cannot write Claude Code's own files, so a note would not help it.
        (
            Answer::No(_),
            RulingKind::TurnTimeout { .. }
            | RulingKind::TurnFailed { .. }
            | RulingKind::ClaudeFiles { .. },
        ) => Phase::Done { merged: false },
        (Answer::Yes, RulingKind::ClaudeFiles { phase, .. }) => phase,
        (Answer::Yes, RulingKind::ForeignChange { known, .. }) => return Ok(Move::Accept(known)),
        // A no's fix is new code, unreviewed: it goes through the
        // qwen-review loop again before CI, whatever ruling this answers.
        (Answer::No(note), _) => {
            return Ok(Move::Turn {
                prompt: note_prompt(ruling.pull_request, &note),
                phase: Phase::Implement,
                force: Some(Phase::Review(Review::first())),
            });
        }
        (Answer::Yes, RulingKind::Merge { head }) => Phase::Merge {
            head,
            readied: None,
            auto: false,
        },
        (
            Answer::Yes,
            RulingKind::Rebase { .. }
            | RulingKind::StillRed { .. }
            | RulingKind::MergeRefused { .. },
        ) => Phase::Ci {
            head: None,
            since: now,
        },
        (Answer::Yes, RulingKind::Closed) => Phase::Done { merged: false },
        (Answer::Yes, RulingKind::ReviewGuard { review }) => Phase::Review(Review {
            guard_cleared: true,
            ..review
        }),
        // The fix ends under the same round, which checks the head again.
        (Answer::Yes, RulingKind::FixNotPushed { fix, prompt }) => {
            let phase = match fix {
                Fix::Review(review) => Phase::Review(review),
                Fix::CodeRabbit { head, .. } => fixing(Some(head)),
            };
            return Ok(Move::Turn {
                prompt,
                phase,
                force: None,
            });
        }
        (Answer::Yes, RulingKind::CodeRabbitCap { prompt, head, .. }) => {
            return Ok(Move::Turn {
                prompt,
                phase: fixing(head),
                force: None,
            });
        }
        (Answer::Yes, RulingKind::CodeRabbitSilent { .. }) => Phase::Ci {
            head: None,
            since: now,
        },
    };
    Ok(Move::Phase(phase))
}

fn question(project: &str, id: u64, issue: u64, number: Option<u64>, kind: &RulingKind) -> String {
    let trigger = |answer: &str| format!("`shep trigger {project} rule '{id} {answer}'`");
    let (yes, no) = (trigger("yes"), trigger("no <note>"));
    let about = number.map_or_else(
        || format!("issue #{issue}"),
        |n| format!("pull request #{n}"),
    );
    let ask = match kind {
        RulingKind::Merge { head } => {
            format!(
                "Merge {about} at {} into main? {yes} merges it",
                short(head)
            )
        }
        RulingKind::Rebase { reason } => format!(
            "Kelpie cannot rebase {about} onto main: {reason}. \
             Once the branch is fixed, {yes} has kelpie look again"
        ),
        RulingKind::StillRed { head, checks } => format!(
            "CI failed again on {about} at {}, and the worker pushed \
             no fix: {}. {yes} has kelpie look again",
            short(head),
            checks.join(", ")
        ),
        RulingKind::MergeRefused { head, reason } => format!(
            "Kelpie could not merge {about} at {} after catching it up: {reason}. \
             {yes} has kelpie look again and merge once every gate passes",
            short(head)
        ),
        RulingKind::Closed => format!(
            "{} was closed without merging. {yes} drops the work \
             item and keeps its branch on the forge",
            capitalized(&about)
        ),
        RulingKind::ReviewGuard { review } => format!(
            "The qwen-review loop on {about} has run {} rounds without \
             settling. {yes} lets it keep going",
            review.round.saturating_sub(1)
        ),
        RulingKind::FixNotPushed { fix, .. } => {
            let round = match fix {
                Fix::Review(review) => format!("round {} of the qwen-review loop", review.round),
                Fix::CodeRabbit { round, .. } => format!("CodeRabbit round {round}"),
            };
            format!(
                "The worker on {about} ended its fix for {round} without pushing, \
                 so those findings still hold. {yes} sends it the findings again"
            )
        }
        RulingKind::CodeRabbitCap { rounds, held, .. } => format!(
            "CodeRabbit has run {rounds} rounds on {about}, its cap, and the judge \
             still holds {held} of its findings. {yes} sends the worker those \
             findings and lets the rounds go past the cap"
        ),
        RulingKind::CodeRabbitSilent { head } => format!(
            "CodeRabbit never reviewed {about} at {} after kelpie summoned it. \
             {yes} has kelpie look at CI and summon it again",
            short(head)
        ),
        RulingKind::Question { asked, .. } => {
            return format!(
                "The worker on {about} asks:\n\n{asked}\n\n{} sends the worker your answer.",
                trigger("answer <text>")
            );
        }
        RulingKind::TurnTimeout { .. } => {
            return format!(
                "The worker on {about} has been running past its turn's ceiling, \
                 and kelpie stopped it. {yes} resumes its session for another turn, \
                 and {no} stops the work item, keeping its branch and pull request \
                 on the forge."
            );
        }
        RulingKind::TurnFailed { reason, .. } => {
            return format!(
                "The worker's turn on {about} failed: {}. {yes} tries that step again, \
                 and {no} stops the work item, keeping its branch and pull request \
                 on the forge.",
                reason.trim()
            );
        }
        RulingKind::ClaudeFiles { head, files, .. } => {
            return format!(
                "{} at {} changes Claude Code's own files, which run outside the \
                 worker's sandbox: {}. {yes} accepts them at that head and kelpie \
                 carries on, and {no} stops the work item, keeping its branch and \
                 pull request on the forge.",
                capitalized(&about),
                short(head),
                files.join(", ")
            );
        }
        RulingKind::ForeignChange { description, .. } => {
            return format!(
                "{} changed outside kelpie: {description}. {yes} accepts it and kelpie \
                 carries on, and {no} sends the worker your note.",
                capitalized(&about)
            );
        }
    };
    format!("{ask}, and {no} sends the worker your note.")
}

// A CodeRabbit fix ends back in its round, which checks it moved `head`.
// With no head, from an older state file, it ends under Implement and
// goes straight to CI.
fn fixing(head: Option<String>) -> Phase {
    head.map_or(Phase::Implement, |head| {
        Phase::CodeRabbit(CodeRabbitStage::Fixing { head })
    })
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn note_prompt(number: Option<u64>, note: &str) -> String {
    let about = number.map_or_else(
        || "your work item".to_owned(),
        |n| format!("pull request #{n}"),
    );
    format!("The maintainer answered no on {about}, with this note:\n\n{note}\n")
}

fn declined_prompt(number: Option<u64>, head: &str, note: &str) -> String {
    let about = number.map_or_else(
        || "your branch".to_owned(),
        |n| format!("pull request #{n}"),
    );
    format!(
        "Someone other than you pushed commit {} to {about}, and the maintainer \
         declined it, with this note:\n\n{note}\n\nYour worktree is now at that commit. \
         Revert or change it with a new commit on top, and push with \
         `git push origin HEAD`. Do not force-push.\n",
        short(head)
    )
}

fn answer_prompt(text: &str) -> String {
    format!("The maintainer answered your question:\n\n{text}\n")
}

#[cfg(test)]
mod tests {

    use serde_json::json;

    use super::*;
    use crate::ports::{Checks, Session};
    use crate::runner::gate::CHECKS_SETTLE;
    use crate::runner::step;
    use crate::test::{Rig, Scripted};

    // The pull request is public: it gets what happened and who it waits on,
    // and never a command, which the webhook and the relay carry instead.
    #[test]
    fn a_ruling_on_the_pull_request_names_no_command_and_a_merge_says_nothing() {
        let review = Review::first();
        let known = Known {
            labels: vec![],
            ready: false,
            head: None,
        };
        let kinds = [
            RulingKind::Rebase {
                reason: "conflict in a.txt".into(),
            },
            RulingKind::StillRed {
                head: "abcdef123".into(),
                checks: vec!["test".into(), "lint".into()],
            },
            RulingKind::Closed,
            RulingKind::ReviewGuard {
                review: review.clone(),
            },
            RulingKind::FixNotPushed {
                fix: Fix::Review(review),
                prompt: "fix it".into(),
            },
            RulingKind::CodeRabbitCap {
                rounds: 3,
                held: 2,
                prompt: "fix it".into(),
                head: None,
            },
            RulingKind::CodeRabbitSilent {
                head: "abcdef123".into(),
            },
            RulingKind::Question {
                asked: "Which flag?".into(),
                resume: Resume::Nothing,
            },
            RulingKind::TurnTimeout { phase: None },
            RulingKind::TurnFailed {
                reason: "boom".into(),
                phase: Phase::Implement,
                retry: Turn::Next { prompt: "x".into() },
            },
            RulingKind::ForeignChange {
                description: "the `bug` label was added".into(),
                known,
            },
        ];
        for kind in kinds {
            let said = comment(&kind).unwrap_or_default();
            assert!(said.ends_with("\n\nWaiting on the maintainer."), "{said}");
            for internal in ["shep trigger", "rule '", "ruling", "yes", "<note>"] {
                assert!(!said.contains(internal), "{internal} in {said}");
            }
        }
        assert_eq!(comment(&RulingKind::Merge { head: "abc".into() }), None);
    }

    #[test]
    fn nothing_merges_without_a_yes() {
        let (rig, runner, _) = Rig::parked("shep");
        assert_eq!(step(&runner).unwrap(), Some(StepReport::Alerted { id: 1 }));
        for _ in 0..5 {
            rig.clock.advance(3600);
            assert_eq!(step(&runner).unwrap(), None);
        }
        let refused = [
            ("2 yes", "no ruling 2 is pending"),
            (
                "1 no",
                "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"1 no\"",
            ),
            (
                "1 yes please",
                "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"1 yes please\"",
            ),
            (
                "one yes",
                "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"one yes\"",
            ),
            (
                "1 maybe",
                "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`, not \"1 maybe\"",
            ),
        ];
        for (params, error) in refused {
            assert_eq!(
                rig.ask(&runner, "rule", Some(params)),
                json!({ "error": error }),
                "{params}"
            );
            assert_eq!(step(&runner).unwrap(), None, "{params}");
        }
        assert_eq!(rig.forge.merges(), []);
        assert!(rig.forge.readied().is_empty());
        assert!(rig.worktree_7().exists());
    }

    // A no's fix is new code the loop has not seen: it goes back through
    // the qwen-review loop, not straight to CI, before the next ruling.
    #[test]
    fn a_no_sends_the_note_to_the_worker_and_it_goes_through_review_before_the_next_ruling() {
        let (rig, runner, _) = Rig::parked("rotom");
        rig.ask(&runner, "rule", Some("1 no  rename the flag to --dry-run "));
        rig.claude.script([
            Scripted::Push("rename.txt", "renamed\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the noted turn: pushes, enters round 1
        let [first, noted] = rig.claude.calls().try_into().unwrap();
        assert_eq!(noted.session, Session::Resume(first.session.id().clone()));
        assert_eq!(
            noted.prompt,
            "The maintainer answered no on pull request #71, with this note:\n\n\
             rename the flag to --dry-run\n"
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"]["state"],
            "review",
            "a no's fix resumes the loop, not CI directly"
        );

        let pushed = rig.forge.head_of("kelpie/7").unwrap();
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        assert_eq!(
            step(&runner).unwrap(),
            None,
            "CI on the new head is pending"
        );
        rig.forge.set_checks(&pushed, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 2, .. })
        ));
        let rulings = &rig.ask(&runner, "status", None)["rulings"];
        assert_eq!(
            rulings[0]["kind"],
            json!({ "kind": "merge", "head": pushed })
        );
        assert_eq!(rulings.as_array().unwrap().len(), 1);
        assert_eq!(rig.forge.merges(), []);
    }

    #[test]
    fn ruling_ids_are_never_given_twice() {
        let (rig, runner, _) = Rig::parked("rotom");
        rig.ask(&runner, "rule", Some("1 yes"));
        step(&runner).unwrap();
        rig.clock.advance(CHECKS_SETTLE);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Finished { merged: true, .. })
        ));
        drop(runner);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(72, "kelpie/7", &[7]);
        rig.claude.script([
            Scripted::Push("again.txt", "again\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the worker's first turn: opens the pull request
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        let head = rig.forge.head_of("kelpie/7").unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 2, .. })
        ));
        assert_eq!(
            rig.ask(&runner, "rule", Some("1 yes")),
            json!({ "error": "no ruling 1 is pending" })
        );
    }

    #[test]
    fn a_no_on_a_commit_pushed_by_hand_has_the_worker_build_on_it_without_force() {
        let (rig, runner, _) = Rig::with_pull_request("rotom");
        let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 1, .. })
        ));
        rig.ask(&runner, "rule", Some("1 no revert it"));
        // A plain push from the worktree: the stand-in panics if it is refused.
        rig.claude.script([
            Scripted::Push("revert.txt", "reverted\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap();
        let noted = rig.claude.calls().pop().unwrap();
        assert_eq!(
            noted.prompt,
            format!(
                "Someone other than you pushed commit {} to pull request #71, and the \
                 maintainer declined it, with this note:\n\nrevert it\n\nYour worktree is \
                 now at that commit. Revert or change it with a new commit on top, and \
                 push with `git push origin HEAD`. Do not force-push.\n",
                &by_hand[..7]
            )
        );
        let pushed = rig.forge.head_of("kelpie/7").unwrap();
        let parent = crate::test::git(&rig.worktree_7(), &["rev-parse", "HEAD^"]);
        assert_eq!(parent, by_hand);

        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        rig.forge.set_checks(&pushed, Checks::Passed);
        let Some(StepReport::Ruling {
            id: 2, question, ..
        }) = rig.verdict(&runner)
        else {
            panic!("no second ruling");
        };
        assert!(question.starts_with("Merge pull request #71"), "{question}");
    }

    #[test]
    fn a_yes_on_a_head_the_branch_moved_past_asks_about_the_new_head_instead() {
        let (rig, runner, head) = Rig::with_pull_request("koji");
        rig.push_by_hand("kelpie/7", "first.txt");
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 1, .. })
        ));
        let second = rig.push_by_hand("kelpie/7", "second.txt");
        let status = rig.ask(&runner, "rule", Some("1 yes"));
        assert_eq!(
            status["work_item"]["phase"],
            json!({ "state": "ruling", "id": 2 })
        );
        assert_eq!(status["rulings"][0]["kind"]["known"]["head"], json!(second));
        let question = status["rulings"][0]["question"].as_str().unwrap();
        assert!(
            question.starts_with(&format!(
                "Pull request #71 changed outside kelpie: its head moved to {}",
                &second[..7]
            )),
            "{question}"
        );
        assert_eq!(
            crate::test::git(&rig.worktree_7(), &["rev-parse", "HEAD"]),
            head
        );
    }

    #[test]
    fn a_yes_on_a_head_is_refused_while_the_worktree_holds_work_not_pushed() {
        let (rig, runner, head) = Rig::with_pull_request("chelone");
        let by_hand = rig.push_by_hand("kelpie/7", "by-hand.txt");
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 1, .. })
        ));
        std::fs::write(rig.worktree_7().join("work.txt"), "unsaved\n").unwrap();
        let reply = rig.ask(&runner, "rule", Some("1 yes"));
        let error = reply["error"].as_str().unwrap();
        assert!(
            error.starts_with(&format!("cannot bring the worktree to {}: ", &by_hand[..7])),
            "{error}"
        );
        assert_eq!(
            rig.ask(&runner, "status", None)["work_item"]["phase"],
            json!({ "state": "ruling", "id": 1 })
        );
        assert_eq!(
            crate::test::git(&rig.worktree_7(), &["rev-parse", "HEAD"]),
            head
        );

        // A commit kelpie never saw pushed is the worker's too.
        let worktree = rig.worktree_7();
        crate::test::git(&worktree, &["commit", "--quiet", "-am", "not pushed"]);
        let reply = rig.ask(&runner, "rule", Some("1 yes"));
        assert!(
            reply["error"].as_str().unwrap().contains("holds work"),
            "{reply}"
        );
    }
}
