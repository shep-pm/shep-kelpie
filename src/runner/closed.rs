//! Work items whose issue was closed with no change
//!
//! A work item with no pull request ends once its issue is closed while its
//! branch and worktree hold no work: a turn that ends so finishes it with no
//! nudge and no ruling, and a pass ends a parked one and withdraws its
//! ruling. Kelpie cannot tell who closed the issue. Work found, or a git
//! that cannot say, leaves the item to go on as before.

use super::Runner;
use crate::ports::Alert;
use crate::state::{ProjectState, StateError};
use crate::work_item::Phase;
use crate::worktree;

/// Seconds between reads of a parked work item's issue. A parked item waits
/// hours, and each read is a forge call and a fetch.
pub(super) const PARKED_READ: u64 = 5 * 60;

impl Runner {
    /// Whether `issue`'s work item has its issue closed and no work on its
    /// branch or in its worktree. A git that cannot say is noted, and counts
    /// as work.
    ///
    /// # Errors
    ///
    /// Why the forge could not be asked.
    pub(super) fn closed_with_no_change(&mut self, issue: u64) -> Result<bool, String> {
        let item = self.state.item(issue).expect("an open work item");
        let (branch, tree) = (item.branch.clone(), item.worktree.clone());
        let read = self.ports.forge.issue(&self.remote, issue);
        if read
            .map_err(|e| format!("cannot read issue #{issue}: {e}"))?
            .open
        {
            return Ok(false);
        }
        match worktree::holds_work(&self.settings.git.checkout, &tree, &branch) {
            Ok(held) => Ok(!held),
            Err(e) => {
                let note = format!("issue #{issue} is closed, but its work cannot be read: {e}");
                self.notes.push(note);
                Ok(false)
            }
        }
    }

    /// Ends each parked work item with no pull request whose issue was
    /// closed with no change, withdrawing its ruling
    ///
    /// Each such item's issue is read at most once every [`PARKED_READ`]
    /// seconds, counted in memory, so a restart reads it at once. An open
    /// pull request that closes the issue keeps the item. A forge that
    /// cannot be asked is noted and asked again at the next read.
    ///
    /// # Errors
    ///
    /// [`StateError::Write`] when the change cannot be saved.
    pub(super) fn read_parked(&mut self) -> Result<(), StateError> {
        let now = self.ports.clock.now();
        let read = &self.parked_reads;
        let due = |issue: u64| {
            read.get(&issue)
                .is_none_or(|at| now.0.saturating_sub(at.0) >= PARKED_READ)
        };
        let parked = self.issues_where(|i| {
            i.parked() && i.pull_request.is_none() && i.attached.is_none() && due(i.issue)
        });
        let mut ended = Vec::new();
        for issue in parked {
            self.parked_reads.insert(issue, now);
            match self.closed_with_no_change(issue) {
                Ok(true) => ended.push(issue),
                Ok(false) => {}
                Err(why) => self.notes.push(why),
            }
        }
        if ended.is_empty() {
            return Ok(());
        }
        // A pull request on another branch that closes the issue is work too.
        match self.ports.forge.open_pull_requests(&self.remote) {
            Ok(open) => ended.retain(|n| !open.iter().any(|pr| pr.closes.contains(n))),
            Err(e) => {
                let note = format!("cannot list open pull requests: {e}");
                self.notes.push(note);
                return Ok(());
            }
        }
        let mut next = self.state.clone();
        let withdrawn: Vec<(u64, Option<u64>)> = (ended.iter())
            .map(|&issue| (issue, end_closed(&mut next, issue)))
            .collect();
        if let Err(e) = self.save(next) {
            // Read again on the next pass, before any answer resumes a worker.
            for issue in &ended {
                self.parked_reads.remove(issue);
            }
            return Err(e);
        }
        for (issue, id) in withdrawn {
            self.say_closed(issue, id);
        }
        Ok(())
    }

    /// Logs that `issue` was closed with no change, so its work item ends,
    /// and that ruling `id` is withdrawn, if it was parked on one. Posts it
    /// once to the webhook, where one is set: nothing is left to retry it for.
    pub(super) fn say_closed(&mut self, issue: u64, id: Option<u64>) {
        let project = self.project.as_str().to_owned();
        let withdrawn = id.map_or_else(String::new, |id| format!(" Ruling {id} is withdrawn."));
        let text = format!(
            "Issue #{issue} was closed with no change, so its work item ends with no \
             pull request.{withdrawn} Nothing to answer."
        );
        self.notes.push(text.clone());
        self.notice_on(issue, &text);
        let Some(webhook) = self.webhook.clone() else {
            return;
        };
        let alert = Alert {
            title: format!("kelpie: {project} #{issue} closed with no change"),
            text,
            reply: None,
        };
        if let Err(e) = self.ports.alerts.post(&webhook, &alert) {
            let failed = format!("cannot post that #{issue} was closed with no change: {e}");
            self.notes.push(failed);
        }
    }
}

/// Marks `issue`'s work item in `next` to end, closed with no change, and
/// drops its rulings. Returns the ruling it was parked on, if any.
pub(super) fn end_closed(next: &mut ProjectState, issue: u64) -> Option<u64> {
    let item = next.item_mut(issue).expect("a work item that ends is open");
    let id = match item.phase {
        Phase::Ruling { id } => Some(id),
        _ => None,
    };
    item.phase = Phase::Done {
        merged: false,
        closed: true,
    };
    next.rulings.retain(|r| r.issue != Some(issue));
    id
}

#[cfg(test)]
mod tests;
