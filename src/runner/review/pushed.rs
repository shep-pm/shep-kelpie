//! The pushed head, which every review round reads
//!
//! A local round or a session reads the worktree, and a bot the pull
//! request, but CI and the merge take the head on `origin`. So before a
//! round runs, the worktree must hold nothing uncommitted and have that head
//! checked out. If not, the worker gets one turn to push or discard, and a
//! worktree still off the pushed head after it parks the work item on a
//! `stuck` ruling naming the files and both heads. A worker already sent
//! back once to commit its turn's work has had that one turn.

use super::super::Runner;
use super::super::gate::short;
use super::super::report::{Begin, StepReport};
use super::super::turn::named_files;
use crate::ports::PullRequestState;
use crate::state::{StateError, Stuck};
use crate::work_item::{Phase, Review, ReviewStage, Turn};
use crate::worktree;

/// How a worktree stands off the head on `origin`
struct Off {
    /// The files it holds uncommitted
    files: Vec<String>,
    /// The commit it has checked out
    head: String,
    /// The branch's head on `origin`
    pushed: String,
}

impl Runner {
    /// What the step does instead of `review`'s round, when the worktree is
    /// not the pushed head: the worker's turn to push or discard, sent here
    ///
    /// `None` lets the round run, and keeps in `review` the head it reads.
    pub(super) fn before_round(
        &mut self,
        review: &mut Review,
    ) -> Result<Option<Begin>, StateError> {
        let off = match self.off_pushed() {
            Ok(Ok(head)) => {
                review.reading = Some(head);
                let reading = review.reading.clone();
                self.update(|item| {
                    if let Phase::Review(review) = &mut item.phase {
                        review.reading = reading;
                    }
                })?;
                return Ok(None);
            }
            Ok(Err(off)) => off,
            Err(reason) => return self.unreadable(reason).map(Some),
        };
        let item = self.current().expect("a round is a work item's");
        let issue = item.issue;
        let pull_request = item.pull_request.expect("review runs on a pull request");
        let prompt = push_prompt(&off.files, &off.head, &off.pushed);
        let pushing = Review {
            stage: ReviewStage::Pushing,
            ..review.clone()
        };
        self.update(|item| {
            item.turn = Turn::Next { prompt };
            item.phase = Phase::Review(pushing);
        })?;
        Ok(Some(Begin::Report(StepReport::Unpushed {
            issue,
            pull_request,
            round: review.round,
            files: off.files,
            head: off.head,
            pushed: off.pushed,
        })))
    }

    /// The worker's turn to push or discard has ended: `review`'s round runs
    /// on a worktree at the pushed head, or the work item parks
    pub(super) fn pushing_ended(&mut self, review: Review) -> Result<Begin, StateError> {
        let round = Review {
            stage: ReviewStage::Round,
            ..review
        };
        let off = match self.off_pushed() {
            Ok(off) => off.err(),
            Err(reason) => return self.unreadable(reason),
        };
        let Some(Off {
            files,
            head,
            pushed,
        }) = off
        else {
            self.update(|item| item.phase = Phase::Review(round))?;
            return self.review_step();
        };
        let number = self.current().and_then(|item| item.pull_request);
        let number = number.expect("review runs on a pull request");
        let kind = Stuck::Unpushed {
            files,
            head,
            pushed,
            review: round,
        };
        self.raise(number, kind.into())
    }

    // Git could not say where the worktree stands. A branch merged or closed
    // by hand may be gone from `origin`, so the pull request ends the way the
    // gate ends it; anything else fails the step, which is tried again.
    fn unreadable(&mut self, reason: String) -> Result<Begin, StateError> {
        let number = self.current().and_then(|item| item.pull_request);
        let number = number.expect("review runs on a pull request");
        match self.ports.forge.pull_request(&self.settings.forge, number) {
            Ok(pr) if pr.state == PullRequestState::Merged => {
                self.update(|item| item.phase = Phase::Done { merged: true })?;
                self.finish(true)
            }
            Ok(pr) if pr.state == PullRequestState::Closed => {
                self.raise(number, Stuck::Closed.into())
            }
            _ => Ok(self.gate_failed(reason)),
        }
    }

    /// Whether the worktree still holds `head`, checked out and clean, as a
    /// round that read it ends. Git that cannot say counts as no.
    pub(super) fn still_at(&self, head: &str) -> bool {
        let Some(item) = self.current() else {
            return false;
        };
        let repo = &self.settings.repo;
        let files = worktree::uncommitted(repo, &item.worktree);
        let at = worktree::head(repo, &item.worktree);
        match (files, at) {
            (Ok(files), Ok(at)) => files.is_empty() && at == head,
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("cannot read issue #{}'s worktree: {e}", item.issue);
                false
            }
        }
    }

    // Asks git for the worktree's files and head, and `origin` for its head:
    // the pushed head when the worktree holds it clean, or how it stands off it.
    fn off_pushed(&self) -> Result<Result<String, Off>, String> {
        let item = self.current().expect("a worktree is a work item's");
        let repo = &self.settings.repo;
        let files = worktree::uncommitted(repo, &item.worktree)
            .map_err(|e| format!("cannot list issue #{}'s uncommitted files: {e}", item.issue))?;
        let head = worktree::head(repo, &item.worktree)
            .map_err(|e| format!("cannot read issue #{}'s worktree head: {e}", item.issue))?;
        let pushed = self.origin_head()?;
        if files.is_empty() && head == pushed {
            return Ok(Ok(pushed));
        }
        Ok(Err(Off {
            files,
            head,
            pushed,
        }))
    }
}

/// How a worktree off the pushed head stands, in a prompt's or a ruling's words
pub(in crate::runner) fn off_text(files: &[String], head: &str, pushed: &str) -> String {
    let mut said = Vec::new();
    if !files.is_empty() {
        said.push(format!(
            "it holds uncommitted changes: {}",
            named_files(files)
        ));
    }
    if head != pushed {
        said.push(format!(
            "it has {} checked out, and the head on origin is {}",
            short(head),
            short(pushed)
        ));
    }
    said.join("; ")
}

/// The prompt for the worker's turn to push or discard, before a review round
///
/// Run in the foreground, since nothing wakes a turn that ended.
pub(in crate::runner) fn push_prompt(files: &[String], head: &str, pushed: &str) -> String {
    format!(
        "Before the review reads your pull request, your worktree must hold exactly \
         what is pushed, since the review, CI and the merge all take the head on \
         origin. It does not: {}. Commit what you mean to keep and push it with \
         `git push origin HEAD`, and discard what you do not want, so the worktree is \
         clean at the pushed head. If the push is rejected because origin moved, stop \
         and say so in your reply; never force-push. Run commands in the foreground and wait for them. \
         If only the maintainer can unblock you, end your reply with a \
         <kelpie-question> block.",
        off_text(files, head, pushed)
    )
}

#[cfg(test)]
mod tests;
