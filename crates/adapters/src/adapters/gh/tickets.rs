//! GitHub's issues as the ticket port (ADR 0014)
//!
//! A GitHub project's tickets are its issues, so each method here is the `gh`
//! call for one. The impl stands apart from `gh` itself, so no `gh` call site
//! has both ports in scope at once.
//!
//! A GitHub ticket's id is its issue number, so each id is turned back into
//! the number the `gh` call needs. A ticket of another source has none.

use std::path::{Path, PathBuf};

use super::{Gh, attachment, blocked, board, issue, issues, thread};
use crate::board::ReadyIssue;
use crate::ports::{
    FinishedParent, ForgeError, Issue, IssueCloseReason, OpenIssue, SubIssue, ThreadComment,
    Tickets,
};
use crate::settings::ForgeSlug;
use crate::ticket::TicketId;

// The issue number `ticket` is, for the `gh` call that needs one
fn number(ticket: TicketId) -> Result<u64, ForgeError> {
    ticket.github().ok_or(ForgeError::NoIssue { ticket })
}

impl Tickets for Gh {
    fn issue(&self, repo: &ForgeSlug, ticket: TicketId) -> Result<Issue, ForgeError> {
        issue(None, repo, number(ticket)?)
    }

    fn sub_issues(&self, repo: &ForgeSlug, parent: TicketId) -> Result<Vec<SubIssue>, ForgeError> {
        issues::sub_issues(repo, number(parent)?)
    }

    fn finished_parents(&self, repo: &ForgeSlug) -> Result<Vec<FinishedParent>, ForgeError> {
        issues::finished_parents(repo)
    }

    fn ready_issues(&self, repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        board::ready_issues(None, repo)
    }

    fn open_issues(&self, repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
        issues::open_issues(repo)
    }

    fn issue_comments(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        thread::issue_comments(&self.etags, repo, number(ticket)?)
    }

    fn fetch_image(&self, url: &str, stem: &Path, most: u64) -> Result<PathBuf, ForgeError> {
        attachment::fetch_image(url, stem, most)
    }

    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError> {
        issues::create_issue(repo, title, body, labels)
    }

    fn close_issue(&self, repo: &ForgeSlug, ticket: TicketId) -> Result<(), ForgeError> {
        issues::close_issue(repo, number(ticket)?)
    }

    fn close_issue_as(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        reason: IssueCloseReason,
    ) -> Result<(), ForgeError> {
        issues::close_issue_as(repo, number(ticket)?, reason)
    }

    fn add_blocked_by(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        blocker: TicketId,
    ) -> Result<bool, ForgeError> {
        blocked::add_blocked_by(repo, number(ticket)?, number(blocker)?)
    }

    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        issues::set_label(repo, number(ticket)?, label, on)
    }
}
