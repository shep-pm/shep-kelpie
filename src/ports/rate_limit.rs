//! Holding every forge call while the forge's rate limit is used up
//!
//! A runner's forge goes through [`RateHeld`]. A call the forge answers
//! with its rate limit used up holds every later call until the limit
//! resets, read once from the forge, or for [`FALLBACK`] when the forge
//! cannot say. A held call fails at once with [`ForgeError::Held`], and
//! the hold is told once, through [`ForgeHold::take_told`].

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use super::{
    Clock, Forge, ForgeError, Issue, NewLabel, OpenIssue, PullRequest, QueueStanding, Reviewed,
    Timestamp, Visibility,
};
use crate::board::{OpenPullRequest, ReadyIssue};
use crate::review_bot::{Activity, Login};
use crate::settings::ForgeSlug;

/// How long a hold lasts when the forge cannot say when its limit resets
const FALLBACK: u64 = 10 * 60;

/// What gh says when a limit is used up, lowercased: the REST and GraphQL
/// primary limits, the secondary limit, and an HTTP 429 answer
const USED_UP: [&str; 4] = [
    "api rate limit exceeded",
    "api rate limit already exceeded",
    "secondary rate limit",
    "http 429",
];

/// Whether `error` is the forge saying its rate limit is used up
fn used_up(error: &ForgeError) -> bool {
    let ForgeError::Failed(stderr) = error else {
        return false;
    };
    let stderr = stderr.to_lowercase();
    USED_UP.iter().any(|said| stderr.contains(said))
}

/// `at` as a UTC time, as `2026-10-10T05:31:54Z`
pub(super) fn time_of(at: Timestamp) -> String {
    i64::try_from(at.0)
        .ok()
        .and_then(|s| jiff::Timestamp::from_second(s).ok())
        .map_or_else(|| at.0.to_string(), |at| at.to_string())
}

/// Until when the forge is held, shared by [`RateHeld`] and its runner
#[derive(Debug, Clone, Default)]
pub struct ForgeHold(Arc<Mutex<Hold>>);

#[derive(Debug, Default)]
struct Hold {
    until: Option<Timestamp>,
    // The log line each hold makes, until the runner takes it
    told: Vec<String>,
}

impl ForgeHold {
    fn lock(&self) -> std::sync::MutexGuard<'_, Hold> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Until when every forge call is held, while that is after `now`
    pub fn until(&self, now: Timestamp) -> Option<Timestamp> {
        let mut hold = self.lock();
        if hold.until.is_some_and(|until| until <= now) {
            hold.until = None;
        }
        hold.until
    }

    /// The log line of each hold since the last take
    pub fn take_told(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().told)
    }

    fn hold(&self, until: Timestamp, error: &ForgeError) {
        let mut hold = self.lock();
        hold.until = Some(until);
        hold.told.push(format!(
            "the forge's rate limit is used up ({error}), so kelpie makes no forge call until {}",
            time_of(until)
        ));
    }
}

/// A forge whose calls are held while its rate limit is used up
pub struct RateHeld {
    forge: Box<dyn Forge>,
    clock: Arc<dyn Clock>,
    hold: ForgeHold,
}

impl RateHeld {
    /// `forge`, holding its calls in `hold` by `clock`'s time
    pub fn new(forge: Box<dyn Forge>, clock: Arc<dyn Clock>, hold: ForgeHold) -> Self {
        Self { forge, clock, hold }
    }

    fn call<T>(
        &self,
        call: impl FnOnce(&dyn Forge) -> Result<T, ForgeError>,
    ) -> Result<T, ForgeError> {
        let now = self.clock.now();
        if let Some(until) = self.hold.until(now) {
            return Err(ForgeError::Held(until));
        }
        let result = call(self.forge.as_ref());
        if let Err(error) = &result
            && used_up(error)
        {
            let until = match self.forge.rate_limit_reset() {
                Ok(Some(reset)) if reset > now => reset,
                _ => Timestamp(now.0.saturating_add(FALLBACK)),
            };
            self.hold.hold(until, error);
        }
        result
    }
}

impl fmt::Debug for RateHeld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RateHeld")
            .field("hold", &self.hold)
            .finish_non_exhaustive()
    }
}

impl Forge for RateHeld {
    fn visibility(&self, repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        self.call(|f| f.visibility(repo))
    }

    fn default_branch(&self, repo: &ForgeSlug) -> Result<String, ForgeError> {
        self.call(|f| f.default_branch(repo))
    }

