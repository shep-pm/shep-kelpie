//! Rulings: raising one, and the maintainer's answer
//!
//! A ruling is saved before its comment is posted, so a comment that fails
//! loses nothing: the ruling stays in status and in the log. Its question
//! carries the exact triggers that answer it. What a yes does depends on
//! the ruling; a no's note, or an answer to the worker's question, is
//! always the worker's next turn.

use std::fmt;

use super::gate::short;
use super::report::{Begin, StepReport};
use super::review::pushed::{off_text, push_prompt};
use super::rework::HUMAN;
use super::{Names, Runner};
use crate::ports::Timestamp;
use crate::settings::Merging;
use crate::state::{Fix, ProjectState, Resume, Ruling, RulingKind, StateError, Stuck};
use crate::work_item::{Known, Phase, Review, ReviewStage, Turn, WorkItem, foreign_change};
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
    /// Do not, and send the worker this note. On a merge ruling its fix
    /// goes to CI and back to the ruling.
    No(String),
    /// On a merge ruling only: send the worker this note, and its change
    /// through a new pass of the review
    Rework(String),
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
    /// The ruling is not a merge ruling, and was given a rework
    NotAMerge(u64),
    /// The ruling asks for an issue's label, which no note can give, and
    /// was given a no
    YesOnly(u64),
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
            Self::Adopt(head, e) => {
                write!(f, "cannot bring the worktree to {}: {e}", short(head))
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
    pub fn rule(&mut self, id: u64, answer: Answer) -> Result<(), RuleError> {
        let at = self.state.rulings.iter().position(|r| r.id == id);
        let at = at.ok_or(RuleError::NoSuchRuling(id))?;
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let ruling = next.rulings.remove(at);
        // Whether the findings a merged pull request left are filed or dropped.
        let follow_up = match (&answer, &ruling.kind) {
            (Answer::Yes, RulingKind::FollowUp { .. }) => Some(true),
            (Answer::No(_), RulingKind::FollowUp { .. }) => Some(false),
            _ => None,
        };
        let accepts = match (&answer, &ruling.kind) {
            (Answer::Yes, RulingKind::AgentFiles { head, .. }) => Some(head.clone()),
            _ => None,
        };
        // A merge ruling's no whose fix goes to CI with no pass of the review.
        // Any answer to a merge ruling replaces what an earlier no left.
        let merge = matches!(ruling.kind, RulingKind::Merge { .. });
        let noted_from = match (&answer, &ruling.kind) {
            (Answer::No(_), RulingKind::Merge { head, .. }) if !needs_pass(&ruling.kind) => {
                Some(head.clone())
            }
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
        // Any answer to a refused merge gives the next one its catch-up again.
        let refusal_answered = matches!(ruling.kind, RulingKind::Stuck(Stuck::MergeRefused { .. }));
        // Only the ruling the work item is parked on moves it. Any other,
        // which nothing leaves behind today, is answered by clearing it.
        let parked_on = |item: &WorkItem| item.phase == Phase::Ruling { id };
        let parked = next.work_items.iter().find(|item| parked_on(item));
        // The answer moves only the work item it parks, whichever is open.
        self.focus = parked.map(|item| item.issue);
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
            let tip = worktree::origin_head(&self.settings.git.checkout, &item.branch)
                .map_err(|e| RuleError::Adopt(to.clone(), e.to_string()))?;
            if tip != *to {
                let change = foreign_change(&item.known, &seen.labels, seen.ready, &tip);
                let issue = item.issue;
                return self.ask_again(next, issue, ruling.pull_request, change, now);
            }
        }
        let moved = decide(id, answer, ruling, now, head_moved)?;
        if let Some(item) = next.work_items.iter_mut().find(|item| parked_on(item)) {
            if let (Some(filed), Some(pending)) = (follow_up, item.follow_ups.as_mut()) {
                if filed {
                    pending.ruled = true;
                    pending.first_refused = None;
                } else {
                    pending.findings.clear();
                }
            }
            if accepts.is_some() {
                item.claude_files_accepted = accepts;
            }
            if merge {
                item.noted_from = noted_from;
                item.late_from = None;
                self.late_reads.remove(&item.issue);
            }
            let regate = match (vouches, self.settings.git.merging) {
                (true, Merging::Auto) => regate(&self.settings.git.checkout, item)?,
                (true, Merging::Ask) => {
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
                            let (repo, branch) = (&self.settings.git.checkout, &item.branch);
                            worktree::adopt(repo, &item.worktree, branch, &from, &to)
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
        self.save(next).map_err(RuleError::State)
    }

    // The branch moved again while the ruling waited, so the answer was about
    // a head that is gone. What stands now is asked about afresh, or, when
    // nothing outside kelpie is left, the gate looks again.
    fn ask_again(
        &mut self,
        mut next: ProjectState,
        issue: u64,
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
        let _ = self.post_ruling(number, id);
        Ok(())
    }

    // Saves the ruling and parks the worker on it, then posts it on pull
    // request `number`.
    pub(super) fn raise(&mut self, number: u64, kind: RulingKind) -> Result<Begin, StateError> {
        let mut next = self.state.clone();
        let issue = self.current().expect("a ruling is about a work item").issue;
        let (id, question) = park(self.names(), &mut next, issue, Some(number), kind);
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
    // The pull request carries no question: the webhook does.
    pub(super) fn post_ruling(&self, number: Option<u64>, id: u64) -> Option<String> {
        let number = number?;
        let ruling = self.state.rulings.iter().find(|r| r.id == id)?;
        let comment = comment(&ruling.kind)?;
        let posted = self.ports.forge.comment(&self.remote, number, &comment);
        posted.err().map(|e| e.to_string())
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
    Ok(true)
}

/// Adds a ruling to `next` and parks the work item for `issue` on it
///
/// Returns the ruling's id and question.
pub(super) fn park(
    names: Names<'_>,
    next: &mut ProjectState,
    issue: u64,
    pull_request: Option<u64>,
    kind: RulingKind,
) -> (u64, String) {
    let id = names.ids.claim(names.project, next.last_ruling);
    let item = next
        .item_mut(issue)
        .expect("a ruling is about an open work item");
    item.phase = Phase::Ruling { id };
    item.counts.rulings = item.counts.rulings.saturating_add(1);
    let text = question(id, issue, pull_request, &kind);
    next.last_ruling = id;
    next.rulings.push(Ruling {
        id,
        issue: Some(issue),
        question: text.clone(),
        pull_request,
        kind,
        alerted: false,
    });
    (id, text)
}

// What a reader of the pull request is told of a ruling: what happened and
// that it waits on the maintainer, with no command and nothing of kelpie's.
// A merge ruling says nothing, since `ready-for-human` already does.
fn comment(kind: &RulingKind) -> Option<String> {
    let said = match kind {
        RulingKind::Merge { .. } => return None,
        RulingKind::Stuck(reason) => stuck_comment(reason),
        RulingKind::Question { asked, .. } => asked.clone(),
        RulingKind::AgentFiles { files, .. } => format!(
            "This pull request changes agents' own files: {}.",
            files.join(", ")
        ),
        RulingKind::ForeignChange { description, .. } => {
            format!("This pull request was changed: {description}.")
        }
        // Merged and done: nothing on the pull request waits on the maintainer.
        RulingKind::FollowUp { .. } => return None,
    };
    Some(format!("{said}\n\nWaiting on the maintainer."))
}

// The fix turns a worker had on red runs. The cap is read live, so a
// lowered one can sit below the count.
fn fix_turns_had(turns: u32) -> String {
    match turns {
        0 => "The worker has had no fix turn on red runs".to_owned(),
        1 => "The worker has had 1 fix turn on red runs".to_owned(),
        n => format!("The worker has had {n} fix turns on red runs"),
    }
}

fn stuck_comment(reason: &Stuck) -> String {
    match reason {
        Stuck::Rebase { why } => {
            format!("This branch could not be rebased onto main: {why}.")
        }
        Stuck::StillRed {
            head,
            checks,
            fix_turns: None,
        } => format!(
            "CI failed again at {} and no fix was pushed: {}.",
            short(head),
            checks.join(", ")
        ),
        Stuck::StillRed {
            head,
            checks,
            fix_turns: Some(turns),
        } => format!(
            "CI failed at {}: {}. {}.",
            short(head),
            checks.join(", "),
            fix_turns_had(*turns)
        ),
        // The forge's own words stay off a public pull request.
        Stuck::MergeRefused { head, .. } => {
            format!("Merging at {} was refused twice.", short(head))
        }
        Stuck::Closed => "This pull request was closed without merging.".to_owned(),
        Stuck::LocalModelSpilled { .. } => {
            "The local model for this pull request's review is not fully on the GPU, \
             so a review round did not run."
                .to_owned()
        }
        Stuck::FixNotPushed { .. } => {
            "A fix for review findings ended without a push, so those findings still hold."
                .to_owned()
        }
        Stuck::Unpushed { .. } => {
            "Work in this pull request's worktree is not pushed, so its review waits.".to_owned()
        }
        Stuck::TurnTimeout { .. } => {
            "The work on this pull request ran too long and was stopped.".to_owned()
        }
        Stuck::Unlabelled { .. } => "No implementer was picked for this issue.".to_owned(),
        Stuck::TurnFailed { .. } => {
            "The work on this pull request hit an error and stopped.".to_owned()
        }
    }
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
        (_, RulingKind::Question { .. }) => return Err(RuleError::WantsAnswer(id)),
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
        (Answer::Rework(_), _) => return Err(RuleError::NotAMerge(id)),
        (Answer::No(_), RulingKind::Stuck(Stuck::Unlabelled { .. })) => {
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
        (Answer::Yes, RulingKind::Stuck(Stuck::TurnFailed { phase, retry, .. })) => {
            let turn = match retry {
                Turn::Running { .. } => Turn::Next {
                    prompt: FAILED_CONTINUE.to_owned(),
                },
                other => other,
            };
            return Ok(Move::Retry { turn, phase });
        }
        // A worker cannot write agents' own files, so a note would not help it.
        (
            Answer::No(_),
            RulingKind::Stuck(Stuck::TurnTimeout { .. } | Stuck::TurnFailed { .. })
            | RulingKind::AgentFiles { .. },
        ) => Phase::Done { merged: false },
        (Answer::Yes, RulingKind::AgentFiles { phase, .. }) => phase,
        (Answer::Yes, RulingKind::ForeignChange { known, .. }) => return Ok(Move::Accept(known)),
        // The pull request is merged, so a no has no worker to send a note to.
        (Answer::Yes | Answer::No(_), RulingKind::FollowUp { .. }) => Phase::Done { merged: true },
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
        (
            Answer::Yes,
            RulingKind::Stuck(
                Stuck::Rebase { .. } | Stuck::StillRed { .. } | Stuck::MergeRefused { .. },
            ),
        ) => Phase::Ci {
            head: None,
            since: now,
        },
        (Answer::Yes, RulingKind::Stuck(Stuck::Closed)) => Phase::Done { merged: false },
        // No work item is parked on it, so an answer only clears it.
        (Answer::Yes, RulingKind::Stuck(Stuck::Unlabelled { .. })) => Phase::Implement,
        (Answer::Yes, RulingKind::Stuck(Stuck::LocalModelSpilled { review, .. })) => {
            Phase::Review(review)
        }
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
        // The fix ends under the same round, which checks the head again.
        (Answer::Yes, RulingKind::Stuck(Stuck::FixNotPushed { fix, prompt })) => {
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

pub(super) fn question(id: u64, issue: u64, number: Option<u64>, kind: &RulingKind) -> String {
    let trigger = |answer: &str| format!("`shep kelpie rule {id} {answer}`");
    let (yes, no) = (trigger("yes"), trigger("no <note>"));
    let about = number.map_or_else(
        || format!("issue #{issue}"),
        |n| format!("pull request #{n}"),
    );
    // A work item stopped before its pull request opened leaves none on the forge.
    let kept = if number.is_some() {
        ", keeping its branch and pull request on the forge"
    } else {
        ""
    };
    let ask = match kind {
        RulingKind::Merge {
            head,
            unreviewed,
            open_threads,
            unread_head,
            note_fix,
            late_fix,
            nits,
        } => {
            let mut unread = unreviewed.as_ref().map_or_else(String::new, |why| {
                format!(" No reviewer read it in its last review: {why}.")
            });
            if *unread_head {
                unread.push_str(
                    " Kelpie has no record of a review round reading this head, or of a fix turn it sent pushing it.",
                );
            }
            if *note_fix {
                unread.push_str(
                    " This head is the worker's fix for your note, and no reviewer read it.",
                );
            }
            if *late_fix {
                unread.push_str(
                    " This head is the worker's fix for a review bot's late review, and no reviewer read it.",
                );
            }
            let mut open = open_threads.as_ref().map_or_else(String::new, |open| {
                format!(" Review bot threads are still open on it: {open}.")
            });
            if let Some(nits) = nits {
                open.push_str(&format!(" {nits}."));
            }
            let rework = trigger("rework <note>");
            if needs_pass(kind) {
                return format!(
                    "Merge {about} at {} into main?{unread}{open} {yes} merges it, and {no} \
                     or {rework} sends the worker your note for a change the whole review \
                     reads again, since only a new pass clears that.",
                    short(head)
                );
            }
            return format!(
                "Merge {about} at {} into main?{unread}{open} {yes} merges it, {no} sends the \
                 worker your note for a fix that goes to CI and back to you, and {rework} \
                 sends it your note for a change the whole review reads again.",
                short(head)
            );
        }
        RulingKind::Stuck(reason) => match reason {
            Stuck::Rebase { why } => format!(
                "Kelpie cannot rebase {about} onto main: {why}. \
                 Once the branch is fixed, {yes} has kelpie look again"
            ),
            Stuck::StillRed {
                head,
                checks,
                fix_turns: None,
            } => format!(
                "CI failed again on {about} at {}, and the worker pushed \
                 no fix: {}. {yes} has kelpie look again",
                short(head),
                checks.join(", ")
            ),
            Stuck::StillRed {
                head,
                checks,
                fix_turns: Some(turns),
            } => format!(
                "CI failed on {about} at {}: {}. {}, and the cap `ci.fix_attempts` sets is \
                 reached. {yes} has kelpie look again",
                short(head),
                checks.join(", "),
                fix_turns_had(*turns)
            ),
            Stuck::MergeRefused { head, why } => format!(
                "Kelpie could not merge {about} at {} after catching it up: {why}. \
                 {yes} has kelpie look again and merge once every gate passes",
                short(head)
            ),
            Stuck::Closed => format!(
                "{} was closed without merging. {yes} drops the work \
                 item and keeps its branch on the forge",
                capitalized(&about)
            ),
            Stuck::LocalModelSpilled { review, why } => format!(
                "Round {} of the review on {about} did not run: {why}. \
                 Once the model is back on the GPU, {yes} runs the round again",
                review.round
            ),
            Stuck::Unpushed {
                files,
                head,
                pushed,
                review,
            } => format!(
                "The worker on {about} still has work not pushed after its turn to push or \
                 discard it: {}. Round {} of the review did not run. {yes} gives the worker \
                 another turn to push or discard it",
                off_text(files, head, pushed),
                review.round
            ),
            Stuck::FixNotPushed { fix, .. } => {
                let Fix::Review(review) = fix;
                format!(
                    "The worker on {about} ended its fix for round {} of the review without \
                     pushing, so those findings still hold. {yes} sends it the findings again",
                    review.round
                )
            }
            Stuck::TurnTimeout { .. } => {
                return format!(
                    "The worker on {about} has been running past its turn's ceiling, \
                     and kelpie stopped it. {yes} resumes its session for another turn, \
                     and {no} stops the work item{kept}."
                );
            }
            Stuck::TurnFailed { why, .. } => {
                return format!(
                    "The worker's turn on {about} failed: {}. {yes} tries that step again, \
                     and {no} stops the work item{kept}.",
                    why.trim()
                );
            }
            Stuck::Unlabelled { why } => {
                return format!(
                    "The issue writer could not pick an implementer for {about}: {}. Label \
                     it `agent:<name>` for one `agents.implementers` lists, then answer {yes}: \
                     the board takes it once this is answered, and an issue still unlabelled \
                     goes to the issue writer again.",
                    why.trim()
                );
            }
        },
        RulingKind::Question { asked, .. } => {
            return format!(
                "The worker on {about} asks:\n\n{asked}\n\n{} sends the worker your answer.",
                trigger("<text>")
            );
        }
        RulingKind::AgentFiles { head, files, .. } => {
            return format!(
                "{} at {} changes agents' own files, which decide what \
                 an agent runs in the worktree: {}. {yes} accepts them at that head and kelpie \
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
        RulingKind::FollowUp { findings, refused } => {
            let list: Vec<String> = findings
                .iter()
                .map(|f| match f.line {
                    0 => format!("- {} {}", f.file, f.what.trim()),
                    line => format!("- {}:{line} {}", f.file, f.what.trim()),
                })
                .collect();
            let (said, files) = match refused {
                Some(why) => (
                    format!(
                        "The forge has refused for hours to take the {} confirmed \
                         finding(s) {about} left unfixed: {}.",
                        findings.len(),
                        why.trim()
                    ),
                    "tries again",
                ),
                None => (
                    format!(
                        "{} merged with {} confirmed finding(s) left unfixed.",
                        capitalized(&about),
                        findings.len()
                    ),
                    "files each as an issue on the board",
                ),
            };
            return format!(
                "{said}\n\n{}\n\n{yes} {files}, and {} drops them.",
                list.join("\n"),
                trigger("no")
            );
        }
    };
    format!("{ask}, and {no} sends the worker your note.")
}

// Whether a merge ruling warns of what only a new pass of the review clears:
// a pass no reviewer read, a listed bot's threads left open, or a head no
// round read and no fix turn of kelpie's pushed.
fn needs_pass(kind: &RulingKind) -> bool {
    matches!(
        kind,
        RulingKind::Merge { unreviewed, open_threads, unread_head, .. }
            if unreviewed.is_some() || open_threads.is_some() || *unread_head
    )
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

fn rework_prompt(number: Option<u64>, note: &str) -> String {
    let about = number.map_or_else(
        || "your work item".to_owned(),
        |n| format!("pull request #{n}"),
    );
    format!(
        "The maintainer asked for a rework of {about}, with this note:\n\n{note}\n\n\
         Once you push it, the whole review reads your change again.\n"
    )
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
mod no_fix;
#[cfg(test)]
mod tests;
