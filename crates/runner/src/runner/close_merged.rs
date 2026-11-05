//! Closing the issue of a merged pull request that left it open
//!
//! A pull request merged with no closing reference in its body leaves its
//! issue open, however the merge came about, and anything the issue blocks
//! then waits on it. Once a work item's merge is recorded, a step reads the
//! issue and closes it if the forge still shows it open. A local ticket's pull
//! request has no closing reference, so the step always closes it.
//!
//! An issue the pull request names is the forge's to close. A board pass
//! closes one still open [`FORGE_CLOSE_WAIT`] after its merge.

use super::Runner;
use crate::board::ReadyIssue;
use crate::ports::{SubIssue, SubIssueState};
use crate::ticket::TicketId;

/// Closes the forge may refuse, one in each step, before the step gives up on an issue
const TRIES: u32 = 3;

/// How long the forge is left to close an issue its merged pull request names
/// before kelpie closes it itself
///
/// GitHub closes a named issue about three seconds after the merge, so ten
/// minutes is past any close the forge means to make, and short enough that
/// what the issue blocks waits little on it.
pub(super) const FORGE_CLOSE_WAIT: u64 = 600;

/// What kelpie says on an issue it closes after the pull request merged
/// without closing it
fn merged_without_closing(pull_request: u64) -> String {
    format!("Pull request #{pull_request} merged without closing this issue, so kelpie closes it.")
}

/// What kelpie says on a local ticket it closes after its pull request merged,
/// which names no ticket for the forge to close
fn merged_local(pull_request: u64) -> String {
    format!("Pull request #{pull_request} merged, so kelpie closes this ticket.")
}

/// What kelpie says on a parent it closes after the pull request that closed
/// its last open sub-issue merged
fn last_sub_issue_merged(pull_request: u64) -> String {
    format!(
        "Pull request #{pull_request} closed the last open sub-issue of this issue, so kelpie closes it too."
    )
}

impl Runner {
    /// Closes `issue` if the forge still shows it open after `pull_request`
    /// merged, trying the close once
    ///
    /// The comment goes first, once. A close the forge refuses returns why, and
    /// the step retries after its backoff. After [`TRIES`] refusals it is told
    /// in the log and the work item goes on. A failed comment or unread issue
    /// is told in the log and holds nothing.
    pub(super) fn close_merged_issue(
        &mut self,
        ticket: TicketId,
        pull_request: u64,
    ) -> Option<String> {
        // A local ticket's pull request names no issue for the forge to close, so
        // kelpie closes the ticket, and says nothing on GitHub.
        let Some(issue) = ticket.github() else {
            return self.close_local(ticket, pull_request);
        };
        // The forge closes a named issue a moment after the merge, so one that
        // still reads as open then is left to it.
        let named = (self.ports.forge).reviewed(&self.remote, pull_request);
        if named.is_ok_and(|pr| pr.closes.contains(&issue)) {
            return None;
        }
        match self.ports.tickets.issue(&self.remote, ticket) {
            Ok(read) if !read.open => {
                self.close_refused.remove(&ticket);
                return None;
            }
            Ok(_) => {}
            Err(e) => {
                self.notes.push(format!(
                    "cannot read issue #{issue} after merging #{pull_request}: {e}"
                ));
                return None;
            }
        }
        self.close_with_comment(ticket, pull_request, &merged_without_closing(pull_request))
    }

    /// Closes each ready issue a merged pull request left open, once the forge
    /// has had [`FORGE_CLOSE_WAIT`] to close it
    ///
    /// Only a finished issue on the board's ready list is swept, since the
    /// board reads no other. A close the forge refuses is tried [`TRIES`] times
    /// across the passes that follow, with the comment once, and then left be
    /// until the runner restarts.
    pub(super) fn close_merged_left_open(&mut self, ready: &[ReadyIssue]) {
        let now = self.ports.clock.now();
        let left: Vec<(TicketId, u64)> = ready
            .iter()
            .filter_map(|issue| {
                // A ticket worked twice is what its last work item did with it
                let record = self
                    .state
                    .history
                    .iter()
                    .rfind(|r| r.issue == issue.ticket)?;
                let (true, Some(pull_request)) = (record.merged, record.pull_request) else {
                    return None;
                };
                let waited = now.0 >= record.at.0.saturating_add(FORGE_CLOSE_WAIT);
                let given_up = (self.close_refused.get(&issue.ticket)).is_some_and(|n| *n >= TRIES);
                (waited && !given_up).then_some((issue.ticket, pull_request))
            })
            .collect();
        for (ticket, pull_request) in left {
            match self.ports.tickets.issue(&self.remote, ticket) {
                Ok(read) if !read.open => {
                    self.close_refused.remove(&ticket);
                }
                Ok(_) => {
                    let text = merged_without_closing(pull_request);
                    self.close_with_comment(ticket, pull_request, &text);
                }
                Err(e) => self.notes.push(format!(
                    "cannot read {} a merge left open: {e}",
                    super::turn::ticket_name(ticket)
                )),
            }
        }
    }

