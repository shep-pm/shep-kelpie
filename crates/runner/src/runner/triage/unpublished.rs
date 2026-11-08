//! The packages a ready issue needs that are not published yet
//!
//! The issue check reads no network, so the triage poll asks the registry, with
//! the runner's lock let go ([`super::poll_read`]), for each package a ready
//! issue's `Needs:` line names, and keeps what it read for the board's
//! [`check_holds`](super::Runner::check_holds). A package found published is never
//! asked of again in this run. One not found is asked again at each poll, and the
//! issue goes on at the first that finds it. A package nothing has read yet holds
//! its issue too. The board's own read asks the same of each ready issue it lists,
//! with the lock let go, since the poll's listing may leave one out.

use std::collections::BTreeSet;

use super::super::Runner;
use crate::board::check::{self, Needed};
use crate::ports::RegistryError;

/// What one poll read of the registry: each package asked of, and the answer
pub(in crate::runner) type Read = Vec<(Needed, Result<bool, RegistryError>)>;

/// The packages the runner has read, for its own run only. The next start asks again.
#[derive(Debug, Default)]
pub(in crate::runner) struct Looked {
    /// The packages found published, as the `Needs:` line writes each
    published: BTreeSet<String>,
    /// The packages a read found not published
    waiting: BTreeSet<String>,
    /// The packages the last read of each could not answer, so each is noted once
    unread: BTreeSet<String>,
}

impl Looked {
    /// The packages already found published, which a poll asks nothing of
    pub(in crate::runner) fn published(&self) -> &BTreeSet<String> {
        &self.published
    }

    /// Keeps what a read of the registry found, and returns what to note: each
    /// package that could not be read and could not be when it was last asked
    pub(super) fn keep(&mut self, read: Read) -> Vec<String> {
        let mut notes = Vec::new();
        for (needed, answer) in read {
            let package = needed.to_string();
            let failed = answer.is_err();
            let noted = if failed {
                !self.unread.insert(package.clone())
            } else {
                self.unread.remove(&package);
                false
            };
            match answer {
                Ok(true) => {
                    self.waiting.remove(&package);
                    self.published.insert(package);
                }
                Ok(false) => {
                    self.waiting.insert(package);
                }
                Err(why) if !noted => notes.push(format!(
                    "registry: cannot read whether {package} is published, so an \
                     issue that needs it is not held: {why}"
                )),
                Err(_) => {}
            }
        }
        notes
    }

    /// The first package `body`'s `Needs:` line names that is not found published
    /// and that the last read of it did not fail: one found missing, or one no read
    /// has asked of yet
    pub(super) fn first_waiting(&self, body: &str) -> Option<String> {
        (check::needs(body).iter())
            .map(ToString::to_string)
            .find(|package| !self.published.contains(package) && !self.unread.contains(package))
    }
}

impl Runner {
    // Keeps what the poll read of the registry, and notes a package it could not read
    pub(in crate::runner) fn keep_registry(&mut self, read: Read) {
        let notes = self.triage_run.registry.keep(read);
        self.notes.extend(notes);
    }
}
