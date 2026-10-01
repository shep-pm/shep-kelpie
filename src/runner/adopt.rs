//! Adopting an open pull request kelpie didn't open
//!
//! The maintainer adopts one with `adopt <pr>`, or with `ready-for-agent` on
//! one whose branch isn't `kelpie/N`, seen on the board's poll. Adopted pull
//! requests wait in the state file for a free slot, one at a time, and go
//! before the board's issues. Each starts at CI on its branch as
//! `origin` holds it, with its labels, ready state and head taken as kelpie's
//! own. A review asking for changes is the worker's first turn instead. The
//! branch's owner may still be pushing, so only a push the worker's worktree
//! holds is the worker's.

use std::fmt;
use std::path::{Path, PathBuf};

use super::Runner;

use super::report::{Begin, StepReport};
use super::rework::{HUMAN, review_text};
use super::trigger;
use super::turn;
use crate::board::{LabelError, OpenPullRequest, READY, Skip, WorkerModel, worker_override};
use crate::pacer::Scope;
use crate::ports::{ForgeError, Issue, MaintainerReview, PullRequestState, Reviewed, Role};
use crate::state::{StateError, Waiting};
use crate::work_item::{Known, Phase, Review, Turn, WorkItem, new_session_id};
use crate::worktree::{self, Start};

/// The file in the build folder that tells the worker what it adopted
const ADOPTED_FILE: &str = "adopted-pull-request.md";

