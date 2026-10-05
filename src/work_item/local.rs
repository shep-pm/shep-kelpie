//! What a work item keeps of its local reviewers' rounds that left files
//! unreviewed

use super::WorkItem;
use crate::settings::ReviewerName;

/// How many failed rounds in a row take a local reviewer out of a work item
pub const LOCAL_FAILURES_DOWN: u32 = 2;

impl WorkItem {
    /// Whether local reviewer `name` reviewed nothing twice in a row, which
    /// leaves the review to the other reviewers
    pub fn local_reviewer_down(&self, name: &ReviewerName) -> bool {
        self.local_failures
            .get(name)
            .is_some_and(|n| *n >= LOCAL_FAILURES_DOWN)
    }

    /// Records a local round by `reviewer` that reviewed something, leaving
    /// `unreviewed` unreviewed
    ///
    /// A file the same reviewer left unreviewed last time and leaves again
    /// counts against it; a round that left none, or only new ones, or
    /// whose files another reviewer left, clears its count.
    pub fn note_local_round(&mut self, reviewer: &ReviewerName, unreviewed: &[String]) {
        let mine = self.local_unreviewed_by.as_ref() == Some(reviewer);
        let again = mine
            && unreviewed
                .iter()
                .any(|file| self.local_unreviewed.contains(file));
        if again {
            *self.local_failures.entry(reviewer.clone()).or_default() += 1;
        } else {
            self.local_failures.remove(reviewer);
        }
        self.local_unreviewed = unreviewed.to_vec();
        self.local_unreviewed_by = Some(reviewer.clone());
    }

    /// The local reviewers that reviewed nothing twice in a row
    pub fn local_reviewers_down(&self) -> Vec<&ReviewerName> {
        let down = self
            .local_failures
            .iter()
            .filter(|(name, _)| self.local_reviewer_down(name));
        down.map(|(name, _)| name).collect()
    }
}
