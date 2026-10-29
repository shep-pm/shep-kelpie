//! The guard as the ticket port, and as both ports at once (ADR 0014)
//!
//! A GitHub project's tickets are its issues, so a ticket read or written
//! through the guard is the source's own call, checked against this machine's
//! paths exactly as a post through the guard's forge port is: an issue's title
//! and body are kelpie's text, and a post that names one is never sent. The
//! impls stand here rather than in `local_paths` itself, so no call site there
//! has both ports in scope at once.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Guarded;
use crate::board::ReadyIssue;
use crate::ports::{
    FinishedParent, Forge, ForgeError, Issue, IssueCloseReason, OpenIssue, OpenTicket, SubIssue,
    ThreadComment, Tickets,
};
use crate::settings::ForgeSlug;
use crate::ticket::TicketId;

impl Guarded {
    /// This guard as both ports a runner reads a source through: one guard
    /// behind the forge port and behind the ticket port, so a call on either
    /// reaches the same guard, rate-limit hold and ledger
    #[must_use]
    pub fn into_ports(self) -> (Arc<dyn Forge>, Arc<dyn Tickets>) {
        let guarded = Arc::new(self);
        (
            Arc::clone(&guarded) as Arc<dyn Forge>,
            Arc::clone(&guarded) as Arc<dyn Tickets>,
        )
    }
}

impl Tickets for Guarded {
    fn issue(&self, repo: &ForgeSlug, ticket: TicketId) -> Result<Issue, ForgeError> {
        self.tickets.issue(repo, ticket)
    }

    fn sub_issues(&self, repo: &ForgeSlug, parent: TicketId) -> Result<Vec<SubIssue>, ForgeError> {
        self.tickets.sub_issues(repo, parent)
    }

    fn finished_parents(&self, repo: &ForgeSlug) -> Result<Vec<FinishedParent>, ForgeError> {
        self.tickets.finished_parents(repo)
    }

    fn ready_issues(&self, repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        self.tickets.ready_issues(repo)
    }

    fn open_issues(&self, repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
        self.tickets.open_issues(repo)
    }

    fn open_tickets(&self, repo: &ForgeSlug) -> Result<Vec<OpenTicket>, ForgeError> {
        self.tickets.open_tickets(repo)
    }

    fn issue_comments(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        self.tickets.issue_comments(repo, ticket)
    }

    fn fetch_image(&self, url: &str, stem: &Path, most: u64) -> Result<PathBuf, ForgeError> {
        self.tickets.fetch_image(url, stem, most)
    }

    fn file_ticket(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError> {
        self.check(repo, "the issue's title", title)?;
        self.check(repo, "the issue's body", body)?;
        self.tickets.file_ticket(repo, title, body, labels)
    }

    fn comment_ticket(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.check(repo, "the comment", body)?;
        self.tickets.comment_ticket(repo, ticket, body)
    }

    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError> {
        self.check(repo, "the issue's title", title)?;
        self.check(repo, "the issue's body", body)?;
        self.tickets.create_issue(repo, title, body, labels)
    }

    fn add_blocked_by(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        blocker: TicketId,
    ) -> Result<(), ForgeError> {
        self.tickets.add_blocked_by(repo, ticket, blocker)
    }

    fn close_issue(&self, repo: &ForgeSlug, ticket: TicketId) -> Result<(), ForgeError> {
        self.tickets.close_issue(repo, ticket)
    }

    fn close_issue_as(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        reason: IssueCloseReason,
    ) -> Result<(), ForgeError> {
        self.tickets.close_issue_as(repo, ticket, reason)
    }

    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.tickets.set_issue_label(repo, ticket, label, on)
    }
}
