//! The local ticket store: a project's tickets as markdown files in kelpie's home
//!
//! A project whose `tickets.source` is `local` keeps each ticket as
//! `<kelpie home>/<project>/tickets/k<N>.md` (ADR 0014). The file's format is in
//! [`file`]. A new ticket takes the next counter after the highest on disk, and
//! no ticket's file is ever removed, so a counter is not reused.
//!
//! [`LocalTickets`] is the project's [`Tickets`] port, and `shep kelpie ticket`
//! works the same files, so a ticket filed by hand is on the board at once.

use std::io::{self, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub use file::{Comment, FileError, Standing, Stored};
pub use routed::{Routed, SharedSource};

use crate::board::{Blocker, READY, ReadyIssue, SubIssues};
use crate::ports::{
    FinishedParent, ForgeError, Issue, IssueCloseReason, OpenIssue, OpenTicket, SubIssue,
    SubIssueState, ThreadComment, Tickets, Timestamp,
};
use crate::settings::ForgeSlug;
use crate::ticket::TicketId;

mod file;
mod routed;
pub mod verb;

/// Why a local ticket cannot be read or written
#[derive(Debug)]
pub enum LocalError {
    /// The ticket is not a local one, or its file is not there
    NoTicket(TicketId),
    /// The store's folder or a ticket's file could not be read or written
    Io {
        /// The file or folder
        path: PathBuf,
        /// What the OS said
        error: io::Error,
    },
    /// A ticket's file is not one, with its path and why
    Unreadable {
        /// The file
        path: PathBuf,
        /// What is wrong with it
        why: FileError,
    },
    /// The text to write is refused
    Refused(FileError),
    /// A parent or blocker the ticket names is not a ticket of the store
    Unknown(TicketId),
}

impl core::fmt::Display for LocalError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoTicket(ticket) => write!(f, "no local ticket {ticket}"),
            Self::Io { path, error } => write!(f, "{}: {error}", path.display()),
            Self::Unreadable { path, why } => write!(f, "{}: {why}", path.display()),
            Self::Refused(why) => why.fmt(f),
            Self::Unknown(ticket) => write!(f, "{ticket} is not a ticket of this project's store"),
        }
    }
}

impl core::error::Error for LocalError {}

impl From<LocalError> for ForgeError {
    fn from(e: LocalError) -> Self {
        match e {
            LocalError::NoTicket(ticket) => Self::NoIssue { ticket },
            LocalError::Unreadable { .. } => Self::Unreadable(e.to_string()),
            e => Self::Failed(e.to_string()),
        }
    }
}

/// What a new ticket says besides its text
#[derive(Debug, Clone, Default)]
pub struct Links {
    /// The ticket it is a sub-issue of
    pub parent: Option<TicketId>,
    /// The tickets that must close before it is taken
    pub blocked_by: Vec<TicketId>,
}

/// One project's local tickets
#[derive(Debug, Clone)]
pub struct LocalTickets {
    folder: PathBuf,
}

impl LocalTickets {
    /// The store in `folder`, which need not exist until a ticket is filed
    pub fn new(folder: PathBuf) -> Self {
        Self { folder }
    }

    /// The store's folder
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    fn path(&self, ticket: TicketId) -> Result<PathBuf, LocalError> {
        match ticket {
            TicketId::Local(_) => Ok(self.folder.join(format!("{ticket}.md"))),
            TicketId::Github(_) => Err(LocalError::NoTicket(ticket)),
        }
    }

    fn io(path: &Path) -> impl FnOnce(io::Error) -> LocalError {
        let path = path.to_owned();
        move |error| LocalError::Io { path, error }
    }

