//! Reworking an open pull request kelpie opened, from its latest review
//!
//! A pull request asks for one with the `ready-for-agent` label or a review
//! requesting changes, seen on the board's poll, and `rework <pr>` asks by
//! hand. The worker starts on the pull request's branch as `origin` holds
//! it, and its first turn is the latest review, verbatim, in a file in its
//! build folder. The work item then runs every gate again. Its issue stays
//! finished, so the board never brings it back. Kelpie labels each pull
//! request it hands back `ready-for-human`, so its labels say whose turn it is.

use std::fmt;
use std::path::{Path, PathBuf};

use super::Runner;
use super::report::{Begin, ReworkBy, StepReport};
use super::trigger;
use super::turn;
use crate::board::{LabelError, OpenPullRequest, READY, WorkerModel, worker_override};
use crate::pacer::Scope;
use crate::ports::{ForgeError, MaintainerReview, PullRequestState, Reviewed};
use crate::state::StateError;
use crate::work_item::{Known, Phase, Review, WorkItem, new_session_id};

/// The label on a pull request kelpie handed back to the maintainer
///
/// Kelpie never creates it: the maintainer makes it in the project's repo.
pub const HUMAN: &str = "ready-for-human";

/// The file in the build folder that carries the review
const REVIEW_FILE: &str = "maintainer-review.md";

