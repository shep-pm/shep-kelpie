//! The tickets a project's work items come from, as their source holds them
//!
//! ADR 0014: a ticket lives in exactly one source, which is its truth. Kelpie
//! reads and writes it through this port; the forge port holds none of that.
//! Every project's source today is GitHub issues in the pull request's own
//! repo. A local store and ClickUp follow.
//!
//! The board stays kelpie's whichever source the tickets come from: the saved
//! order, holds and picks, drops and the file-overlap check are its scheduling.

use std::path::{Path, PathBuf};

#[cfg(doc)]
use super::Forge;
use super::{
    FinishedParent, ForgeError, Issue, IssueCloseReason, OpenIssue, OpenTicket, SubIssue,
    ThreadComment,
};
#[cfg(doc)]
use crate::board::READY;
use crate::board::ReadyIssue;
use crate::settings::ForgeSlug;
use crate::ticket::TicketId;

/// The tickets of one project, as their source holds them
///
/// A ticket's id is opaque to kelpie: whatever its source gives it. A GitHub
/// ticket's id is its issue number, so a GitHub source's impl turns each id
/// back into the number its `gh` call needs.
pub trait Tickets: Send + Sync {
    /// Ticket `ticket` on `repo`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked, has no such ticket, or
    /// its answer cannot be read.
    fn issue(&self, repo: &ForgeSlug, ticket: TicketId) -> Result<Issue, ForgeError>;

    /// The sub-issues of ticket `parent` on `repo`, with how each stands
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer cannot be read.
    fn sub_issues(&self, repo: &ForgeSlug, parent: TicketId) -> Result<Vec<SubIssue>, ForgeError>;

    /// The open issues on `repo`, ready or not, that have sub-issues and whose
    /// sub-issues are all closed, whatever they closed as
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn finished_parents(&self, repo: &ForgeSlug) -> Result<Vec<FinishedParent>, ForgeError>;

    /// The open issues on `repo` labelled [`READY`]
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn ready_issues(&self, repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError>;

    /// The open issues on `repo`, for telling a finding already filed
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn open_issues(&self, repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError>;

    /// The open tickets of the project's own source, for telling a finding
    /// already filed
    ///
    /// [`Tickets::open_issues`] is the GitHub issues, which triage reads. This is
    /// the tickets the project's work items come from, which are the same until
    /// the project's source is not GitHub.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the source cannot be asked or its answer read.
    fn open_tickets(&self, repo: &ForgeSlug) -> Result<Vec<OpenTicket>, ForgeError> {
        let open = self.open_issues(repo)?;
        Ok(open
            .into_iter()
            .filter_map(|issue| {
                Some(OpenTicket {
                    ticket: TicketId::from_issue(issue.number)?,
                    title: issue.title,
                    body: issue.body,
                })
            })
            .collect())
    }

    /// Every comment on ticket `ticket` of `repo`, oldest first
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge cannot be asked or its answer read.
    fn issue_comments(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
    ) -> Result<Vec<ThreadComment>, ForgeError>;

    /// Downloads the image at `url`, which [`is_github_attachment`](super::is_github_attachment) accepts, to `stem` plus its extension
    ///
    /// The source's own login goes with the request, so an image on a repo
    /// that needs a login can be read. It goes to GitHub's hosts alone. Nothing
    /// is left behind on an error.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the link is not one this source may fetch, the
    /// download fails or runs past `most` bytes, or what came is not a PNG,
    /// JPEG, GIF or WebP image. A source with no attachments always refuses.
    fn fetch_image(&self, url: &str, stem: &Path, most: u64) -> Result<PathBuf, ForgeError> {
        let _ = (url, stem, most);
        Err(ForgeError::Failed(
            "this source holds no attachments".to_owned(),
        ))
    }

    /// Opens a ticket on `repo` with these labels, and returns its id
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses, or its answer names no ticket.
    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError>;

    /// Files a ticket in the project's own source with these labels, and
    /// returns its id
    ///
    /// [`Tickets::create_issue`] on a source that holds both kinds is the GitHub
    /// issue, which adoption needs a number of. This is where a follow-up
    /// lands.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the source refuses, or its answer names no ticket.
    fn file_ticket(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError> {
        self.create_issue(repo, title, body, labels)
    }

    /// Comments on a ticket of the project's own source other than a GitHub
    /// one, whose comments go through [`Forge::comment`]
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the source takes no comment through this port, or
    /// cannot write it.
    fn comment_ticket(
        &self,
        _repo: &ForgeSlug,
        ticket: TicketId,
        _body: &str,
    ) -> Result<(), ForgeError> {
        Err(ForgeError::Failed(format!(
            "ticket {ticket} takes no comment through the ticket port"
        )))
    }

    /// Closes ticket `ticket`, with no comment: a comment would be posted as the
    /// login `gh` acts as, and only [`Forge::comment`] records the id of one
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn close_issue(&self, repo: &ForgeSlug, ticket: TicketId) -> Result<(), ForgeError>;

    /// Closes ticket `ticket` for `reason`, which the forge records on it
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn close_issue_as(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        reason: IssueCloseReason,
    ) -> Result<(), ForgeError>;

    /// Links ticket `ticket` as blocked by `blocker`, so the source shows the
    /// block too
    ///
    /// A link already there stays as it is. No link is ever removed. False
    /// when `blocker` is a pull request, which no link joins to an issue, so
    /// nothing was added and nothing is wrong.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the source refuses the link, as it does for a
    /// blocker it does not hold, or cannot be asked.
    fn add_blocked_by(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        blocker: TicketId,
    ) -> Result<bool, ForgeError>;

    /// Adds `label` to ticket `ticket`, or takes it off
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the forge refuses or cannot be asked.
    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        ticket: TicketId,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError>;
}
