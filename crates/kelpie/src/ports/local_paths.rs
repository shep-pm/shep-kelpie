//! Keeping this machine's paths off the forge
//!
//! Everything kelpie posts to the forge goes through [`Guarded`], which
//! refuses a body naming a local folder: the home folder, kelpie's home,
//! which holds every worktree, build and shots folder, and the project's
//! checkout. A post that names one is never sent.

use std::fmt;
use std::path::Path;

use super::{Forge, ForgeError, Issue, NewLabel, OpenIssue, PullRequest, Reviewed, Visibility};
use crate::board::{OpenPullRequest, ReadyIssue};
use crate::review_bot::{Activity, Login};
use crate::settings::ForgeSlug;

/// A forge that refuses to post any text naming a local folder
pub struct Guarded {
    forge: Box<dyn Forge>,
    local: Vec<String>,
}

impl Guarded {
    /// `forge`, refusing posts that name any of `folders`
    ///
    /// A folder with no parent, such as `/`, names nothing and is left out.
    pub fn new<'a>(forge: Box<dyn Forge>, folders: impl IntoIterator<Item = &'a Path>) -> Self {
        let local = folders
            .into_iter()
            .filter(|folder| folder.parent().is_some())
            .map(Path::to_string_lossy)
            .map(|folder| folder.trim_end_matches('/').to_owned())
            .filter(|folder| !folder.is_empty())
            .collect();
        Self { forge, local }
    }

    fn check(&self, body: &str) -> Result<(), ForgeError> {
        if self
            .local
            .iter()
            .any(|folder| body.contains(folder.as_str()))
        {
            return Err(ForgeError::LocalPath);
        }
        Ok(())
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
        self.check(body)?;
        self.forge.comment(repo, number, body)
    }

    fn post_comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<u64, ForgeError> {
        self.check(body)?;
        self.forge.post_comment(repo, number, body)
    }

    fn edit_comment(&self, repo: &ForgeSlug, id: u64, body: &str) -> Result<(), ForgeError> {
        self.check(body)?;
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
        self.check(title)?;
        self.check(body)?;
        self.forge.create_issue(repo, title, body, labels)
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
}