/// Why `rework` was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReworkError {
    /// A work item is already in flight, for this issue
    InFlight(u64),
    /// The forge could not show the pull request
    PullRequest(u64, ForgeError),
    /// The pull request is merged or closed, as named
    NotOpen(u64, &'static str),
    /// The pull request is not from a `kelpie/<issue>` branch on the repo itself
    NotKelpies(u64),
    /// The latest review has no body and no unresolved comment
    NothingToRework(u64),
    /// The forge could not show the pull request's issue
    Issue(u64, ForgeError),
    /// The issue's `worker:` label cannot be used
    Label(LabelError),
    /// No random session id could be drawn, with the OS's reason
    Session(String),
    /// The review could not be written for the worker, with the reason
    ReviewFile(String),
    /// A label could not be taken off the pull request
    Unlabel(u64, &'static str, ForgeError),
    /// The work item could not be saved
    State(StateError),
}

impl fmt::Display for ReworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InFlight(issue) => write!(f, "the work item for #{issue} is in flight"),
            Self::PullRequest(number, e) => write!(f, "cannot read pull request #{number}: {e}"),
            Self::NotOpen(number, state) => write!(f, "pull request #{number} is {state}"),
            Self::NotKelpies(number) => {
                write!(f, "pull request #{number} is not one kelpie opened")
            }
            Self::NothingToRework(number) => write!(
                f,
                "nothing to rework: the latest review of #{number} has no body \
                 and no unresolved comment"
            ),
            Self::Issue(issue, e) => write!(f, "cannot read issue #{issue}: {e}"),
            Self::Label(e) => e.fmt(f),
            Self::Session(e) => write!(f, "cannot draw a session id: {e}"),
            Self::ReviewFile(e) => f.write_str(e),
            Self::Unlabel(number, label, e) => {
                write!(f, "cannot take the `{label}` label off #{number}: {e}")
            }
            Self::State(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ReworkError {}

impl ReworkError {
    // Whether asking again, with nothing changed on the pull request, is
    // refused the same way
    fn settled(&self) -> bool {
        matches!(
            self,
            Self::NotOpen(..) | Self::NotKelpies(_) | Self::NothingToRework(_) | Self::Label(_)
        )
    }
}

impl Runner {
    /// Makes open pull request `number`, which kelpie opened, the work item
    /// in flight, and returns the model and effort its worker runs on
    ///
    /// Its first turn runs once the project is running.
    ///
    /// # Errors
    ///
    /// [`ReworkError`] naming why the pull request cannot be reworked, or
    /// the change cannot be saved. Nothing changes then.
    pub fn rework(&mut self, number: u64) -> Result<WorkerModel, ReworkError> {
        if let Some(item) = &self.state.work_item {
            return Err(ReworkError::InFlight(item.issue));
        }
        let pr = self
            .ports
            .forge
            .reviewed(&self.settings.forge, number)
            .map_err(|e| ReworkError::PullRequest(number, e))?;
        self.start_rework(number, pr)
    }

    // Starts the rework one of `open` asks for, lowest number first. A
    // refusal that asking again would not change takes the label off,
    // records the review and goes to the pull request as a comment.
    pub(super) fn rework_asked(
        &mut self,
        open: &[OpenPullRequest],
    ) -> Result<Option<Begin>, StateError> {
        let mut ours: Vec<u64> = open
            .iter()
            .filter(|pr| {
                let issue = pr.head.strip_prefix("kelpie/");
                issue.is_some_and(|n| trigger::number(n).is_some())
            })
            .map(|pr| pr.number)
            .collect();
        ours.sort_unstable();
        for number in ours {
            let pr = match self.ports.forge.reviewed(&self.settings.forge, number) {
                Ok(pr) => pr,
                Err(e) => {
                    let reason = ReworkError::PullRequest(number, e).to_string();
                    return Ok(Some(Begin::Report(StepReport::BoardFailed { reason })));
                }
            };
            // A fork's branch can take kelpie's name, and is none of its business.
            if pr.from_fork {
                continue;
            }
            let labelled = pr.labels.iter().any(|l| l == READY);
            let asked =
                |r: &MaintainerReview| r.changes_requested && !self.state.reworked.contains(&r.id);
            let by = if labelled {
                ReworkBy::Label
            } else if pr.review.as_ref().is_some_and(asked) {
                ReworkBy::Review
            } else {
                continue;
            };
            if let Some(held) = self.pace(Scope::Dispatch)?.holds() {
                return Ok(Some(held));
            }
            let review = pr.review.as_ref().map(|r| r.id.clone());
            let begin = match self.start_rework(number, pr) {
                Ok(worker) => Begin::Report(StepReport::Reworked {
                    issue: self.state.work_item.as_ref().map_or(0, |item| item.issue),
                    pull_request: number,
                    worker,
                    by,
                }),
                Err(ReworkError::State(e)) => return Err(e),
                Err(e) if e.settled() => self.refuse_rework(number, labelled, review, &e)?,
                Err(e) => Begin::Report(StepReport::BoardFailed {
                    reason: e.to_string(),
                }),
            };
            return Ok(Some(begin));
        }
        Ok(None)
    }

    fn refuse_rework(
        &mut self,
        number: u64,
        labelled: bool,
        review: Option<String>,
        refused: &ReworkError,
    ) -> Result<Begin, StateError> {
        let repo = &self.settings.forge;
        if labelled && let Err(e) = self.ports.forge.set_label(repo, number, READY, false) {
            let reason = ReworkError::Unlabel(number, READY, e).to_string();
            return Ok(Begin::Report(StepReport::BoardFailed { reason }));
        }
        if let Some(review) = review.filter(|r| !self.state.reworked.contains(r)) {
            let mut next = self.state.clone();
            next.reworked.push(review);
            self.save(next)?;
        }
        let reason = refused.to_string();
        let comment = format!("Kelpie cannot rework this pull request: {reason}.");
        let comment_failed = self
            .ports
            .forge
            .comment(&self.settings.forge, number, &comment)
            .err()
            .map(|e| e.to_string());
        Ok(Begin::Report(StepReport::ReworkRefused {
            pull_request: number,
            reason,
            comment_failed,
        }))
    }

    // Puts `ready-for-human` on pull request `number` and takes
    // `ready-for-agent` off, as its labels on the forge stand now.
    pub(super) fn hand_back(&self, number: u64) -> Result<(), String> {
        let repo = &self.settings.forge;
        let failed = |e: ForgeError| format!("cannot hand #{number} back: {e}");
        let labels = self
            .ports
            .forge
            .pull_request(repo, number)
            .map_err(failed)?
            .labels;
        if !labels.iter().any(|l| l == HUMAN) {
            self.ports
                .forge
                .set_label(repo, number, HUMAN, true)
                .map_err(failed)?;
        }
        if labels.iter().any(|l| l == READY) {
            self.ports
                .forge
                .set_label(repo, number, READY, false)
                .map_err(failed)?;
        }
        Ok(())
    }

    // Checks `pr` can be reworked, then writes its review for the worker,
    // takes the triage labels off and saves the work item, in that order.
    fn start_rework(&mut self, number: u64, pr: Reviewed) -> Result<WorkerModel, ReworkError> {
        let repo = &self.settings.forge;
        match pr.state {
            PullRequestState::Open => {}
            PullRequestState::Merged => return Err(ReworkError::NotOpen(number, "merged")),
            PullRequestState::Closed => return Err(ReworkError::NotOpen(number, "closed")),
        }
        let issue = pr
            .branch
            .strip_prefix("kelpie/")
            .and_then(trigger::number)
            .filter(|_| !pr.from_fork)
            .ok_or(ReworkError::NotKelpies(number))?;
        let review = pr
            .review
            .filter(|r| !r.body.trim().is_empty() || !r.comments.is_empty())
            .ok_or(ReworkError::NothingToRework(number))?;
        let found = self
            .ports
            .forge
            .issue(repo, issue)
            .map_err(|e| ReworkError::Issue(issue, e))?;
        let worker = worker_override(&found.labels)
            .map_err(ReworkError::Label)?
            .unwrap_or_else(|| WorkerModel::from(&self.settings.models.worker));
        let session = new_session_id().map_err(|e| ReworkError::Session(e.to_string()))?;
        let fresh = self.fresh(issue, found.title, worker.clone(), session);
        let text = review_text(number, &review);
        turn::write(&fresh.build, &review_path(&fresh.build), &text)
            .map_err(ReworkError::ReviewFile)?;
        let mut labels = pr.labels;
        for label in [READY, HUMAN] {
            if labels.iter().any(|l| l == label) {
                self.ports
                    .forge
                    .set_label(repo, number, label, false)
                    .map_err(|e| ReworkError::Unlabel(number, label, e))?;
                labels.retain(|l| l != label);
            }
        }
        let mut next = self.state.clone();
        if !next.reworked.contains(&review.id) {
            next.reworked.push(review.id);
        }
        next.work_item = Some(WorkItem {
            branch: pr.branch,
            rework: true,
            pull_request: Some(number),
            // The fix is new code, so the qwen-review loop runs before CI.
            resume: Some(Phase::Review(Review::first())),
            known: Known {
                labels,
                ready: !pr.draft,
            },
            ..fresh
        });
        self.save(next).map_err(ReworkError::State)?;
        Ok(worker)
    }
}

// The worker can read its build folder, and a commit never carries it.
fn review_path(build: &Path) -> PathBuf {
    build.join(REVIEW_FILE)
}

// The reviewer's words go in as written: nothing here rewords them.
fn review_text(number: u64, review: &MaintainerReview) -> String {
    let mut text = format!("# The latest review of pull request #{number}\n");
    if !review.body.trim().is_empty() {
        text.push('\n');
        push_verbatim(&mut text, &review.body);
    }
    for comment in &review.comments {
        let at = match comment.line {
            Some(line) => format!("`{}` line {line}", comment.file),
            None => format!("`{}`", comment.file),
        };
        text.push_str(&format!("\n## On {at}\n\n"));
        push_verbatim(&mut text, &comment.body);
    }
    text
}

fn push_verbatim(text: &mut String, words: &str) {
    text.push_str(words);
    if !words.ends_with('\n') {
        text.push('\n');
    }
}

/// The first turn of a rework: the review, in the file it was written to
pub(super) fn first_prompt(item: &WorkItem) -> String {
    let number = item.pull_request.unwrap_or_default();
    format!(
        "Your work item reworks your pull request #{number} for issue #{}: {}\n\n\
         Its latest review asks for changes, and is in {}. \
         Make the changes it asks for, then commit and push with `git push origin HEAD`. \
         Your branch is the pull request's as `origin` holds it, with any commits the \
         maintainer pushed. The pull request is already open, so do not open another.\n",
        item.issue,
        item.title,
        review_path(&item.build).display()
    )
}

#[cfg(test)]
mod tests;
