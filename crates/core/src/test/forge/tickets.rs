//! The rig's tickets: the same issues its forge holds (ADR 0014)
//!
//! The rig has one forge, so a ticket read or written through it lands on the
//! same fake a test set up. The impl stands here rather than in `forge` itself,
//! so no call site there has both ports in scope at once.
//!
//! No runner call reaches it. `Runner::open` wraps the forge it is given in the
//! guard and takes both ports from that one guard, so a ticket write lands on
//! the rig's forge impl whichever port it was asked of. Which port a call used
//! is a rule over the source, and `scripts/ticket-port.sh` holds it.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::{CreatedIssue, FakeForge, held};
use crate::board::{READY, ReadyIssue};
use crate::ports::{
    FinishedParent, ForgeError, Issue, IssueCloseReason, OpenIssue, SubIssue, ThreadComment,
    Tickets,
};
use crate::settings::ForgeSlug;
use crate::test::ticket;
use crate::ticket::TicketId;

// The issue number of a ticket the rig is asked for. Every test's project has
// GitHub issues as its tickets, so every ticket the rig is asked for is one.
// A ready issue's labels as the board reads them. With no priority it reads
// as `priority: P4`, and with no kind as `kind:fix`, so it passes the check.
fn checked_labels(mut labels: Vec<String>) -> Vec<String> {
    if !labels.iter().any(|l| crate::board::is_priority_label(l)) {
        labels.push("priority: P4".to_owned());
    }
    if !labels
        .iter()
        .any(|l| crate::board::check::kind_of(l).is_some())
    {
        labels.push("kind:fix".to_owned());
    }
    labels
}

fn number(ticket: TicketId) -> u64 {
    ticket.github().expect("a test's ticket is a GitHub issue")
}

impl Tickets for FakeForge {
    fn issue(&self, _repo: &ForgeSlug, id: TicketId) -> Result<Issue, ForgeError> {
        let number = number(id);
        self.ask()?;
        self.wait_if_held(held::At::Issue);
        if self.missing.lock().unwrap().contains(&number) {
            return Err(ForgeError::Failed(format!("no issue #{number}")));
        }
        if let Some(allowed) = self.unread.lock().unwrap().get_mut(&number) {
            if *allowed == 0 {
                return Err(ForgeError::Failed(format!(
                    "#{number} cannot be read right now"
                )));
            }
            *allowed -= 1;
        }
        // Each read takes its own lock, so none may be held across another.
        let open = !self.closed.lock().unwrap().contains(&number);
        let filed = (self.issues.lock().unwrap().iter())
            .find(|i| i.number == number)
            .map(|i| (i.title.clone(), i.body.clone()));
        let (title, body) = filed.unwrap_or_else(|| {
            (
                format!("Title of #{number}"),
                format!("Body of #{number}.\n"),
            )
        });
        Ok(Issue {
            title,
            body,
            labels: self.labels_of(number),
            open,
            parent: self.parent_of(number).map(ticket),
            blocked_by: self
                .blockers_of(number)
                .into_iter()
                .map(|b| b.ticket)
                .collect(),
        })
    }

    fn sub_issues(&self, _repo: &ForgeSlug, parent: TicketId) -> Result<Vec<SubIssue>, ForgeError> {
        self.read_sub_issues(number(parent))
    }

    fn finished_parents(&self, _repo: &ForgeSlug) -> Result<Vec<FinishedParent>, ForgeError> {
        self.read_finished_parents()
    }

    fn ready_issues(&self, _repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        self.board_reads.fetch_add(1, Ordering::SeqCst);
        self.ask()?;
        self.wait_if_held(held::At::Board);
        self.board()?;
        let ready = self.ready.lock().unwrap().clone();
        let closed = self.closed.lock().unwrap().clone();
        let unchecked = self.unchecked.lock().unwrap().clone();
        Ok(ready
            .into_iter()
            .filter(|i| !closed.contains(&number(i.ticket)))
            .map(|i| {
                let number = number(i.ticket);
                ReadyIssue {
                    labels: match unchecked.contains(&number) {
                        true => self.labels_of(number),
                        false => checked_labels(self.labels_of(number)),
                    },
                    blocked_by: self.blockers_of(number),
                    parent: self.parent_of(number).map(ticket),
                    sub_issues: self.count_sub_issues(number),
                    ..i
                }
            })
            .collect())
    }

