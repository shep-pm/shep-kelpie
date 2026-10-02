//! Who reviews each round
//!
//! Rounds go down the project's reviewers in order, from the one after the
//! last round's, skipping any that cannot run: one limited to paths the pull
//! request does not change, or a local one once `review.local_rounds` are
//! spent or has reviewed nothing twice running. With none left, the
//! project's own Claude round runs. A round's reviewer is kept in its state
//! once it starts, so a round cut short resumes with the same one.

use std::path::Path;
use std::process::{Command, Stdio};

use super::super::Runner;
use super::super::review_bot::cap::matches;
use crate::settings::{LoopReviewer, ReviewerName};
use crate::work_item::Review;

/// The reviewer a round runs
#[derive(Debug, Clone)]
pub(super) struct Chosen {
    pub(super) reviewer: LoopReviewer,
    /// Whether it was the only one that could run
    pub(super) alone: bool,
}

impl Runner {
    /// Who reviews `review`'s round in `worktree`, diffed from `base`
    ///
    /// # Errors
    ///
    /// A message when git cannot list the changed files.
    pub(super) fn choose_reviewer(
        &self,
        review: &Review,
        worktree: &Path,
        base: &str,
    ) -> Result<Chosen, String> {
        if let Some(reviewer) = review.reviewer.as_ref().and_then(|n| self.listed(n)) {
            return Ok(Chosen {
                reviewer,
                alone: review.alone,
            });
        }
        let changed = match self.lineup.iter().any(|r| !r.paths().is_empty()) {
            true => changed_files(worktree, base)?,
            false => Vec::new(),
        };
        let local_left = self.local_left();
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
            touched && (!r.is_local() || (local_left > 0 && !down(r)))
        };
        let eligible = self.lineup.iter().filter(|r| runs(r)).count();
        let len = self.lineup.len();
        let start = match review.last.as_ref() {
            Some(last) => self
                .lineup
                .iter()
                .position(|r| &r.name == last)
                .map_or(0, |i| i + 1),
            // An older state file's rounds alternated from the first reviewer.
            None => (review.round.saturating_sub(1) as usize) % len.max(1),
        };
        let next = (0..len)
            .map(|i| &self.lineup[(start + i) % len])
            .find(|r| runs(r));
        let reviewer = match next {
            Some(reviewer) => reviewer.clone(),
            None => LoopReviewer::claude(&self.agents.reviewer, &self.agents.limits.reviewer),
        };
        Ok(Chosen {
            reviewer,
            alone: eligible <= 1,
        })
    }

    /// Whether `review`'s round counts toward `review.local_rounds`: a local
    /// reviewer's, where a limit is set
    ///
    /// With no limit nothing is counted, so an older binary reads the state file.
    pub(super) fn counts_local(&self, review: &Review) -> bool {
        self.settings.review.local_rounds.is_some() && self.is_local_round(review)
    }

    /// Whether `review`'s round is a local reviewer's
    pub(super) fn is_local_round(&self, review: &Review) -> bool {
        match &review.reviewer {
            Some(name) => self.listed(name).is_some_and(|r| r.is_local()),
            // An older state file ran the local round on odd rounds.
            None => {
                review.round % 2 == 1
                    && self.local_left() > 0
                    && self.lineup.first().is_some_and(LoopReviewer::is_local)
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

    // How many rounds local reviewers may run on a work item.
    fn local_rounds(&self) -> u32 {
        self.settings
            .review
            .local_rounds
            .map_or(u32::MAX, |rounds| rounds.get())
    }

    // How many of them this work item has not run yet.
    fn local_left(&self) -> u32 {
        let ran = self.current().map_or(0, |item| item.local_rounds);
        self.local_rounds().saturating_sub(ran)
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
