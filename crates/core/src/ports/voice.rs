//! Kelpie's own voice on the forge, made as its GitHub App

use super::{
    CheckRunId, ForgeError, GateCheck, InstallationToken, LineComment, NewLabel, PostedComment,
};
use crate::settings::ForgeSlug;

/// The id [`Voice::review`] gives a review the forge took whose answer named none
pub const UNREAD_ID: u64 = 0;

/// Kelpie's own voice on the forge, as its GitHub App where one covers the repo
///
/// Its writes are the rulings, notices and review rounds posted as comments,
/// the replies and resolves on a review bot's threads, and the issues and
/// labels kelpie makes. A summon and the labels on a pull request stay on the
/// maintainer's login through [`Forge`](super::Forge), as a review bot answers
/// a person. Each write below is made only after [`Voice::covers`] says yes.
pub trait Voice: Send + Sync {
    /// Whether an App covers `repo`: its owner has one, installed on the repo
    fn covers(&self, repo: &ForgeSlug) -> bool;

    /// The token a worker's call on `repo` holds in place of the maintainer's
    /// login, fresh for each call and able to push its branch and nothing
    /// more, or `None` where no App covers `repo` and the worker runs as that login
    ///
    /// # Errors
    ///
    /// [`ForgeError::AppBusy`] when an App covers `repo` and GitHub cannot be
    /// asked just now: the worker waits and tries again.
    /// [`ForgeError::App`] when asking again will not help. Either way the
    /// worker never runs as the maintainer in the App's place.
    fn worker_token(&self, repo: &ForgeSlug) -> Result<Option<InstallationToken>, ForgeError>;

    /// Posts `body` as a comment on issue or pull request `thread`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the App cannot post it.
    fn comment(&self, repo: &ForgeSlug, thread: u64, body: &str) -> Result<(), ForgeError>;

    /// Posts `body` as a comment on issue or pull request `thread`, and
    /// returns the comment as the forge numbered and dated it
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the App cannot post it or its answer cannot be read.
    fn post(&self, repo: &ForgeSlug, thread: u64, body: &str) -> Result<PostedComment, ForgeError>;

    /// Posts a comment-only review of `number` at `head`: `body` and a comment on each line in `comments`
    ///
    /// Returns the review's id. The review never approves and never asks for
    /// changes. A line the diff does not show, or one the forge refuses, makes the
    /// forge refuse the comments, and the review is posted again with their text
    /// added to `body`.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the App cannot post it. A review the forge took
    /// whose answer names no id is `Ok(UNREAD_ID)`, since posting it again
    /// would be a second review.
    fn review(
        &self,
        repo: &ForgeSlug,
        number: u64,
        head: &str,
        body: &str,
        comments: &[LineComment],
    ) -> Result<u64, ForgeError>;

    /// Rewrites comment `comment`, one the App posted, to say `body`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the App cannot edit it.
    fn edit_comment(&self, repo: &ForgeSlug, comment: u64, body: &str) -> Result<(), ForgeError>;

    /// Reacts to comment `comment` with a thumbs up
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the App cannot react.
    fn thumbs_up(&self, repo: &ForgeSlug, comment: u64) -> Result<(), ForgeError>;

    /// Opens an issue on `repo` with these labels, and returns its number
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the App cannot open it, or its answer names no number.
    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u64, ForgeError>;

    /// Replaces issue `number`'s body with `body`, its title as it stands
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the App cannot edit it.
    fn edit_issue_body(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError>;

    /// Resolves review thread `thread`
    ///
    /// A thread already resolved, or one the forge no longer has, is done.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the App cannot resolve it.
    fn resolve_thread(&self, repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError>;

    /// Replies to review thread `thread` with `body`
    ///
    /// A thread the forge no longer has is done, since there is nothing to
    /// reply to.
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when the App cannot reply.
    fn reply_to_thread(&self, repo: &ForgeSlug, thread: &str, body: &str)
    -> Result<(), ForgeError>;

    /// Makes `label` on `repo`
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when GitHub refuses, such as when `repo` already has it.
    fn create_label(&self, repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError>;

    /// Adds `label` to issue `number`, or takes it off
    ///
    /// # Errors
    ///
    /// [`ForgeError`] when GitHub refuses or cannot be asked.
    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError>;

    /// Shows `check` as a new check run on commit `head`, and returns its id
    ///
    /// # Errors
    ///
    /// [`ForgeError::NoPermission`] when the App's installation does not hold
    /// the Checks permission. Another [`ForgeError`] when the App cannot post
    /// it or its answer names no id.
    fn check_run(
        &self,
        repo: &ForgeSlug,
        head: &str,
        check: &GateCheck,
    ) -> Result<CheckRunId, ForgeError>;

    /// Rewrites check run `id`, one the App made, to show `check`
    ///
    /// # Errors
    ///
    /// As [`Voice::check_run`], less the id.
    fn update_check_run(
        &self,
        repo: &ForgeSlug,
        id: CheckRunId,
        check: &GateCheck,
    ) -> Result<(), ForgeError>;
}