/// Why a pull request cannot be adopted
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptError {
    /// The forge could not show the pull request
    PullRequest(u64, ForgeError),
    /// The pull request is merged or closed, as named
    NotOpen(u64, &'static str),
    /// The pull request's branch is on a fork, not the repo itself
    Fork(u64),
    /// The pull request merges into this branch, not `main`
    Base(u64, String),
    /// The pull request was opened by this login, not the account kelpie acts as
    Author(u64, String),
    /// The pull request names no issue on the repo that it closes
    NoIssue(u64),
    /// A work item for the issue it closes is open
    InFlight(u64),
    /// The forge could not say which account kelpie acts as
    Viewer(ForgeError),
    /// The forge could not show the pull request's issue
    Issue(u64, ForgeError),
    /// The issue's `worker:` label cannot be used
    Label(LabelError),
    /// No random session id could be drawn, with the OS's reason
    Session(String),
    /// The worktree could not be prepared on the branch, with the reason
    Worktree(String),
    /// The forge could not show the review bot's reviews of the pull request
    ReviewBot(String, u64, ForgeError),
    /// The file for the worker could not be written, with the reason
    File(String),
    /// A label could not be taken off the pull request
    Unlabel(u64, String, ForgeError),
    /// The change could not be saved
    State(StateError),
}

impl fmt::Display for AdoptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PullRequest(number, e) => write!(f, "cannot read pull request #{number}: {e}"),
            Self::NotOpen(number, state) => write!(f, "pull request #{number} is {state}"),
            Self::Fork(number) => write!(f, "pull request #{number} comes from a fork"),
            Self::Base(number, base) => write!(
                f,
                "pull request #{number} merges into `{base}`, not `{}`",
                worktree::BASE
            ),
            Self::Author(number, login) => {
                write!(
                    f,
                    "pull request #{number} was opened by {login}, not by kelpie's account"
                )
            }
            Self::NoIssue(number) => {
                write!(f, "pull request #{number} names no issue it closes")
            }
            Self::InFlight(issue) => write!(f, "the work item for #{issue} is in flight"),
            Self::Viewer(e) => write!(f, "cannot read the account kelpie acts as: {e}"),
            Self::Issue(issue, e) => write!(f, "cannot read issue #{issue}: {e}"),
            Self::Label(e) => e.fmt(f),
            Self::Session(e) => write!(f, "cannot draw a session id: {e}"),
            Self::Worktree(e) => write!(f, "cannot prepare its worktree: {e}"),
            Self::ReviewBot(bot, number, e) => {
                write!(f, "cannot read {bot}'s reviews of #{number}: {e}")
            }
            Self::File(e) => f.write_str(e),
            Self::Unlabel(number, label, e) => {
                write!(f, "cannot take the `{label}` label off #{number}: {e}")
            }
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for AdoptError {}

impl AdoptError {
    // Whether asking again, with nothing changed on the pull request, is
    // refused the same way
    fn settled(&self) -> bool {
        matches!(
            self,
            Self::NotOpen(..)
                | Self::Fork(_)
                | Self::Base(..)
                | Self::Author(..)
                | Self::NoIssue(_)
                | Self::Label(_)
        )
    }
}

impl Runner {
    /// Adopts open pull request `number`, which waits for a free slot and
    /// any adopted before it
    ///
    /// Adopting one already waiting or in flight changes nothing.
    ///
    /// # Errors
    ///
    /// [`AdoptError`] naming why the pull request cannot be adopted, which
    /// also goes to it as a comment, or why the change cannot be saved.
    /// Nothing is adopted then.
    pub fn adopt(&mut self, number: u64) -> Result<(), AdoptError> {
        let in_flight = self
            .state
            .work_items
            .iter()
            .any(|i| i.pull_request == Some(number));
        if in_flight || self.waiting(number) {
            return Ok(());
        }
        let pr = self
            .ports
            .forge
            .reviewed(&self.settings.forge, number)
            .map_err(|e| AdoptError::PullRequest(number, e))?;
        if let Err(e) = self.adoptable(number, &pr) {
            if e.settled() {
                let _ = self.refusal_comment(number, &e);
            }
            return Err(e);
        }
        let mut next = self.state.clone();
        next.adopted.push(Waiting {
            pull_request: number,
            by_label: false,
        });
        self.save(next).map_err(AdoptError::State)
    }

    fn waiting(&self, number: u64) -> bool {
        self.state.adopted.iter().any(|w| w.pull_request == number)
    }

    // Queues each open pull request labelled `ready-for-agent` whose branch
    // isn't `kelpie/N`, and lets go of any the label adopted that lost it.
    // Then starts the oldest adopted one that can start. A refusal takes the
    // label off and goes to the pull request as a comment, and one merged or
    // closed while it waited just leaves. Returns each that could not start.
    pub(super) fn adopt_waiting(
        &mut self,
        open: &[OpenPullRequest],
    ) -> Result<(Option<Begin>, Vec<Skip>), StateError> {
        let mut skipped = Vec::new();
        let mut labelled: Vec<u64> = open
            .iter()
            .filter(|pr| pr.labels.iter().any(|l| l == READY))
            .filter(|pr| {
                let kelpies = pr.head.strip_prefix("kelpie/").and_then(trigger::number);
                kelpies.is_none()
            })
            .map(|pr| pr.number)
            .collect();
        labelled.sort_unstable();
        let mut next = self.state.clone();
        next.adopted
            .retain(|w| !w.by_label || labelled.contains(&w.pull_request));
        for number in labelled {
            if !next.adopted.iter().any(|w| w.pull_request == number) {
                next.adopted.push(Waiting {
                    pull_request: number,
                    by_label: true,
                });
            }
        }
        if next.adopted != self.state.adopted {
            self.save(next)?;
        }
        let waiting: Vec<u64> = self.state.adopted.iter().map(|w| w.pull_request).collect();
        for number in waiting {
            if let Some(held) = self.pace_worker(Scope::Dispatch)?.holds() {
                return Ok((Some(held), skipped));
            }
            let begin = match self.start_adoption(number) {
                Ok((issue, worker)) => Begin::Report(StepReport::Adopted {
                    issue,
                    pull_request: number,
                    worker,
                }),
                Err(AdoptError::State(e)) => return Err(e),
                Err(AdoptError::NotOpen(..)) => {
                    self.let_go(number)?;
                    continue;
                }
                // It waits for the work item on its issue to end.
                Err(AdoptError::InFlight(_)) => continue,
                Err(e) if e.settled() => match self.refuse_adoption(number, &e)? {
                    Ok(begin) => begin,
                    Err(e) => {
                        skipped.push(Skip::Adopt {
                            pull_request: number,
                            error: e.to_string(),
                        });
                        continue;
                    }
                },
                Err(e) => {
                    skipped.push(Skip::Adopt {
                        pull_request: number,
                        error: e.to_string(),
                    });
                    continue;
                }
            };
            return Ok((Some(begin), skipped));
        }
        Ok((None, skipped))
    }

    // The outer error is a save that failed; the inner, a label that would
    // not come off, which keeps it waiting for the next poll to refuse.
    fn refuse_adoption(
        &mut self,
        number: u64,
        refused: &AdoptError,
    ) -> Result<Result<Begin, AdoptError>, StateError> {
        let repo = &self.settings.forge;
        let unlabelled = self.ports.forge.pull_request(repo, number).and_then(|pr| {
            if pr.labels.iter().any(|l| l == READY) {
                self.ports.forge.set_label(repo, number, READY, false)
            } else {
                Ok(())
            }
        });
        if let Err(e) = unlabelled {
            return Ok(Err(AdoptError::Unlabel(number, READY.to_owned(), e)));
        }
        self.let_go(number)?;
        let comment_failed = self.refusal_comment(number, refused);
        Ok(Ok(Begin::Report(StepReport::AdoptRefused {
            pull_request: number,
            reason: refused.to_string(),
            comment_failed,
        })))
    }

    fn let_go(&mut self, number: u64) -> Result<(), StateError> {
        let mut next = self.state.clone();
        next.adopted.retain(|w| w.pull_request != number);
        self.save(next)
    }

    fn refusal_comment(&self, number: u64, refused: &AdoptError) -> Option<String> {
        let comment = format!("Kelpie cannot adopt this pull request: {refused}.");
        let posted = self
            .ports
            .forge
            .comment(&self.settings.forge, number, &comment);
        posted.err().map(|e| e.to_string())
    }

    // The issue an open pull request of kelpie's account, from the repo
    // itself, closes: the lowest, when it closes more than one
    fn adoptable(&mut self, number: u64, pr: &Reviewed) -> Result<u64, AdoptError> {
        match pr.state {
            PullRequestState::Open => {}
            PullRequestState::Merged => return Err(AdoptError::NotOpen(number, "merged")),
            PullRequestState::Closed => return Err(AdoptError::NotOpen(number, "closed")),
        }
        if pr.from_fork {
            return Err(AdoptError::Fork(number));
        }
        if pr.base != worktree::BASE {
            return Err(AdoptError::Base(number, pr.base.clone()));
        }
        let me = self.viewer().map_err(AdoptError::Viewer)?;
        if pr.author != me {
            let login = if pr.author.is_empty() {
                "a deleted account".to_owned()
            } else {
                pr.author.clone()
            };
            return Err(AdoptError::Author(number, login));
        }
        pr.closes
            .iter()
            .min()
            .copied()
            .ok_or(AdoptError::NoIssue(number))
    }

    // Checks the pull request can be adopted, then prepares its worktree,
    // writes what the worker needs, takes the triage and summon labels off
    // and saves the work item, in that order.
    fn start_adoption(&mut self, number: u64) -> Result<(u64, WorkerModel), AdoptError> {
        let repo = self.settings.forge.clone();
        let pr = self
            .ports
            .forge
            .reviewed(&repo, number)
            .map_err(|e| AdoptError::PullRequest(number, e))?;
        let issue = self.adoptable(number, &pr)?;
        if self.state.item(issue).is_some() {
            return Err(AdoptError::InFlight(issue));
        }
        let found = self
            .ports
            .forge
            .issue(&repo, issue)
            .map_err(|e| AdoptError::Issue(issue, e))?;
        let worker = worker_override(&found.labels)
            .map_err(AdoptError::Label)?
            .unwrap_or_else(|| WorkerModel::from(&self.agents.worker));
        let session = new_session_id().map_err(|e| AdoptError::Session(e.to_string()))?;
        let fresh = self.fresh(issue, found.title.clone(), worker.clone(), session);
        // A start that failed part way leaves a worktree on a head `origin`
        // may have moved past, so each start begins from a fresh one.
        let removed = worktree::remove(
            &self.settings.repo,
            &fresh.worktree,
            &pr.branch,
            &fresh.build,
            false,
        );
        removed.map_err(|e| AdoptError::Worktree(e.to_string()))?;
        let prepared = worktree::prepare(
            &self.settings.repo,
            &fresh.worktree,
            &pr.branch,
            Start::Pushed,
            &fresh.build,
        );
        prepared.map_err(|e| AdoptError::Worktree(e.to_string()))?;
        let head = worktree::origin_head(&self.settings.repo, &pr.branch)
            .map_err(|e| AdoptError::Worktree(e.to_string()))?;
        // The cap counts every listed bot's reviews so far. One of this head
        // counts when the gate finds it, as any round does.
        let bots: Vec<_> = self
            .settings
            .reviewers()
            .into_iter()
            .map(|b| self.profile(b))
            .collect();
        let mut rounds = 0u32;
        let counted = if self.settings.coderabbit.enabled {
            &bots[..]
        } else {
            &[]
        };
        for bot in counted {
            let activity = self
                .ports
                .forge
                .review_bot(&repo, number, bot.login())
                .map_err(|e| AdoptError::ReviewBot(bot.name().to_owned(), number, e))?;
            rounds = rounds.saturating_add(bot.reviewed_besides(&activity, &head));
        }
        let review = pr
            .review
            .as_ref()
            .filter(|r| r.changes_requested && !self.state.reworked.contains(&r.id))
            .cloned();
        let text = adopted_text(number, &pr, issue, &found, review.as_ref());
        turn::write(&fresh.build, &adopted_path(&fresh.build), &text).map_err(AdoptError::File)?;
        // With the summon labels off, no push summons a review bot outside the lease.
        let mut labels = pr.labels;
        let summons = bots.iter().filter_map(|bot| bot.label());
        for label in [READY, HUMAN].into_iter().chain(summons) {
            if labels.iter().any(|l| l == label) {
                self.ports
                    .forge
                    .set_label(&repo, number, label, false)
                    .map_err(|e| AdoptError::Unlabel(number, label.to_owned(), e))?;
                labels.retain(|l| l != label);
            }
        }
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        next.adopted.retain(|w| w.pull_request != number);
        let (turn, phase, resume) = match &review {
            Some(review) => {
                next.reworked.push(review.id.clone());
                let first = Some(Phase::Review(Review::first()));
                (Turn::Due, Phase::Implement, first)
            }
            None => {
                let ci = Phase::Ci {
                    head: None,
                    since: now,
                };
                (Turn::Ended { at: now }, ci, None)
            }
        };
        let mut item = WorkItem {
            branch: pr.branch,
            adopted: true,
            arrived: Some(head.clone()),
            pull_request: Some(number),
            turn,
            phase,
            resume,
            known: Known {
                labels,
                ready: !pr.draft,
                head: Some(head),
            },
            ..fresh
        };
        item.coderabbit.rounds = rounds;
        item.summon_owed = self.settings.coderabbit.enabled;
        next.work_items.push(item);
        self.save(next).map_err(AdoptError::State)?;
        Ok((issue, worker))
    }
}

impl Runner {
    /// The branch's head on `origin`, as the worker's own push
    ///
    /// On an adopted branch its owner may push too, so the head counts only
    /// when the worktree holds it. `None` keeps the head kelpie knew, which
    /// errs toward parking.
    pub(super) fn own_push(&self) -> Option<String> {
        let item = self.current()?;
        let head = self.origin_head().ok()?;
        if !item.adopted {
            return Some(head);
        }
        let checked_out = worktree::head(&self.settings.repo, &item.worktree).ok()?;
        (checked_out == head).then_some(head)
    }

    // A push by anyone else to an adopted branch since kelpie last looked
    // goes to the gate, which parks it, before a worker's turn builds on it.
    pub(super) fn pushed_by_someone_else(&mut self) -> Result<Option<Begin>, StateError> {
        let Some(item) = self.current() else {
            return Ok(None);
        };
        let due = matches!(item.turn, Turn::Due | Turn::Next { .. });
        let Some(known) = item.known.head.clone().filter(|_| item.adopted && due) else {
            return Ok(None);
        };
        if self.origin_head().is_ok_and(|head| head == known) {
            return Ok(None);
        }
        let now = self.ports.clock.now();
        self.update(|item| {
            item.turn = Turn::Ended { at: now };
            item.resume = None;
            item.phase = Phase::Ci {
                head: None,
                since: now,
            };
        })?;
        self.check_ci().map(Some)
    }
}

/// Whether the adopted work item's worker has yet to begin its session
pub(super) fn unborn(item: &WorkItem) -> bool {
    item.adopted
        && !item
            .calls
            .iter()
            .any(|c| c.role == Role::Worker && c.session == item.session)
}

// The worker can read its build folder, and a commit never carries it.
fn adopted_path(build: &Path) -> PathBuf {
    build.join(ADOPTED_FILE)
}

// The pull request's, the issue's and the reviewer's words go in as written.
fn adopted_text(
    number: u64,
    pr: &Reviewed,
    issue: u64,
    found: &Issue,
    review: Option<&MaintainerReview>,
) -> String {
    let mut text = format!("# Pull request #{number}: {}\n", pr.title);
    push_block(&mut text, &pr.body);
    text.push_str(&format!("\n# Issue #{issue}: {}\n", found.title));
    push_block(&mut text, &found.body);
    if let Some(review) = review {
        text.push('\n');
        text.push_str(&review_text(number, review));
    }
    text
}

fn push_block(text: &mut String, words: &str) {
    if words.trim().is_empty() {
        return;
    }
    text.push('\n');
    text.push_str(words);
    if !words.ends_with('\n') {
        text.push('\n');
    }
}

/// The first turn of an adopted work item, ahead of `then`, what the gate
/// sends it, or the review to fix when there is nothing else
pub(super) fn first_prompt(item: &WorkItem, then: Option<&str>) -> String {
    let number = item.pull_request.unwrap_or_default();
    let mut prompt = format!(
        "Your work item adopts pull request #{number} for issue #{}: {}\n\n\
         You did not write its code. {} holds the pull request's title and body, \
         its issue, and the review to fix, if it has one. Your branch is the pull \
         request's as `origin` holds it. The pull request is already open, so do \
         not open another.\n\n",
        item.issue,
        item.title,
        adopted_path(&item.build).display()
    );
    prompt.push_str(then.unwrap_or(
        "Make the changes the review asks for, then commit and push with \
         `git push origin HEAD`.",
    ));
    prompt.push('\n');
    prompt
}

#[cfg(test)]
mod tests;
