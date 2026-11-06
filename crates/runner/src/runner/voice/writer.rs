//! What writes to the repo for the runner, as the App where one covers it, with no lock

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::board::make_missing_with;
use crate::ports::{Forge, ForgeError, NewLabel, Tickets, Voice};
use crate::runner::open_forge::stop_has_come;
use crate::settings::{ForgeSlug, TicketSource};
use crate::ticket::TicketId;

/// The ports a write to the repo goes through, and who writes
///
/// [`Runner::writer`](crate::runner::Runner) copies it under the lock, so a write that waits its
/// turn behind the login's pace can be made with the lock let go.
pub(in crate::runner) struct Writer {
    pub(super) forge: Arc<dyn Forge>,
    pub(super) tickets: Arc<dyn Tickets>,
    pub(super) voice: Arc<dyn Voice>,
    pub(super) remote: ForgeSlug,
    pub(super) source: TicketSource,
    /// Whether kelpie's App covers the repo
    pub(super) app: bool,
    // The stop of the loop these hands write in, copied from the runner they came from
    pub(super) loop_stop: Option<Arc<AtomicBool>>,
}

impl Writer {
    /// The forge `gh` writes through
    pub(in crate::runner) fn forge(&self) -> &dyn Forge {
        self.forge.as_ref()
    }

    /// The repo the writes go to
    pub(in crate::runner) fn remote(&self) -> &ForgeSlug {
        &self.remote
    }

    /// Makes each of `wanted` the repo lacks, as the App where one covers it
    pub(in crate::runner) fn make_missing_labels<'a>(
        &self,
        wanted: impl IntoIterator<Item = NewLabel<'a>>,
    ) -> Result<(), String> {
        let repo = &self.remote;
        make_missing_with(
            self.forge.as_ref(),
            repo,
            wanted,
            &|| stop_has_come(&self.loop_stop),
            |label| match self.app {
                true => self.voice.create_label(repo, label),
                false => self.forge.create_label(repo, label),
            },
        )
    }

    /// Opens an issue, as the App where one covers the repo
    pub(in crate::runner) fn open_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u64, ForgeError> {
        match self.app {
            true => self.voice.create_issue(&self.remote, title, body, labels),
            // `gh` files a GitHub issue, so its ticket has an issue number.
            false => (self.tickets)
                .create_issue(&self.remote, title, body, labels)
                .and_then(|ticket| ticket.github().ok_or(ForgeError::NoIssue { ticket })),
        }
    }

    /// Files a ticket in the project's own source, and returns its id
    ///
    /// A GitHub project's goes out as the App where one covers the repo. A local
    /// project's is a file in the store, which no App reaches.
    pub(in crate::runner) fn file_ticket(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<TicketId, ForgeError> {
        match (self.source, self.app) {
            (TicketSource::Github, true) => self
                .voice
                .create_issue(&self.remote, title, body, labels)
                .and_then(|number| {
                    TicketId::from_issue(number).ok_or_else(|| {
                        ForgeError::Unreadable("the forge numbered the issue zero".to_owned())
                    })
                }),
            _ => self.tickets.file_ticket(&self.remote, title, body, labels),
        }
    }
}
