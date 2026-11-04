//! Recording each of kelpie's App's calls in the forge ledger

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use super::{
    CheckRunId, Clock, ForgeError, GateCheck, InstallationToken, LineComment, NewLabel,
    PostedComment, Voice,
};
use crate::forge_ledger::{Ended, Identity, Kind, Ledger};
use crate::settings::ForgeSlug;

/// A voice whose calls are each added to the forge ledger as they end
///
/// [`Voice::covers`] is not a call: it is answered from the App's cached
/// installations, and a refresh it makes is its token's mint, which the
/// App's token calls count.
pub struct RecordedVoice {
    voice: Arc<dyn Voice>,
    clock: Arc<dyn Clock>,
    ledger: Arc<Ledger>,
}

impl RecordedVoice {
    /// `voice`, adding each call to `ledger` at `clock`'s time
    pub fn new(voice: Arc<dyn Voice>, clock: Arc<dyn Clock>, ledger: Arc<Ledger>) -> Self {
        Self {
            voice,
            clock,
            ledger,
        }
    }

    fn call<T>(
        &self,
        kind: Kind,
        call: impl FnOnce(&dyn Voice) -> Result<T, ForgeError>,
    ) -> Result<T, ForgeError> {
        let started = Instant::now();
        let result = call(self.voice.as_ref());
        let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let ended = match &result {
            Ok(_) => Ended::Ok,
            Err(error) if rate_limited(error) => Ended::UsedUp,
            Err(_) => Ended::Failed,
        };
        (self.ledger).record_by(Identity::App, self.clock.now(), kind, millis, ended);
        result
    }
}

/// Whether the App's call failed on a rate limit, which its errors say in words
fn rate_limited(error: &ForgeError) -> bool {
    matches!(error, ForgeError::Limited { .. })
        || matches!(error, ForgeError::App(why) | ForgeError::AppBusy(why)
            if why.to_lowercase().contains("rate limit"))
}

impl fmt::Debug for RecordedVoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordedVoice").finish_non_exhaustive()
    }
}

impl Voice for RecordedVoice {
    fn covers(&self, repo: &ForgeSlug) -> bool {
        self.voice.covers(repo)
    }

    fn worker_token(&self, repo: &ForgeSlug) -> Result<Option<InstallationToken>, ForgeError> {
        self.call(Kind::AppWorkerToken, |v| v.worker_token(repo))
    }

    fn comment(&self, repo: &ForgeSlug, thread: u64, body: &str) -> Result<(), ForgeError> {
        self.call(Kind::AppComment, |v| v.comment(repo, thread, body))
    }

    fn post(&self, repo: &ForgeSlug, thread: u64, body: &str) -> Result<PostedComment, ForgeError> {
        self.call(Kind::AppComment, |v| v.post(repo, thread, body))
    }

    fn review(
        &self,
        repo: &ForgeSlug,
        number: u64,
        head: &str,
        body: &str,
        comments: &[LineComment],
    ) -> Result<u64, ForgeError> {
        self.call(Kind::AppReview, |v| {
            v.review(repo, number, head, body, comments)
        })
    }

    fn edit_comment(&self, repo: &ForgeSlug, comment: u64, body: &str) -> Result<(), ForgeError> {
        self.call(Kind::AppEditComment, |v| {
            v.edit_comment(repo, comment, body)
        })
    }

    fn thumbs_up(&self, repo: &ForgeSlug, comment: u64) -> Result<(), ForgeError> {
        self.call(Kind::AppThumbsUp, |v| v.thumbs_up(repo, comment))
    }

    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u64, ForgeError> {
        self.call(Kind::AppCreateIssue, |v| {
            v.create_issue(repo, title, body, labels)
        })
    }

    fn edit_issue_body(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError> {
        self.call(Kind::AppEditIssue, |v| {
            v.edit_issue_body(repo, number, body)
        })
    }

    fn resolve_thread(&self, repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        self.call(Kind::AppResolveThread, |v| v.resolve_thread(repo, thread))
    }

    fn reply_to_thread(
        &self,
        repo: &ForgeSlug,
        thread: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.call(Kind::AppReplyToThread, |v| {
            v.reply_to_thread(repo, thread, body)
        })
    }

    fn create_label(&self, repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError> {
        self.call(Kind::AppCreateLabel, |v| v.create_label(repo, label))
    }

    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.call(Kind::AppSetIssueLabel, |v| {
            v.set_issue_label(repo, number, label, on)
        })
    }

    fn check_run(
        &self,
        repo: &ForgeSlug,
        head: &str,
        check: &GateCheck,
    ) -> Result<CheckRunId, ForgeError> {
        self.call(Kind::AppCheckRun, |v| v.check_run(repo, head, check))
    }

    fn update_check_run(
        &self,
        repo: &ForgeSlug,
        id: CheckRunId,
        check: &GateCheck,
    ) -> Result<(), ForgeError> {
        self.call(Kind::AppUpdateCheckRun, |v| {
            v.update_check_run(repo, id, check)
        })
    }
}
