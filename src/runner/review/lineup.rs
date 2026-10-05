//! Who reviews each round
//!
//! Each round goes to the first listed reviewer the pass has not run,
//! skipping any that cannot run: one limited to paths the pull request does
//! not change, or a local one that has reviewed nothing twice running. The
//! pass ends once none is left. A pass in which none can run gets the
//! project's own Claude round. A round's reviewer is kept in its state once
//! it starts, so a round cut short resumes with the same one.

use std::path::Path;
use std::process::{Command, Stdio};

use super::super::Runner;
use super::super::review_bot::cap::matches;
use crate::ports::Timestamp;
use crate::settings::{LoopReviewer, ReviewerName};
use crate::work_item::{Phase, Review};

impl Runner {
    /// Who reviews `review`'s round in `worktree`, diffed from `base`, or
    /// none once the pass has run every reviewer that can run
    ///
    /// # Errors
    ///
    /// A message when git cannot list the changed files.
    pub(super) fn choose_reviewer(
        &self,
        review: &Review,
        worktree: &Path,
        base: &str,
    ) -> Result<Option<LoopReviewer>, String> {
        if let Some(reviewer) = review.reviewer.as_ref().and_then(|n| self.listed(n)) {
            return Ok(Some(reviewer));
        }
        let ran = self.ran_of(review);
        let changed = match self.lineup.iter().any(|r| !r.paths().is_empty()) {
            true => changed_files(worktree, base)?,
            false => Vec::new(),
        };
        let down = |r: &LoopReviewer| {
            let item = self.current();
            item.is_some_and(|item| item.local_reviewer_down(&r.name))
        };
        let runs = |r: &LoopReviewer| {
            let paths = r.paths();
            let touched = paths.is_empty()
                || changed
                    .iter()
                    .any(|file| paths.iter().any(|glob| matches(glob.as_str(), file)));
            touched && !(r.is_local() && down(r))
        };
        let due = |r: &&LoopReviewer| !ran.contains(&r.name) && runs(r);
        if let Some(next) = self.lineup.iter().find(due) {
            return Ok(Some(next.clone()));
        }
        Ok(ran
            .is_empty()
            .then(|| LoopReviewer::claude(&self.agents.reviewer, &self.agents.limits.reviewer)))
    }

    // The reviewers `review`'s pass has run. A state file saved before `ran`
    // names only the last, and its pass ran the list in order up to it.
    fn ran_of(&self, review: &Review) -> Vec<ReviewerName> {
        let Some(last) = review.last.as_ref().filter(|_| review.ran.is_empty()) else {
            return review.ran.clone();
        };
        match self.lineup.iter().position(|r| &r.name == last) {
            Some(at) => self.lineup[..=at].iter().map(|r| r.name.clone()).collect(),
            None => vec![last.clone()],
        }
    }

    /// Where the review goes once `review`'s reviewer is done: the next
    /// reviewer's round, or CI at `now` once none is left to run
    pub(in crate::runner) fn after_round(&self, review: Review, now: Timestamp) -> Phase {
        let ran = self.ran_of(&review);
        let next = Review { ran, ..review }.next_round();
        let Some(item) = self.current() else {
            return Phase::Review(next);
        };
        match self.choose_reviewer(&next, &item.worktree, &item.review_base()) {
            Ok(None) => Phase::Ci {
                head: None,
                since: now,
            },
            // A diff git could not take is the next round's step to report.
            Ok(Some(_)) | Err(_) => Phase::Review(next),
        }
    }

    /// Whether `review`'s round is a local reviewer's
    pub(super) fn is_local_round(&self, review: &Review) -> bool {
        match &review.reviewer {
            Some(name) => self.listed(name).is_some_and(|r| r.is_local()),
            // An older state file ran the local round on odd rounds.
            None => {
                review.round % 2 == 1 && self.lineup.first().is_some_and(LoopReviewer::is_local)
            }
        }
    }

    // The listed reviewer named `name`, or the Claude round every project has.
    fn listed(&self, name: &ReviewerName) -> Option<LoopReviewer> {
        let found = self.lineup.iter().find(|r| &r.name == name).cloned();
        found.or_else(|| {
            (name == &ReviewerName::claude())
                .then(|| LoopReviewer::claude(&self.agents.reviewer, &self.agents.limits.reviewer))
        })
    }
}

/// The files `worktree` changes from `base`, as git names them
fn changed_files(worktree: &Path, base: &str) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", "--name-only", "--no-renames", base])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git diff: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let names = String::from_utf8_lossy(&output.stdout);
    Ok(names.lines().map(str::to_owned).collect())
}
