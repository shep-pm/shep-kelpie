//! What a work item from a state file older than review bots in the pass
//! owes them, settled once the runner knows the project's list
//!
//! An adopted pull request's owed summon becomes owed by each listed bot.
//! An item past its review that no bot read owes the listed bots a pass
//! before its merge: CI's next green run starts it, and a merge ruling
//! already pending is withdrawn so CI can, which the log and the webhook
//! say. With no bot listed, it owes none.

use super::Runner;
use crate::ports::Alert;
use crate::state::{RulingKind, StateError};
use crate::work_item::Phase;

impl Runner {
    /// Settles what the work items an older state file loaded owe the
    /// review bots the project lists
    ///
    /// # Errors
    ///
    /// [`StateError::Write`] when the change cannot be saved.
    pub(super) fn settle_older_bots(&mut self) -> Result<(), StateError> {
        let listed: Vec<_> = self.listed_bots().iter().map(|bot| bot.bot).collect();
        let owing = |item: &crate::work_item::WorkItem| item.summon_owed || item.bots_after_ci;
        if !self.state.work_items.iter().any(owing) {
            return Ok(());
        }
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let mut withdrawn = Vec::new();
        for item in &mut next.work_items {
            if std::mem::take(&mut item.summon_owed) {
                item.summons_owed = listed.iter().copied().collect();
            }
            if !item.bots_after_ci {
                continue;
            }
            if listed.is_empty() {
                item.bots_after_ci = false;
                continue;
            }
            let Phase::Ruling { id } = item.phase else {
                continue;
            };
            let merge =
                |r: &crate::state::Ruling| r.id == id && matches!(r.kind, RulingKind::Merge { .. });
            if self.state.rulings.iter().any(merge) {
                withdrawn.push((id, item.issue, item.pull_request));
                item.phase = Phase::Ci {
                    head: None,
                    since: now,
                };
            }
        }
        next.rulings
            .retain(|r| !withdrawn.iter().any(|(id, ..)| *id == r.id));
        self.save(next)?;
        for (id, issue, number) in withdrawn {
            self.say_withdrawn(id, issue, number);
        }
        Ok(())
    }

    // Logs the withdrawn merge ruling and posts it once to the webhook,
    // where one is set: no ruling is left to retry the post for.
    fn say_withdrawn(&mut self, id: u64, issue: u64, number: Option<u64>) {
        let project = self.project.as_str().to_owned();
        let about = number.map_or_else(
            || format!("issue #{issue}"),
            |n| format!("pull request #{n}"),
        );
        let text = format!(
            "Merge ruling {id} on {about} is withdrawn: no review bot {project} lists had \
             read it, so they read it first, and the merge ruling is raised again after \
             they have. Nothing to answer."
        );
        self.notes.push(text.clone());
        let Some(webhook) = self.webhook.clone() else {
            return;
        };
        let alert = Alert {
            title: format!("kelpie: {project} merge ruling {id} withdrawn"),
            text,
            reply: None,
        };
        if let Err(e) = self.ports.alerts.post(&webhook, &alert) {
            let failed = format!("cannot post that merge ruling {id} is withdrawn: {e}");
            self.notes.push(failed);
        }
    }

    /// Whether the work item owes the listed bots a pass before its merge,
    /// and if so starts it, its other reviewers having read it
    pub(super) fn bots_before_merge(&mut self) -> Result<bool, StateError> {
        let owed = self.current().is_some_and(|item| item.bots_after_ci);
        if !owed {
            return Ok(false);
        }
        let listed = !self.listed_bots().is_empty();
        self.update(|item| {
            item.bots_after_ci = false;
            if listed {
                item.phase = Phase::Review(crate::work_item::Review {
                    bots_only: true,
                    unread: false,
                    ..crate::work_item::Review::first()
                });
            }
        })?;
        Ok(listed)
    }
}

#[cfg(test)]
mod tests;
