//! A project's two ticket sources behind one port

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::LocalTickets;
use crate::board::ReadyIssue;
use crate::ports::{
    FinishedParent, ForgeError, Issue, IssueCloseReason, OpenIssue, OpenTicket, SubIssue,
    ThreadComment, Tickets,
};
use crate::settings::{ForgeSlug, TicketSource};
use crate::ticket::TicketId;
pub use source::SharedSource;

/// The project's GitHub tickets and its local store, each ticket sent to its own
///
/// A work item keeps the ticket it was opened on across a change of the
/// project's `tickets.source`, so a call about one ticket goes to the source
/// its id names. The board's reads, filing a ticket and listing the open ones
/// go to the project's source, which a [`SharedSource`] changes in use.
/// `create_issue` and `open_issues` stay on GitHub, since adoption files a
/// GitHub issue and needs its number, and triage reads GitHub's.
pub struct Routed {
    github: Arc<dyn Tickets>,
    local: LocalTickets,
    source: SharedSource,
}

impl core::fmt::Debug for Routed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Routed")
            .field("local", &self.local)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl Routed {
    /// `github` and `local`, with the board reading `source`, a
    /// [`TicketSource`] or a [`SharedSource`] the caller keeps to change it
    pub fn new(
        github: Arc<dyn Tickets>,
        local: LocalTickets,
        source: impl Into<SharedSource>,
    ) -> Self {
        Self {
            github,
            local,
            source: source.into(),
        }
    }

    /// The handle to the source the board reads
    pub fn source_cell(&self) -> SharedSource {
        self.source.clone()
    }

    fn of(&self, ticket: TicketId) -> &dyn Tickets {
        match ticket {
            TicketId::Github(_) => self.github.as_ref(),
            TicketId::Local(_) => &self.local,
        }
    }

    fn board(&self) -> &dyn Tickets {
        match self.source.get() {
            TicketSource::Github => self.github.as_ref(),
            TicketSource::Local => &self.local,
        }
    }
}

impl Tickets for Routed {
    fn issue(&self, repo: &ForgeSlug, ticket: TicketId) -> Result<Issue, ForgeError> {
        self.of(ticket).issue(repo, ticket)
    }

    fn sub_issues(&self, repo: &ForgeSlug, parent: TicketId) -> Result<Vec<SubIssue>, ForgeError> {
        self.of(parent).sub_issues(repo, parent)
    }

    fn finished_parents(&self, repo: &ForgeSlug) -> Result<Vec<FinishedParent>, ForgeError> {
        self.board().finished_parents(repo)
    }

    fn ready_issues(&self, repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        self.board().ready_issues(repo)
    }

    fn open_issues(&self, repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
        self.github.open_issues(repo)
    }

    fn open_tickets(&self, repo: &ForgeSlug) -> Result<Vec<OpenTicket>, ForgeError> {
        self.board().open_tickets(repo)
    }

    fn issue_comments(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        self.of(ticket).issue_comments(repo, ticket)
    }

    fn fetch_image(&self, url: &str, stem: &Path, most: u64) -> Result<PathBuf, ForgeError> {
        self.github.fetch_image(url, stem, most)
    }

    fn file_ticket(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError> {
        self.board().file_ticket(repo, title, body, labels)
    }

    fn comment_ticket(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.of(ticket).comment_ticket(repo, ticket, body)
    }

    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError> {
        self.github.create_issue(repo, title, body, labels)
    }

    fn close_issue(&self, repo: &ForgeSlug, ticket: TicketId) -> Result<(), ForgeError> {
        self.of(ticket).close_issue(repo, ticket)
    }

    fn close_issue_as(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        reason: IssueCloseReason,
    ) -> Result<(), ForgeError> {
        self.of(ticket).close_issue_as(repo, ticket, reason)
    }

    fn add_blocked_by(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        blocker: TicketId,
    ) -> Result<(), ForgeError> {
        self.of(ticket).add_blocked_by(repo, ticket, blocker)
    }

    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.of(ticket).set_issue_label(repo, ticket, label, on)
    }
}

mod source;

#[cfg(test)]
mod tests;