    fn open_issues(&self, _repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
        self.issue_listings.fetch_add(1, Ordering::SeqCst);
        self.ask()?;
        // The listing is as the forge had it when the read began, however long a
        // held read takes to come back.
        let down = self.issues_down.lock().unwrap().clone();
        let listed = self.issues.lock().unwrap().clone();
        self.wait_if_held(held::At::Listing);
        match down {
            Some(why) => Err(ForgeError::Failed(why)),
            None => Ok(listed),
        }
    }

    fn issue_comments(
        &self,
        _repo: &ForgeSlug,
        id: TicketId,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        let number = number(id);
        self.ask()?;
        self.wait_if_held(held::At::IssueComments);
        if self.threads.is_unlisted(number) {
            return Err(ForgeError::Failed(format!(
                "the comments on #{number} cannot be listed right now"
            )));
        }
        let mut on = self.threads.on(number);
        // An App's own comment is on the issue like anyone's: the forge lists it.
        if let Some(github) = self.app_comments.lock().unwrap().clone() {
            on.extend(github.posted_on(number));
            on.sort_by_key(|c| c.id);
        }
        Ok(on)
    }

    fn fetch_image(&self, url: &str, stem: &Path, most: u64) -> Result<PathBuf, ForgeError> {
        self.ask()?;
        self.images.fetch(url, stem, most)
    }

    // Numbers start at 900, clear of every issue a test opens by hand.
    fn create_issue(
        &self,
        _repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError> {
        self.ask()?;
        self.issues_up()?;
        if let Some(left) = self.creates_left.lock().unwrap().as_mut() {
            if *left == 0 {
                return Err(ForgeError::Failed("issues are down".into()));
            }
            *left -= 1;
        }
        self.wrote(format!("issue opened: {title}"));
        let mut created = self.created.lock().unwrap();
        let number = 900 + created.len() as u64;
        created.push(CreatedIssue {
            number,
            title: title.to_owned(),
            body: body.to_owned(),
            labels: labels.iter().map(|&l| l.to_owned()).collect(),
        });
        self.list_created(number, title, body, labels);
        drop(created);
        if labels.contains(&READY) {
            self.list_ready(number, false);
        }
        for label in labels.iter().filter(|&&l| l != READY) {
            self.label(number, label);
        }
        Ok(ticket(number))
    }

    fn close_issue(&self, _repo: &ForgeSlug, id: TicketId) -> Result<(), ForgeError> {
        let number = number(id);
        self.ask()?;
        self.issues_up()?;
        if self.closes_down.load(Ordering::SeqCst) {
            self.closes_refused.fetch_add(1, Ordering::SeqCst);
            return Err(ForgeError::Failed("closing is refused".into()));
        }
        self.wrote(format!("issue closed: #{number}"));
        self.closed.lock().unwrap().insert(number);
        self.closings.lock().unwrap().push(number);
        Ok(())
    }

    fn close_issue_as(
        &self,
        _repo: &ForgeSlug,
        id: TicketId,
        reason: IssueCloseReason,
    ) -> Result<(), ForgeError> {
        let number = number(id);
        self.ask()?;
        self.issues_up()?;
        if self.closes_down.load(Ordering::SeqCst) {
            self.closes_refused.fetch_add(1, Ordering::SeqCst);
            return Err(ForgeError::Failed("closing is refused".into()));
        }
        self.wrote(format!("issue closed: #{number}"));
        self.closed.lock().unwrap().insert(number);
        self.closings.lock().unwrap().push(number);
        self.closed_as.lock().unwrap().insert(number, reason);
        Ok(())
    }

    fn add_blocked_by(
        &self,
        _repo: &ForgeSlug,
        id: TicketId,
        blocker: TicketId,
    ) -> Result<bool, ForgeError> {
        self.link(number(id), number(blocker))
    }

    fn set_issue_label(
        &self,
        _repo: &ForgeSlug,
        id: TicketId,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        let number = number(id);
        self.ask()?;
        if self.labels_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("labels are down".into()));
        }
        self.wrote(format!("label {label} on #{number}: {on}"));
        let mut labels = self.labels.lock().unwrap();
        let on_issue = labels.entry(number).or_default();
        on_issue.retain(|l| l != label);
        if on {
            on_issue.push(label.to_owned());
        }
        Ok(())
    }
}