    /// Ticket `ticket`
    ///
    /// # Errors
    ///
    /// [`LocalError`] when it is not a local ticket, has no file, or its file is
    /// not a ticket's.
    pub fn get(&self, ticket: TicketId) -> Result<Stored, LocalError> {
        let path = self.path(ticket)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(LocalError::NoTicket(ticket));
            }
            Err(e) => return Err(Self::io(&path)(e)),
        };
        Stored::parse(&text).map_err(|why| LocalError::Unreadable { path, why })
    }

    /// Every ticket of the store, by id
    ///
    /// # Errors
    ///
    /// [`LocalError`] when the folder cannot be read or one of its tickets is not
    /// a ticket's file, so a mistyped file is seen at once.
    pub fn all(&self) -> Result<Vec<(TicketId, Stored)>, LocalError> {
        let mut tickets = Vec::new();
        for ticket in self.ids()? {
            tickets.push((ticket, self.get(ticket)?));
        }
        Ok(tickets)
    }

    // The ids of the files in the folder, ascending, none for a folder not made yet
    fn ids(&self) -> Result<Vec<TicketId>, LocalError> {
        let entries = match std::fs::read_dir(&self.folder) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Self::io(&self.folder)(e)),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(Self::io(&self.folder))?;
            let name = entry.file_name();
            let id = name
                .to_str()
                .and_then(|n| n.strip_suffix(".md"))
                .and_then(|stem| stem.parse::<TicketId>().ok())
                .filter(|id| matches!(id, TicketId::Local(_)));
            ids.extend(id);
        }
        ids.sort_unstable();
        Ok(ids)
    }

    /// Files a ticket and returns its id
    ///
    /// # Errors
    ///
    /// [`LocalError`] when the text is refused, a parent or blocker is not a
    /// ticket of the store, or the file cannot be written.
    pub fn create(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
        links: Links,
    ) -> Result<TicketId, LocalError> {
        let mut stored = Stored::new(title, body, labels);
        stored.parent = links.parent;
        stored.blocked_by = links.blocked_by;
        self.check_links(&stored)?;
        let text = stored.render().map_err(LocalError::Refused)?;
        std::fs::create_dir_all(&self.folder).map_err(Self::io(&self.folder))?;
        let ids = self.ids()?;
        let mut next = ids.last().map_or(1, |id| counter(*id) + 1);
        // The file appears whole or not at all, and a counter taken meanwhile is skipped.
        static FILED: AtomicU64 = AtomicU64::new(0);
        let scratch = self.folder.join(format!(
            ".new-{}-{}",
            std::process::id(),
            FILED.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::File::create(&scratch)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(Self::io(&scratch))?;
        let filed = loop {
            let ticket =
                TicketId::Local(NonZeroU64::new(next).expect("a counter after one is not zero"));
            let path = self.path(ticket)?;
            match std::fs::hard_link(&scratch, &path) {
                Ok(()) => break Ok(ticket),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => next += 1,
                Err(e) => break Err(Self::io(&path)(e)),
            }
        };
        let _ = std::fs::remove_file(&scratch);
        filed
    }

    /// Changes ticket `ticket` with `change`, and writes it back
    ///
    /// # Errors
    ///
    /// [`LocalError`] when it cannot be read, the changed ticket is refused or
    /// names a parent or blocker the store lacks, or its file cannot be written.
    pub fn update(
        &self,
        ticket: TicketId,
        change: impl FnOnce(&mut Stored),
    ) -> Result<Stored, LocalError> {
        let path = self.path(ticket)?;
        let _held = Lock::take(&path.with_extension("md.lock"))?;
        let mut stored = self.get(ticket)?;
        let before = (stored.parent, stored.blocked_by.clone());
        change(&mut stored);
        // A link already there stays, so a ticket whose blocker's file went can still close.
        let added = |linked: &&TicketId| before.0 != Some(**linked) && !before.1.contains(linked);
        for linked in stored.parent.iter().chain(&stored.blocked_by).filter(added) {
            self.check_linked(*linked)?;
        }
        let text = stored.render().map_err(LocalError::Refused)?;
        let scratch = path.with_extension("md.tmp");
        std::fs::write(&scratch, text).map_err(Self::io(&scratch))?;
        std::fs::rename(&scratch, &path).map_err(Self::io(&path))?;
        Ok(stored)
    }

    // A parent or blocker that is no ticket of the store would never close
    fn check_links(&self, stored: &Stored) -> Result<(), LocalError> {
        for linked in stored.parent.iter().chain(&stored.blocked_by) {
            self.check_linked(*linked)?;
        }
        Ok(())
    }

    fn check_linked(&self, linked: TicketId) -> Result<(), LocalError> {
        match self.path(linked) {
            Ok(path) if path.exists() => Ok(()),
            _ => Err(LocalError::Unknown(linked)),
        }
    }

    fn sub_issues_of(&self, parent: TicketId) -> Result<Vec<SubIssue>, LocalError> {
        Ok(self
            .all()?
            .into_iter()
            .filter(|(_, s)| s.parent == Some(parent))
            .map(|(ticket, s)| SubIssue {
                ticket,
                state: match s.standing {
                    Standing::Open => SubIssueState::Open,
                    Standing::Closed => SubIssueState::Completed,
                    Standing::NotPlanned | Standing::Duplicate => SubIssueState::NotPlanned,
                },
            })
            .collect())
    }
}

/// How long a lock may stand before it is taken for one a dead process left
const STALE_LOCK: Duration = Duration::from_secs(30);
const LOCK_WAIT: Duration = Duration::from_millis(20);
const LOCK_TRIES: u32 = 500;

/// The right to change one ticket's file, held while the file is read and written back
///
/// The runner and `shep kelpie ticket` both change tickets, so each change
/// takes the ticket's `.md.lock` file first, and a change made meanwhile is
/// never read stale and written over.
struct Lock(PathBuf);

impl Lock {
    fn take(path: &Path) -> Result<Self, LocalError> {
        for _ in 0..LOCK_TRIES {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => return Ok(Self(path.to_owned())),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if Self::stale(path) {
                        Self::break_stale(path);
                    } else {
                        std::thread::sleep(LOCK_WAIT);
                    }
                }
                Err(e) => return Err(Self::io(path, e)),
            }
        }
        let held = io::Error::new(io::ErrorKind::TimedOut, "another change holds the ticket");
        Err(Self::io(path, held))
    }

    fn stale(path: &Path) -> bool {
        let age = std::fs::metadata(path).and_then(|m| m.modified());
        age.ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age > STALE_LOCK)
    }

    // Looking at a lock's age and removing it are two steps, so a caller that saw it
    // stale may find another has since replaced it. One caller at a time breaks, and
    // looks again before it removes.
    fn break_stale(path: &Path) {
        let mut guard = path.as_os_str().to_owned();
        guard.push(".break");
        let guard = PathBuf::from(guard);
        if Self::stale(&guard) {
            let _ = std::fs::remove_file(&guard);
        }
        let made = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&guard);
        if made.is_ok() {
            if Self::stale(path) {
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::remove_file(&guard);
        }
    }

    fn io(path: &Path, error: io::Error) -> LocalError {
        LocalError::Io {
            path: path.to_owned(),
            error,
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn counter(ticket: TicketId) -> u64 {
    match ticket {
        TicketId::Local(n) | TicketId::Github(n) => n.get(),
    }
}

impl Tickets for LocalTickets {
    fn issue(&self, _repo: &ForgeSlug, ticket: TicketId) -> Result<Issue, ForgeError> {
        let stored = self.get(ticket)?;
        Ok(Issue {
            title: stored.title,
            body: stored.body,
            labels: stored.labels,
            open: stored.standing.is_open(),
            parent: stored.parent,
            blocked_by: stored.blocked_by,
        })
    }

    fn sub_issues(&self, _repo: &ForgeSlug, parent: TicketId) -> Result<Vec<SubIssue>, ForgeError> {
        Ok(self.sub_issues_of(parent)?)
    }

    fn finished_parents(&self, _repo: &ForgeSlug) -> Result<Vec<FinishedParent>, ForgeError> {
        let all = self.all()?;
        let mut parents = Vec::new();
        for (ticket, stored) in &all {
            let subs: Vec<_> = all
                .iter()
                .filter(|(_, s)| s.parent == Some(*ticket))
                .collect();
            if stored.standing.is_open()
                && !subs.is_empty()
                && subs.iter().all(|(_, s)| !s.standing.is_open())
            {
                parents.push(FinishedParent {
                    ticket: *ticket,
                    sub_issues: subs.len() as u64,
                });
            }
        }
        Ok(parents)
    }

    fn ready_issues(&self, _repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        let all = self.all()?;
        let standing = |ticket: TicketId| {
            all.iter()
                .find(|(t, _)| *t == ticket)
                .is_none_or(|(_, s)| s.standing.is_open())
        };
        let mut ready = Vec::new();
        for (ticket, stored) in &all {
            if !stored.standing.is_open() || !stored.labels.iter().any(|l| l == READY) {
                continue;
            }
            let subs: Vec<_> = all
                .iter()
                .filter(|(_, s)| s.parent == Some(*ticket))
                .collect();
            ready.push(ReadyIssue {
                ticket: *ticket,
                title: stored.title.clone(),
                body: stored.body.clone(),
                assigned: false,
                labels: stored.labels.clone(),
                blocked_by: stored
                    .blocked_by
                    .iter()
                    .map(|&b| Blocker {
                        ticket: b,
                        open: standing(b),
                    })
                    .collect(),
                unlisted_blockers: 0,
                parent: stored.parent,
                sub_issues: SubIssues {
                    total: subs.len() as u64,
                    closed: subs.iter().filter(|(_, s)| !s.standing.is_open()).count() as u64,
                },
            });
        }
        Ok(ready)
    }

    // Triage reads GitHub's issues, and a local project has no triage.
    fn open_issues(&self, _repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
        Ok(Vec::new())
    }

    fn open_tickets(&self, _repo: &ForgeSlug) -> Result<Vec<OpenTicket>, ForgeError> {
        Ok(self
            .all()?
            .into_iter()
            .filter(|(_, s)| s.standing.is_open())
            .map(|(ticket, s)| OpenTicket {
                ticket,
                title: s.title,
                body: s.body,
            })
            .collect())
    }

    fn comment_ticket(
        &self,
        _repo: &ForgeSlug,
        ticket: TicketId,
        body: &str,
    ) -> Result<(), ForgeError> {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        self.update(ticket, |s| {
            s.comments.push(Comment {
                author: "kelpie".to_owned(),
                at,
                body: body.to_owned(),
            });
        })?;
        Ok(())
    }

    fn issue_comments(
        &self,
        _repo: &ForgeSlug,
        ticket: TicketId,
    ) -> Result<Vec<ThreadComment>, ForgeError> {
        let stored = self.get(ticket)?;
        Ok(stored
            .comments
            .into_iter()
            .zip(1..)
            .map(|(c, id)| ThreadComment {
                id,
                thread: counter(ticket),
                author: c.author,
                bot: false,
                body: c.body,
                created: Timestamp(c.at),
            })
            .collect())
    }

    fn create_issue(
        &self,
        _repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError> {
        Ok(self.create(title, body, labels, Links::default())?)
    }

    fn close_issue(&self, _repo: &ForgeSlug, ticket: TicketId) -> Result<(), ForgeError> {
        self.update(ticket, |s| s.standing = Standing::Closed)?;
        Ok(())
    }

    fn close_issue_as(
        &self,
        _repo: &ForgeSlug,
        ticket: TicketId,
        reason: IssueCloseReason,
    ) -> Result<(), ForgeError> {
        self.update(ticket, |s| s.standing = Standing::after(reason))?;
        Ok(())
    }

    fn add_blocked_by(
        &self,
        _repo: &ForgeSlug,
        ticket: TicketId,
        blocker: TicketId,
    ) -> Result<bool, ForgeError> {
        self.update(ticket, |s| {
            if !s.blocked_by.contains(&blocker) {
                s.blocked_by.push(blocker);
            }
        })?;
        Ok(true)
    }

    fn set_issue_label(
        &self,
        _repo: &ForgeSlug,
        ticket: TicketId,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.update(ticket, |s| {
            s.labels.retain(|l| l != label);
            if on {
                s.labels.push(label.to_owned());
            }
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
