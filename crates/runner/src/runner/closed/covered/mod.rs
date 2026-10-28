//! Work items another pull request covers
//!
//! An open pull request that closes a work item's issue and that kelpie's own account did not
//! open covers it, as does any from a fork: the item ends with no change and its issue's thread
//! names the pull request, and the contribution flow takes the pull request from there. A pull
//! request by kelpie's account on another branch is the item's own work and keeps it, as does one
//! on the item's own branch. An item with a pull request of its own is left alone, since the
//! maintainer then has two to choose between, and is asked at its merge ruling.

use super::end;
use crate::board::{OpenPullRequest, closes};
use crate::runner::Runner;
use crate::runner::contributions::Logins;
use crate::state::ProjectState;
use crate::ticket::TicketId;
use crate::work_item::Phase;

/// The open pull requests in `open` that cover `ticket`: not on `branch`, closing `ticket`'s
/// issue, and opened by someone other than the account `own` acts as, or from a fork
pub(in crate::runner) fn covering<'a>(
    open: &'a [OpenPullRequest],
    ticket: TicketId,
    branch: &'a str,
    own: &'a Logins,
) -> impl Iterator<Item = u64> + 'a {
    (open.iter())
        .filter(move |pr| {
            pr.head != branch && closes(pr, ticket) && (pr.from_fork || !own.acting(&pr.author))
        })
        .map(|pr| pr.number)
}

/// Marks `ticket`'s work item in `next` to end with no change, covered by another pull request,
/// and drops its rulings. Returns the ruling it was parked on, if any, or `None` where the work
/// item is not open.
pub(in crate::runner) fn end_covered(
    next: &mut ProjectState,
    ticket: TicketId,
) -> Option<Option<TicketId>> {
    end(
        next,
        ticket,
        Phase::Done {
            merged: false,
            closed: false,
        },
    )
}

impl Runner {
    /// The open pull request in `open` that covers `ticket`'s work item on `branch`
    ///
    /// A login that cannot be read is noted and leaves the item as it is.
    pub(in crate::runner) fn covered_by(
        &mut self,
        open: &[OpenPullRequest],
        ticket: TicketId,
        branch: &str,
    ) -> Option<u64> {
        if !open.iter().any(|pr| closes(pr, ticket)) {
            return None;
        }
        covering(open, ticket, branch, &self.own_logins_noted()?).next()
    }

    /// Whether a pull request covers `ticket`'s work item on `branch`, listing the open ones. A
    /// list that cannot be read is noted and says no.
    pub(in crate::runner) fn covered_now(&mut self, ticket: TicketId, branch: &str) -> bool {
        match self.ports.forge.open_pull_requests(&self.remote) {
            Ok(open) => self.covered_by(&open, ticket, branch).is_some(),
            Err(e) => {
                self.notes
                    .push(format!("cannot list open pull requests: {e}"));
                false
            }
        }
    }

    /// The other open pull requests that cover the current work item, which its merge ruling
    /// names. The reason when they cannot be listed or the logins cannot be read.
    pub(in crate::runner) fn rivals(&mut self) -> Result<Vec<u64>, String> {
        let Some(item) = self.current() else {
            return Ok(Vec::new());
        };
        let (ticket, branch, own_number) = (item.issue, item.branch.clone(), item.pull_request);
        let open = (self.ports.forge.open_pull_requests(&self.remote))
            .map_err(|e| format!("cannot list open pull requests: {e}"))?;
        // The logins are asked only when a pull request might cover the item.
        if !open
            .iter()
            .any(|pr| closes(pr, ticket) && pr.head != branch)
        {
            return Ok(Vec::new());
        }
        let logins = (self.own_logins()).map_err(|e| format!("cannot read the logins: {e}"))?;
        let rivals = covering(&open, ticket, &branch, &logins);
        Ok(rivals.filter(|n| Some(*n) != own_number).collect())
    }

    /// The logins whose pull requests cover no work item, or none when they cannot be read,
    /// which is noted
    pub(in crate::runner) fn own_logins_noted(&mut self) -> Option<Logins> {
        match self.own_logins() {
            Ok(own) => Some(own),
            Err(e) => {
                let note = format!("cannot tell whose pull request covers a work item: {e}");
                self.notes.push(note);
                None
            }
        }
    }

    /// Logs that pull request `number` covers the work item for `ticket`, so it ends, and that
    /// ruling `id` is withdrawn, if it was parked on one. Posts it once to the webhook, where one
    /// is set: nothing is left to retry it for.
    pub(in crate::runner) fn say_covered(
        &mut self,
        ticket: TicketId,
        number: u64,
        id: Option<TicketId>,
    ) {
        let withdrawn = id.map_or_else(String::new, |id| format!(" Ruling {id} is withdrawn."));
        let text = format!(
            "Pull request #{number} closes {}, so its work item ends with no change.{withdrawn} \
             Nothing to answer.",
            ticket.named().to_lowercase()
        );
        self.announce(ticket, text, "covered by another pull request");
    }
}

#[cfg(test)]
mod tests;
