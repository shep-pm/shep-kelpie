//! Keeping this machine's paths off the forge
//!
//! Everything kelpie posts to the forge goes through [`Guarded`], which
//! refuses a text [`LocalPaths`] finds something of this machine's in: its
//! folders, a path under `~`, an address on a local network, or a name the
//! project keeps private. A post that names one is never sent.

use std::fmt;

use super::{
    Forge, ForgeError, Issue, NewLabel, OpenIssue, PullRequest, QueueStanding, Reviewed, Visibility,
};
use crate::board::{OpenPullRequest, ReadyIssue};
use crate::local_paths::{LocalPaths, Surface};
use crate::review_bot::{Activity, Login};
use crate::settings::ForgeSlug;

/// A forge that refuses to post any text naming something of this machine's
pub struct Guarded {
    forge: Box<dyn Forge>,
    local: LocalPaths,
}

impl Guarded {
    /// `forge`, refusing posts that `local` finds something in
    pub fn new(forge: Box<dyn Forge>, local: LocalPaths) -> Self {
        Self { forge, local }
    }

    // `what` names the field, so a refusal says which one to fix.
    fn check(&self, what: &'static str, text: &str) -> Result<(), ForgeError> {
        match self.local.find(text, Surface::Prose) {
            Some(leak) => Err(ForgeError::LocalPath { what, leak }),
            None => Ok(()),
        }
    }
}

impl fmt::Debug for Guarded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Guarded").finish_non_exhaustive()
    }
}

impl Forge for Guarded {
    fn visibility(&self, repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        self.forge.visibility(repo)
    }

    fn default_branch(&self, repo: &ForgeSlug) -> Result<String, ForgeError> {
        self.forge.default_branch(repo)
    }

    fn repo_labels(&self, repo: &ForgeSlug) -> Result<Vec<String>, ForgeError> {
        self.forge.repo_labels(repo)
    }

    // A label's text is kelpie's own constants, which name no folder.
    fn create_label(&self, repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError> {
        self.forge.create_label(repo, label)
    }

    fn can_push(&self, repo: &ForgeSlug) -> Result<bool, ForgeError> {
        self.forge.can_push(repo)
    }

    fn review_bot_seen(&self, repo: &ForgeSlug, login: Login<'_>) -> Result<bool, ForgeError> {
        self.forge.review_bot_seen(repo, login)
    }

    fn issue(&self, repo: &ForgeSlug, number: u64) -> Result<Issue, ForgeError> {
        self.forge.issue(repo, number)
    }

    fn ready_issues(&self, repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        self.forge.ready_issues(repo)
    }

    fn open_pull_requests(&self, repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
        self.forge.open_pull_requests(repo)
    }

    fn pull_request(&self, repo: &ForgeSlug, number: u64) -> Result<PullRequest, ForgeError> {
        self.forge.pull_request(repo, number)
    }

    fn reviewed(&self, repo: &ForgeSlug, number: u64) -> Result<Reviewed, ForgeError> {
        self.forge.reviewed(repo, number)
    }

    fn viewer(&self) -> Result<String, ForgeError> {
        self.forge.viewer()
    }

    fn comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError> {
        self.check("the comment", body)?;
        self.forge.comment(repo, number, body)
    }

    fn post_comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<u64, ForgeError> {
        self.check("the comment", body)?;
        self.forge.post_comment(repo, number, body)
    }

    fn edit_comment(&self, repo: &ForgeSlug, id: u64, body: &str) -> Result<(), ForgeError> {
        self.check("the comment", body)?;
        self.forge.edit_comment(repo, id, body)
    }

    fn open_issues(&self, repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
        self.forge.open_issues(repo)
    }

    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u64, ForgeError> {
        self.check("the issue's title", title)?;
        self.check("the issue's body", body)?;
        self.forge.create_issue(repo, title, body, labels)
    }

    fn add_sub_issue(&self, repo: &ForgeSlug, parent: u64, child: u64) -> Result<(), ForgeError> {
        self.forge.add_sub_issue(repo, parent, child)
    }

    fn add_blocker(&self, repo: &ForgeSlug, number: u64, blocker: u64) -> Result<(), ForgeError> {
        self.forge.add_blocker(repo, number, blocker)
    }

    fn close_issue(&self, repo: &ForgeSlug, number: u64, comment: &str) -> Result<(), ForgeError> {
        self.check("the comment", comment)?;
        self.forge.close_issue(repo, number, comment)
    }

    fn mark_ready(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.forge.mark_ready(repo, number)
    }

    fn set_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.forge.set_label(repo, number, label, on)
    }

    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.forge.set_issue_label(repo, number, label, on)
    }

    fn review_bot(
        &self,
        repo: &ForgeSlug,
        number: u64,
        login: Login<'_>,
    ) -> Result<Activity, ForgeError> {
        self.forge.review_bot(repo, number, login)
    }

    fn resolve_thread(&self, repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        self.forge.resolve_thread(repo, thread)
    }

    fn merge(&self, repo: &ForgeSlug, number: u64, head: &str) -> Result<(), ForgeError> {
        self.forge.merge(repo, number, head)
    }

    fn merge_queue(&self, repo: &ForgeSlug, number: u64) -> Result<QueueStanding, ForgeError> {
        self.forge.merge_queue(repo, number)
    }
}

#[cfg(test)]
mod tests;