    // Closes a local ticket that is still open after `pull_request` merged, with
    // a note on the ticket, on the first try only
    fn close_local(&mut self, ticket: TicketId, pull_request: u64) -> Option<String> {
        match self.ports.tickets.issue(&self.remote, ticket) {
            Ok(read) if !read.open => {
                self.close_refused.remove(&ticket);
                return None;
            }
            Ok(_) => {}
            Err(e) => {
                self.notes.push(format!(
                    "cannot read {} after merging #{pull_request}: {e}",
                    ticket.shown()
                ));
                return None;
            }
        }
        self.close_with_comment(ticket, pull_request, &merged_local(pull_request))
    }

    // The comment, on the first try only, and the close that follows it
    fn close_with_comment(
        &mut self,
        ticket: TicketId,
        pull_request: u64,
        text: &str,
    ) -> Option<String> {
        if !self.close_refused.contains_key(&ticket)
            && let Err(e) = self.comment_on_ticket(ticket, text)
        {
            self.notes.push(format!(
                "cannot comment on {} before closing it: {e}",
                super::turn::ticket_name(ticket)
            ));
        }
        self.close_after_merge(ticket, pull_request)
    }

    // The close itself, with the count of refusals the step gives up on. A
    // ticket given up on keeps its count, so a later pass leaves it be.
    fn close_after_merge(&mut self, ticket: TicketId, pull_request: u64) -> Option<String> {
        let Err(e) = self.ports.tickets.close_issue(&self.remote, ticket) else {
            self.close_refused.remove(&ticket);
            return None;
        };
        let failures = self.close_refused.entry(ticket).or_default();
        *failures += 1;
        let name = super::turn::ticket_name(ticket);
        let reason = format!("cannot close {name} after merging #{pull_request}: {e}");
        if *failures < TRIES {
            return Some(reason);
        }
        self.notes.push(format!("{reason}, refused {TRIES} times"));
        None
    }
}

impl Runner {
    /// Closes the parent of `issue` if `pull_request`'s merge left every one of
    /// its sub-issues closed and at least one of them built, never holding the
    /// work item
    ///
    /// `issue` and the issues the pull request names count as completed. The
    /// forge closes them a moment after the merge. Only the parent is closed.
    /// A read or a close that fails is told in the log.
    pub(super) fn close_done_parent_of(&mut self, ticket: TicketId, pull_request: u64) {
        let parent = match self.ports.tickets.issue(&self.remote, ticket) {
            Ok(read) => read.parent,
            Err(e) => {
                self.notes.push(format!(
                    "cannot read the parent of issue {} after merging #{pull_request}: {e}",
                    ticket.shown()
                ));
                return;
            }
        };
        let Some(parent) = parent else { return };
        match self.ports.tickets.issue(&self.remote, parent) {
            Ok(read) if !read.open => return,
            Ok(_) => {}
            Err(e) => {
                self.notes.push(format!(
                    "cannot read the parent #{parent} of issue {} after merging #{pull_request}: {e}",
                    ticket.shown()
                ));
                return;
            }
        }
        let subs = match self.ports.tickets.sub_issues(&self.remote, parent) {
            Ok(subs) => subs,
            Err(e) => {
                self.notes.push(format!(
                    "cannot read the sub-issues of #{parent} after merging #{pull_request}: {e}"
                ));
                return;
            }
        };
        // A read that fails leaves only `issue` to count, so the parent may stay
        // open until a later merge under it.
        let named = (self.ports.forge).reviewed(&self.remote, pull_request);
        let named: Vec<TicketId> = (named.map(|pr| pr.closes).unwrap_or_default().into_iter())
            .filter_map(TicketId::from_issue)
            .collect();
        let state = |s: &SubIssue| match (s.ticket == ticket || named.contains(&s.ticket), s.state)
        {
            (true, SubIssueState::Open) => SubIssueState::Completed,
            _ => s.state,
        };
        let done = subs.iter().all(|s| state(s) != SubIssueState::Open);
        let built = subs.iter().any(|s| state(s) == SubIssueState::Completed);
        if !(done && built) {
            return;
        }
        if let Err(e) = self.comment_on_ticket(parent, &last_sub_issue_merged(pull_request)) {
            self.notes.push(format!(
                "cannot comment on {} before closing it: {e}",
                super::turn::ticket_name(parent)
            ));
        }
        if let Err(e) = self.ports.tickets.close_issue(&self.remote, parent) {
            self.notes.push(format!(
                "cannot close {} after merging #{pull_request}: {e}",
                super::turn::ticket_name(parent)
            ));
        }
    }
}
