//! The packages a ready issue needs that are not published yet
//!
//! The issue check reads no network, so the triage poll asks the registry, with
//! the runner's lock let go ([`super::poll_read`]), for each package a ready
//! issue's `Needs:` line names. This keeps what it read, for the board's
//! [`check_holds`](super::Runner::check_holds). A publish stands, so a package found
//! published is never asked of again in this run. One not found is asked again at
//! the next poll, and the issue goes on at the first poll that finds it. A package
//! no poll has read yet holds its issue too, so a board read between polls starts
//! no worker on it.

use std::collections::BTreeSet;

use super::super::Runner;
use crate::board::check::{self, Needed};
use crate::ports::RegistryError;

/// What one poll read of the registry: each package asked of, and the answer
pub(super) type Read = Vec<(Needed, Result<bool, RegistryError>)>;

/// The packages the runner has read, for its own run only. The next start asks again.
#[derive(Debug, Default)]
pub(in crate::runner) struct Looked {
    /// The packages found published, as the `Needs:` line writes each
    published: BTreeSet<String>,
    /// The packages the last poll could not read, so each is noted once
    unread: BTreeSet<String>,
}

impl Looked {
    /// The packages already found published, which a poll asks nothing of
    pub(super) fn published(&self) -> &BTreeSet<String> {
        &self.published
    }

    /// Keeps what a poll read, and returns what to note: each package that could
    /// not be read and could not be at the poll before either
    pub(super) fn keep(&mut self, read: Read) -> Vec<String> {
        let before = std::mem::take(&mut self.unread);
        let mut notes = Vec::new();
        for (needed, answer) in read {
            let package = needed.to_string();
            match answer {
                Ok(true) => {
                    self.published.insert(package);
                }
                // Neither found nor unread: the issue waits on it.
                Ok(false) => {}
                Err(why) => {
                    if !before.contains(&package) {
                        notes.push(format!(
                            "registry: cannot read whether {package} is published, so an \
                             issue that needs it is not held: {why}"
                        ));
                    }
                    self.unread.insert(package);
                }
            }
        }
        notes
    }

    /// The first package `body`'s `Needs:` line names that is not found published
    /// and that the last poll did not fail to read: one it found missing, or one no
    /// poll has asked of yet
    pub(super) fn first_waiting(&self, body: &str) -> Option<String> {
        (check::needs(body).iter())
            .map(ToString::to_string)
            .find(|package| !self.published.contains(package) && !self.unread.contains(package))
    }
}

impl Runner {
    // Keeps what the poll read of the registry, and notes a package it could not read
    pub(super) fn keep_registry(&mut self, read: Read) {
        let notes = self.triage_run.registry.keep(read);
        self.notes.extend(notes);
    }
}
