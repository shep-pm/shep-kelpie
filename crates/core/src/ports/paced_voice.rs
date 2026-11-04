//! Kelpie's App held and spaced with the login's forge calls
//!
//! The App has its own limits, but its calls and the maintainer's `gh`
//! calls come from one machine and share the hold: a rate-limited App
//! answer stops the forge calls too, until the time its own headers give.

use std::fmt;
use std::sync::Arc;

use super::rate_limit::{FALLBACK, time_of};
use super::{
    CheckRunId, Clock, ForgeError, ForgeHold, GateCheck, InstallationToken, LineComment, NewLabel,
    PostedComment, Say, Voice,
};
use crate::settings::ForgeSlug;

/// A voice whose calls wait their turn in the login's pace and fail at once
/// with [`ForgeError::Held`] while the forge is held
///
/// [`Voice::covers`] is not a call and is not held.
pub struct PacedVoice {
    voice: Arc<dyn Voice>,
    clock: Arc<dyn Clock>,
    hold: ForgeHold,
    project: String,
    say: Say,
}

impl PacedVoice {
    /// `voice`, spaced and held by `hold` as `project`'s calls by `clock`'s
    /// time, and telling `say` as each hold begins
    pub fn new(
        voice: Arc<dyn Voice>,
        clock: Arc<dyn Clock>,
        hold: ForgeHold,
        project: &str,
        say: Say,
    ) -> Self {
        Self {
            voice,
            clock,
            hold,
            project: project.to_owned(),
            say,
        }
    }

    fn call<T>(
        &self,
        call: impl FnOnce(&dyn Voice) -> Result<T, ForgeError>,
    ) -> Result<T, ForgeError> {
        if let Err(until) = self.hold.admit(self.clock.as_ref(), &self.project) {
            return Err(ForgeError::Held(until));
        }
        let result = call(self.voice.as_ref());
        if let Err(error @ ForgeError::Limited { cooldown, .. }) = &result {
            let now = self.clock.now();
            let until =
                (cooldown.until(now)).unwrap_or(super::Timestamp(now.0.saturating_add(FALLBACK)));
            self.hold.hold(until);
            (self.say)(format!(
                "the GitHub App's rate limit is used up ({error}), so kelpie makes no forge \
                 call until {}",
                time_of(until)
            ));
        }
        result
    }
}

impl fmt::Debug for PacedVoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PacedVoice")
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl Voice for PacedVoice {
    fn covers(&self, repo: &ForgeSlug) -> bool {
        self.voice.covers(repo)
    }

    fn worker_token(&self, repo: &ForgeSlug) -> Result<Option<InstallationToken>, ForgeError> {
        // A repo no App covers needs no call, so a hold has nothing to stop.
        if self.hold.until(self.clock.now()).is_some() && !self.voice.covers(repo) {
            return Ok(None);
        }
        self.call(|v| v.worker_token(repo))
    }

    fn comment(&self, repo: &ForgeSlug, thread: u64, body: &str) -> Result<(), ForgeError> {
        self.call(|v| v.comment(repo, thread, body))
    }

    fn post(&self, repo: &ForgeSlug, thread: u64, body: &str) -> Result<PostedComment, ForgeError> {
        self.call(|v| v.post(repo, thread, body))
    }

    fn review(
        &self,
        repo: &ForgeSlug,
        number: u64,
        head: &str,
        body: &str,
        comments: &[LineComment],
    ) -> Result<u64, ForgeError> {
        self.call(|v| v.review(repo, number, head, body, comments))
    }

    fn edit_comment(&self, repo: &ForgeSlug, comment: u64, body: &str) -> Result<(), ForgeError> {
        self.call(|v| v.edit_comment(repo, comment, body))
    }

    fn thumbs_up(&self, repo: &ForgeSlug, comment: u64) -> Result<(), ForgeError> {
        self.call(|v| v.thumbs_up(repo, comment))
    }

    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u64, ForgeError> {
        self.call(|v| v.create_issue(repo, title, body, labels))
    }

    fn edit_issue_body(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError> {
        self.call(|v| v.edit_issue_body(repo, number, body))
    }

    fn resolve_thread(&self, repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        self.call(|v| v.resolve_thread(repo, thread))
    }

    fn reply_to_thread(
        &self,
        repo: &ForgeSlug,
        thread: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.call(|v| v.reply_to_thread(repo, thread, body))
    }

    fn create_label(&self, repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError> {
        self.call(|v| v.create_label(repo, label))
    }

    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.call(|v| v.set_issue_label(repo, number, label, on))
    }

    fn check_run(
        &self,
        repo: &ForgeSlug,
        head: &str,
        check: &GateCheck,
    ) -> Result<CheckRunId, ForgeError> {
        self.call(|v| v.check_run(repo, head, check))
    }

    fn update_check_run(
        &self,
        repo: &ForgeSlug,
        id: CheckRunId,
        check: &GateCheck,
    ) -> Result<(), ForgeError> {
        self.call(|v| v.update_check_run(repo, id, check))
    }
}
