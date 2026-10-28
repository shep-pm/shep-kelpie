//! Work items whose issue was closed with no change
//!
//! A work item ends once its issue closes and its branch and worktree hold
//! no work. A turn that ends so finishes it with no nudge and no ruling. A
//! pass ends a parked one and withdraws its ruling. Kelpie cannot tell who
//! closed the issue. Work found, or a git that cannot say, leaves the item
//! to go on as before. A parked item with a pull request merged by hand
//! reaches no gate. A pass ends it the same way. One closed without merging
//! gets the gate's `closed` ruling in place of its old one.

use super::Runner;
use crate::ports::Alert;
use crate::state::{ProjectState, RulingKind};
use crate::ticket::TicketId;
use crate::work_item::{Phase, WorkItem};
use crate::worktree::WorktreeError;

/// Seconds between reads of a parked work item's issue. A parked item waits
/// hours, and each read is a forge call and a fetch.
pub(super) const PARKED_READ: u64 = 5 * 60;

impl Runner {
    /// Whether `ticket`'s ticket is closed and its work item's branch and
    /// worktree hold no work. A git that cannot say is noted, and counts as
    /// work.
    ///
    /// # Errors
    ///
    /// Why the forge could not be asked.
    pub(super) fn closed_with_no_change(&mut self, ticket: TicketId) -> Result<bool, String> {
        let read = self.ports.tickets.issue(&self.remote, ticket);
        if read
            .map_err(|e| format!("cannot read {}: {e}", ticket.named().to_lowercase()))?
            .open
        {
            return Ok(false);
        }
        Ok(self.holds_no_work(ticket))
    }

    // Whether the branch and worktree of `ticket`'s work item hold no work. A git that cannot
    // say is noted, and counts as work.
    fn holds_no_work(&mut self, ticket: TicketId) -> bool {
        let read = self.tree_to_check(ticket).map(|tree| tree.holds_work());
        self.no_work(ticket, read)
    }

    // Whether `read`, of whether the work item of `ticket` holds work, found none. A work item
    // that is gone, or a git that cannot say, counts as work, and the latter is noted.
    fn no_work(&mut self, ticket: TicketId, read: Option<Result<bool, WorktreeError>>) -> bool {
        match read {
            None => false,
            Some(Ok(held)) => !held,
            Some(Err(e)) => {
                let note = format!(
                    "{} is closed, but its work cannot be read: {e}",
                    ticket.named().to_lowercase()
                );
                self.notes.push(note);
                false
            }
        }
    }

    // Whether `item` is parked on a ruling that `kind` matches
    pub(super) fn parked_on(&self, item: &WorkItem, kind: impl Fn(&RulingKind) -> bool) -> bool {
        let Phase::Ruling { id } = item.phase else {
            return false;
        };
        (self.state.rulings.iter())
            .find(|r| r.id == id)
            .is_some_and(|r| kind(&r.kind))
    }

    /// Logs that pull request `number` was merged, so the work item for
    /// `ticket` ends, and that ruling `id` is withdrawn, if it was parked on
    /// one. Posts it once to the webhook, where one is set: nothing is left
    /// to retry it for.
    pub(super) fn say_merged(&mut self, ticket: TicketId, number: u64, id: Option<TicketId>) {
        let project = self.project.as_str().to_owned();
        let withdrawn = id.map_or_else(String::new, |id| format!(" Ruling {id} is withdrawn."));
        let text = format!(
            "Pull request #{number} was merged, so work item {} ends.{withdrawn} \
             Nothing to answer.",
            ticket.shown()
        );
        self.notes.push(text.clone());
        let Some(webhook) = self.webhook.clone() else {
            return;
        };
        let alert = Alert {
            title: format!("kelpie: {project} {} merged", ticket.shown()),
            text,
            reply: None,
        };
        if let Err(e) = self.ports.alerts.post(&webhook, &alert) {
            let failed = format!("cannot post that #{number} was merged: {e}");
            self.notes.push(failed);
        }
    }

    /// Logs that the ticket `ticket` was closed with no change, so its work
    /// item ends, and that ruling `id` is withdrawn, if it was parked on one.
    /// Posts it once to the webhook, where one is set: nothing is left to
    /// retry it for.
    pub(super) fn say_closed(&mut self, ticket: TicketId, id: Option<TicketId>) {
        let withdrawn = id.map_or_else(String::new, |id| format!(" Ruling {id} is withdrawn."));
        let text = format!(
            "{} was closed with no change, so its work item ends with no pull \
             request.{withdrawn} Nothing to answer.",
            ticket.named()
        );
        self.announce(ticket, text, "closed with no change");
    }

    // Logs `text`, which says why the work item for `ticket` ends, notices it on the
    // ticket's thread and posts it once to the webhook, where one is set
    fn announce(&mut self, ticket: TicketId, text: String, outcome: &str) {
        let project = self.project.as_str().to_owned();
        self.notes.push(text.clone());
        // A local ticket has no forge thread to post the notice on.
        if let Some(number) = ticket.github() {
            self.notice_on(number, &text);
        }
        let Some(webhook) = self.webhook.clone() else {
            return;
        };
        let alert = Alert {
            title: format!("kelpie: {project} {} {outcome}", ticket.shown()),
            text,
            reply: None,
        };
        if let Err(e) = self.ports.alerts.post(&webhook, &alert) {
            let failed = format!("cannot post that {} was {outcome}: {e}", ticket.shown());
            self.notes.push(failed);
        }
    }
}

/// Marks `ticket`'s work item in `next` to end, its pull request merged, and
/// drops its rulings. Returns the ruling it was parked on, if any, or `None`
/// where the work item is not open.
fn end_merged(next: &mut ProjectState, ticket: TicketId) -> Option<Option<TicketId>> {
    end(
        next,
        ticket,
        Phase::Done {
            merged: true,
            closed: false,
        },
    )
}

/// Marks `ticket`'s work item in `next` to end, closed with no change, and
/// drops its rulings. Returns the ruling it was parked on, if any, or `None`
/// where the work item is not open.
pub(super) fn end_closed(next: &mut ProjectState, ticket: TicketId) -> Option<Option<TicketId>> {
    end(
        next,
        ticket,
        Phase::Done {
            merged: false,
            closed: true,
        },
    )
}

fn end(next: &mut ProjectState, ticket: TicketId, done: Phase) -> Option<Option<TicketId>> {
    let item = next.item_mut(ticket)?;
    let id = match item.phase {
        Phase::Ruling { id } => Some(id),
        _ => None,
    };
    item.phase = done;
    next.rulings.retain(|r| r.issue != Some(ticket));
    Some(id)
}

pub(super) use read::read_parked;
mod covered;
pub(super) use covered::{covering, end_covered};
mod read;
#[cfg(test)]
mod tests;