    fn repo_labels(&self, repo: &ForgeSlug) -> Result<Vec<String>, ForgeError> {
        self.call(|f| f.repo_labels(repo))
    }

    fn create_label(&self, repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError> {
        self.call(|f| f.create_label(repo, label))
    }

    fn can_push(&self, repo: &ForgeSlug) -> Result<bool, ForgeError> {
        self.call(|f| f.can_push(repo))
    }

    fn review_bot_seen(&self, repo: &ForgeSlug, login: Login<'_>) -> Result<bool, ForgeError> {
        self.call(|f| f.review_bot_seen(repo, login))
    }

    fn issue(&self, repo: &ForgeSlug, number: u64) -> Result<Issue, ForgeError> {
        self.call(|f| f.issue(repo, number))
    }

    fn ready_issues(&self, repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        self.call(|f| f.ready_issues(repo))
    }

    fn open_pull_requests(&self, repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
        self.call(|f| f.open_pull_requests(repo))
    }

    fn pull_request(&self, repo: &ForgeSlug, number: u64) -> Result<PullRequest, ForgeError> {
        self.call(|f| f.pull_request(repo, number))
    }

    fn reviewed(&self, repo: &ForgeSlug, number: u64) -> Result<Reviewed, ForgeError> {
        self.call(|f| f.reviewed(repo, number))
    }

    fn viewer(&self) -> Result<String, ForgeError> {
        self.call(|f| f.viewer())
    }

    fn comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError> {
        self.call(|f| f.comment(repo, number, body))
    }

    fn post_comment(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<u64, ForgeError> {
        self.call(|f| f.post_comment(repo, number, body))
    }

    fn open_issues(&self, repo: &ForgeSlug) -> Result<Vec<OpenIssue>, ForgeError> {
        self.call(|f| f.open_issues(repo))
    }

    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u64, ForgeError> {
        self.call(|f| f.create_issue(repo, title, body, labels))
    }

    fn close_issue(&self, repo: &ForgeSlug, number: u64, comment: &str) -> Result<(), ForgeError> {
        self.call(|f| f.close_issue(repo, number, comment))
    }

    fn mark_ready(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.call(|f| f.mark_ready(repo, number))
    }

    fn set_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.call(|f| f.set_label(repo, number, label, on))
    }

    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        self.call(|f| f.set_issue_label(repo, number, label, on))
    }

    fn review_bot(
        &self,
        repo: &ForgeSlug,
        number: u64,
        login: Login<'_>,
    ) -> Result<Activity, ForgeError> {
        self.call(|f| f.review_bot(repo, number, login))
    }

    fn resolve_thread(&self, repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        self.call(|f| f.resolve_thread(repo, thread))
    }

    fn merge(&self, repo: &ForgeSlug, number: u64, head: &str) -> Result<(), ForgeError> {
        self.call(|f| f.merge(repo, number, head))
    }

    fn merge_queue(&self, repo: &ForgeSlug, number: u64) -> Result<QueueStanding, ForgeError> {
        self.call(|f| f.merge_queue(repo, number))
    }

    fn disable_auto_merge(&self, repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.call(|f| f.disable_auto_merge(repo, number))
    }

    // Free of the limits, and asked only once a call finds them used up.
    fn rate_limit_reset(&self) -> Result<Option<Timestamp>, ForgeError> {
        self.forge.rate_limit_reset()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The first is from the live runner's log; the rest are GitHub's
    // documented messages, as gh prints a failed call.
    #[test]
    fn each_of_ghs_used_up_answers_is_a_used_up_limit() {
        let said = [
            "GraphQL: API rate limit already exceeded for user ID 1.",
            "gh: API rate limit exceeded for user ID 1. (HTTP 403)",
            "gh: You have exceeded a secondary rate limit. Please wait a few minutes before you \
             try again. (HTTP 403)",
            "gh: Too Many Requests (HTTP 429)",
        ];
        for stderr in said {
            assert!(used_up(&ForgeError::Failed(stderr.into())), "{stderr}");
        }
    }

    #[test]
    fn any_other_failure_is_not() {
        let other = [
            ForgeError::Failed("gh: Not Found (HTTP 404)".into()),
            ForgeError::Failed("error connecting to api.github.com".into()),
            ForgeError::Unreadable("API rate limit exceeded".into()),
        ];
        for error in other {
            assert!(!used_up(&error), "{error}");
        }
    }
}
