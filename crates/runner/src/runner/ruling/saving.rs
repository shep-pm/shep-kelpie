//! Saving a change to a work item
//!
//! Every change goes through a copy of the state, saved before it stands in
//! memory. `update` changes the work item the step in hand is working on;
//! `update_of` changes the one a ticket names, which no step holds.

use super::super::Runner;
use crate::state::StateError;
use crate::ticket::TicketId;
use crate::work_item::WorkItem;

impl Runner {
    pub(in crate::runner) fn update(
        &mut self,
        change: impl FnOnce(&mut WorkItem),
    ) -> Result<(), StateError> {
        let mut next = self.state.clone();
        change(
            self.current_in(&mut next)
                .expect("a change to the work item in flight"),
        );
        self.save(next)
    }

    /// Saves `change` to the work item for `ticket`, which the step in hand may not have in focus
    ///
    /// A project manager's wake acts on the item it was woken for, and no step
    /// holds it. A ticket whose work item is gone saves nothing.
    ///
    /// # Errors
    ///
    /// [`StateError`] when it cannot be saved.
    pub(in crate::runner) fn update_of(
        &mut self,
        ticket: TicketId,
        change: impl FnOnce(&mut WorkItem),
    ) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let Some(item) = next.item_mut(ticket) else {
            return Ok(());
        };
        change(item);
        self.save(next)
    }
}
