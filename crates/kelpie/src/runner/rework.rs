//! Reworking an open pull request kelpie opened, from the maintainer's review
//!
//! `rework <pr>` makes a work item of a pull request whose work item kelpie
//! finished or dropped. Its worker starts on the pull request's branch as
//! `origin` holds it, and its first turn is the maintainer's latest review,
//! verbatim, in a file in its build folder. The work item then runs every
//! gate again. Its issue stays finished: only this trigger brings a rework.

use std::fmt;
use std::path::{Path, PathBuf};

use super::Runner;
use super::trigger;
use super::turn;
use crate::board::{LabelError, WorkerModel, worker_override};
use crate::ports::{ForgeError, MaintainerReview, PullRequestState};
use crate::state::StateError;
use crate::work_item::{Known, Phase, Review, WorkItem, new_session_id};

/// The file in the build folder that carries the maintainer's review
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
    /// The maintainer's latest review has no body and no unresolved comment
    NothingToRework(u64),
    /// The forge could not show the pull request's issue
    Issue(u64, ForgeError),
    /// The issue's `worker:` label cannot be used
    Label(LabelError),
    /// No random session id could be drawn, with the OS's reason
    Session(String),
    /// The review could not be written for the worker, with the reason
    ReviewFile(String),
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
            Self::State(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ReworkError {}

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
        let repo = &self.settings.forge;
        let pr = self
            .ports
            .forge
            .reviewed(repo, number)
            .map_err(|e| ReworkError::PullRequest(number, e))?;
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
        let mut next = self.state.clone();
        next.work_item = Some(WorkItem {
            branch: pr.branch,
            rework: true,
            pull_request: Some(number),
            // The fix is new code, so the qwen-review loop runs before CI.
            resume: Some(Phase::Review(Review::first())),
            known: Known {
                labels: pr.labels,
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

// The maintainer's words go in as written: nothing here rewords them.
fn review_text(number: u64, review: &MaintainerReview) -> String {
    let mut text = format!("# The maintainer's review of pull request #{number}\n");
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
         The maintainer reviewed it and asked for changes. Their review is in {}. \
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
