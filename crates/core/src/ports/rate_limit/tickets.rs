//! The rate-limit hold as the ticket port, and as both ports at once (ADR
//! 0014)
//!
//! A GitHub project's tickets are its issues, and every read and write of one
//! is held, paced and ledged as the same call through the forge port is. The
//! impl stands here rather than in `rate_limit` itself, so no call site there
//! has both ports in scope at once.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::RateHeld;
use super::app_reads::Read;
use crate::board::ReadyIssue;
use crate::forge_ledger::Kind;
use crate::ports::{
    FinishedParent, Forge, ForgeError, Issue, IssueCloseReason, OpenIssue, SubIssue, ThreadComment,
    Tickets,
};
use crate::settings::ForgeSlug;
use crate::ticket::TicketId;

impl RateHeld {
    /// This hold as both ports a runner reads a source through: one hold
    /// behind the forge port and behind the ticket port, so a call on either
    /// is held, paced and ledged the same way
    #[must_use]
    pub fn into_ports(self) -> (Arc<dyn Forge>, Arc<dyn Tickets>) {
        let held = Arc::new(self);
        (
            Arc::clone(&held) as Arc<dyn Forge>,
            Arc::clone(&held) as Arc<dyn Tickets>,
        )
    }
}

impl Tickets for RateHeld {
    fn issue(&self, repo: &ForgeSlug, ticket: TicketId) -> Result<Issue, ForgeError> {
        self.read_tickets(
            repo,
            Read::Issue,
            |a| a.issue(repo, ticket),
            |t| t.issue(repo, ticket),
        )
    }

    fn sub_issues(&self, repo: &ForgeSlug, parent: TicketId) -> Result<Vec<SubIssue>, ForgeError> {
        // Stays on the login: the App's token reaches one repo, and a parent's
        // sub-issues may sit in another, so a list from the App could be short.
        self.call_tickets(Kind::SubIssues, |t| t.sub_issues(repo, parent))
    }

    fn finished_parents(&self, repo: &ForgeSlug) -> Result<Vec<FinishedParent>, ForgeError> {
        // Stays on the login, as `sub_issues` does: it names parents for the
        // runner to close, and the App's token reaches one repo.
        self.call_tickets(Kind::FinishedParents, |t| t.finished_parents(repo))
    }

    fn ready_issues(&self, repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        self.read_tickets(
            repo,
            Read::ReadyIssues,
            |a| a.ready_issues(repo),
            |t| t.ready_issues(repo),
        )
    }

    fn open_issues(&self, repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
        self.call_tickets(Kind::OpenIssues, |t| t.open_issues(repo))
    }

    fn issue_comments(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        self.call_tickets(Kind::IssueComments, |t| t.issue_comments(repo, ticket))
    }

    // Not a call to the forge's API, so it spends none of the hold's allowance.
    fn fetch_image(&self, url: &str, stem: &Path, most: u64) -> Result<PathBuf, ForgeError> {
        self.tickets.fetch_image(url, stem, most)
    }

    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError> {
        self.call_tickets(Kind::CreateIssue, |t| {
            t.create_issue(repo, title, body, labels)
        })
    }

    fn close_issue(&self, repo: &ForgeSlug, ticket: TicketId) -> Result<(), ForgeError> {
        self.call_tickets(Kind::CloseIssue, |t| t.close_issue(repo, ticket))
    }

    fn close_issue_as(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        reason: IssueCloseReason,
    ) -> Result<(), ForgeError> {
        self.call_tickets(Kind::CloseIssueAs, |t| {
            t.close_issue_as(repo, ticket, reason)
        })
    }

    fn add_blocked_by(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        blocker: TicketId,
    ) -> Result<(), ForgeError> {
        self.call_tickets(Kind::AddBlockedBy, |t| {
            t.add_blocked_by(repo, ticket, blocker)
        })
    }

    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.call_tickets(Kind::SetIssueLabel, |t| {
            t.set_issue_label(repo, ticket, label, on)
        })
    }
}
