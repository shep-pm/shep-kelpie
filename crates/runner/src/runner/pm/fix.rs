//! The project manager's call to keep a stuck work item parked on a fix that
//! is already open
//!
//! A fault in kelpie's settings, its own code or this machine is the
//! maintainer's to fix, and the project manager changes none of them. Where it
//! can name the open fix a stuck item waits on, it parks the item there: the
//! ruling stays, and its card names the fix, so the maintainer has both in one
//! place. Nothing is filed here, and a `fix` that names no open issue is
//! dropped and named.

use super::StuckState;
use super::answer::{Action, FixCall};
use crate::board::briefing::ticket_ref;
use crate::runner::Runner;
use crate::state::StateError;

impl Runner {
    /// Keeps `call`'s work item parked on the fix it names, and returns what
    /// it did in words, or why the call was dropped
    ///
    /// # Errors
    ///
    /// [`StateError`] when the ruling it rewords cannot be saved.
    pub(super) fn settle_fix(
        &mut self,
        call: &FixCall,
    ) -> Result<Result<String, String>, StateError> {
        let (named, fix) = (ticket_ref(call.item), ticket_ref(call.on));
        let Some(StuckState::Ruling { id, .. }) = self.stuck(call.item) else {
            return Ok(Err(format!(
                "fix {named}: it is no stuck work item parked on a ruling"
            )));
        };
        if call.on == call.item {
            return Ok(Err(format!("fix {named}: it cannot be its own fix")));
        }
        let issue = match self.ports.tickets.issue(&self.remote, call.on) {
            Ok(issue) => issue,
            Err(e) => return Ok(Err(format!("fix {named}: cannot read {fix}: {e}"))),
        };
        if !issue.open {
            return Ok(Err(format!("fix {named}: {fix} is closed")));
        }
        match self.settle(call.item, id, Action::Ask, &call.why, Some(call.on))? {
            Ok(()) => Ok(Ok(format!("parked {named} on {fix}"))),
            Err(why) => Ok(Err(why)),
        }
    }
}
