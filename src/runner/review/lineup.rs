//! Who reviews each round
//!
//! Each round goes to the first listed reviewer the pass has not run,
//! skipping any that cannot run: one limited to paths the pull request does
//! not change, a local one that has reviewed nothing twice running, or a
//! review bot that has made its `rounds` of reads. A pass of the bots alone
//! skips every other reviewer. The pass ends once none is left. A round's
//! reviewer is kept in its state once it starts, so a round cut short
//! resumes with the same one.

use std::path::Path;
use std::process::Stdio;

use super::super::Runner;
use crate::ports::Timestamp;
use crate::settings::{AgentName, ListedReviewer};
use crate::work_item::{Phase, Review, WorkItem};

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
    ) -> Result<Option<ListedReviewer>, String> {
        if let Some(reviewer) = review.reviewer.as_ref().and_then(|n| self.listed(n)) {
            return Ok(Some(reviewer));
        }
        let ran = self.ran_of(review);
        let changed = match self.lineup.iter().any(|r| !r.paths.is_empty()) {
            true => changed_files(&self.settings.repo, worktree, base)?,
            false => Vec::new(),
        };
        let down = |r: &ListedReviewer| {
            let item = self.current();
            item.is_some_and(|item| item.local_reviewer_down(&r.name))
        };
        // A bot that owes an adopted pull request a summon reads whatever its reads.
        let read_out = |r: &ListedReviewer| {
            let (Some(bot), Some(item)) = (r.bot(), self.current()) else {
                return false;
            };
            let reads = item.bot_reads.get(&bot.bot).copied().unwrap_or(0);
            let owed = item.summons_owed.contains(&bot.bot);
            !owed && bot.rounds.is_some_and(|rounds| reads >= rounds.get())
        };
        let runs = |r: &ListedReviewer| {
            let paths = &r.paths;
            let touched = paths.is_empty()
                || changed
                    .iter()
                    .any(|file| paths.iter().any(|glob| matches(glob.as_str(), file)));
            let kind = !review.bots_only || r.bot().is_some();
            kind && touched && !(r.is_local() && down(r)) && !read_out(r)
        };
        let due = |r: &&ListedReviewer| !ran.contains(&r.name) && runs(r);
        Ok(self.lineup.iter().find(due).cloned())
    }

    // The reviewers `review`'s pass has run. A state file saved before `ran`
    // names only the last, and its pass ran the list in order up to it.
    fn ran_of(&self, review: &Review) -> Vec<AgentName> {
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
    ///
    /// A pass with a bot it went on without ends in a round step of its own,
    /// which first reads whether that bot reviewed the head since.
    pub(in crate::runner) fn after_round(&self, review: Review, now: Timestamp) -> Phase {
        let ran = self.ran_of(&review);
        let next = Review { ran, ..review }.next_round();
        let Some(item) = self.current() else {
            return Phase::Review(next);
        };
        match self.choose_reviewer(&next, &item.worktree, &item.review_base()) {
            Ok(None) if !next.unread && item.bots_skipped.is_empty() => Phase::Ci {
                head: None,
                since: now,
            },
            // A diff git could not take is the next round's step to report,
            // and a pass no reviewer read ends in a step of its own, which says so.
            Ok(_) | Err(_) => Phase::Review(next),
        }
    }

    /// Why no reviewer of `review`'s pass read `item`'s pull request, naming
    /// each listed one that was down or ran and read nothing, or none when
    /// every listed one was passed over by its own `paths`
    pub(super) fn unread(&self, item: &WorkItem, review: &Review) -> Option<String> {
        let ran = self.ran_of(review);
        let missed: Vec<String> = (self.lineup.iter())
            .filter_map(|r| {
                let skipped = item.bots_skipped.iter().find(|s| s.reviewer() == &r.name);
                let why = if let Some(skipped) = skipped.filter(|_| ran.contains(&r.name)) {
                    return Some(format!("{} was passed over: {}", r.name, skipped.why()));
                } else if ran.contains(&r.name) && item.reviewers_skipped.contains(&r.name) {
                    "was passed over after its calls kept failing"
                } else if ran.contains(&r.name) {
                    "reviewed no file"
                } else if r.is_local() && item.local_reviewer_down(&r.name) {
                    "is down for this work item"
                } else {
                    return None;
                };
                Some(format!("{} {why}", r.name))
            })
            .collect();
        (!missed.is_empty()).then(|| missed.join(", "))
    }

    /// Whether `review`'s round is a local reviewer's
    pub(super) fn is_local_round(&self, review: &Review) -> bool {
        match &review.reviewer {
            Some(name) => self.listed(name).is_some_and(|r| r.is_local()),
            // An older state file ran the local round on odd rounds.
            None => {
                review.round % 2 == 1 && self.lineup.first().is_some_and(ListedReviewer::is_local)
            }
        }
    }

    // The listed reviewer named `name`.
    pub(in crate::runner) fn listed(&self, name: &AgentName) -> Option<ListedReviewer> {
        self.lineup.iter().find(|r| &r.name == name).cloned()
    }
}

/// The files `worktree` changes from `base`, as git names them
fn changed_files(repo: &Path, worktree: &Path, base: &str) -> Result<Vec<String>, String> {
    let output = crate::worktree::trusted_command(repo, worktree)
        .map_err(|e| e.to_string())?
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

/// Whether `path` matches `glob`: `*` and `?` stay inside one folder,
/// `**` spans folders, and `**/` may match none
pub(in crate::runner) fn matches(glob: &str, path: &str) -> bool {
    matches_bytes(glob.as_bytes(), path.as_bytes())
}

fn matches_bytes(glob: &[u8], path: &[u8]) -> bool {
    match glob {
        [] => path.is_empty(),
        [b'*', b'*', b'/', rest @ ..] => {
            matches_bytes(rest, path)
                || path
                    .iter()
                    .enumerate()
                    .any(|(i, b)| *b == b'/' && matches_bytes(rest, &path[i + 1..]))
        }
        [b'*', b'*', rest @ ..] => (0..=path.len()).any(|i| matches_bytes(rest, &path[i..])),
        [b'*', rest @ ..] => {
            let folder = path.iter().position(|b| *b == b'/').unwrap_or(path.len());
            (0..=folder).any(|i| matches_bytes(rest, &path[i..]))
        }
        [b'?', rest @ ..] => path
            .first()
            .is_some_and(|b| *b != b'/' && matches_bytes(rest, &path[1..])),
        [c, rest @ ..] => path.first() == Some(c) && matches_bytes(rest, &path[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glob_keeps_single_stars_inside_one_folder() {
        assert!(matches("*.lock", "Cargo.lock"));
        assert!(!matches("*.lock", "sub/Cargo.lock"));
        assert!(matches("**/*.lock", "sub/deeper/Cargo.lock"));
        assert!(matches("**/*.lock", "Cargo.lock"));
        assert!(matches("docs/**", "docs/a/b.md"));
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "a/c"));
        assert!(!matches("Cargo.lock", "Cargo.lockx"));
    }
}
