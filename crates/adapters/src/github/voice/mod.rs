//! Kelpie's writes on GitHub, made with its App's installation token
//!
//! [`AppVoice`] is the [`Voice`] port over [`GithubApp`]'s tokens and
//! [`GithubApi`]'s writes. A repo whose owner has no App, or whose App is not
//! installed on it, is not covered, and kelpie writes there as it always has.

use std::sync::Arc;

use super::api::{self, ApiError, Call, GithubApi, ThreadWrite};
use super::tokens::{GithubApp, TokenError};
use crate::ports::{
    CheckRunId, ForgeError, GateCheck, InstallationToken, LineComment, NewLabel, PostedComment,
    UNREAD_ID, Voice, with_comments_in_body,
};
use crate::settings::ForgeSlug;

/// [`Voice`] as kelpie's App
pub struct AppVoice {
    tokens: Arc<dyn GithubApp>,
    api: Box<dyn GithubApi>,
}

impl AppVoice {
    /// A voice that writes through `api` with the tokens `tokens` mints
    pub fn new(tokens: Arc<dyn GithubApp>, api: Box<dyn GithubApi>) -> Self {
        Self { tokens, api }
    }

    fn token(&self, repo: &ForgeSlug) -> Result<InstallationToken, ForgeError> {
        self.tokens.token(repo).map_err(token_error)
    }

    fn write(&self, repo: &ForgeSlug, call: &Call) -> Result<String, ForgeError> {
        let token = self.token(repo)?;
        self.send(&token, call)
    }

    fn send(&self, token: &InstallationToken, call: &Call) -> Result<String, ForgeError> {
        self.api.write(token, call).map_err(api_error)
    }

    fn check_write(&self, repo: &ForgeSlug, call: &Call) -> Result<String, ForgeError> {
        let token = (self.tokens.checks_token(repo))
            .map(|issued| issued.token)
            .map_err(checks_error)?;
        match self.api.write(&token, call) {
            Err(ApiError::Refused(403)) => Err(ForgeError::NoPermission(
                "GitHub answered HTTP 403 to the check run".to_owned(),
            )),
            other => other.map_err(api_error),
        }
    }

    // GraphQL answers a refusal with HTTP 200, so the body says how it went.
    fn thread_write(
        &self,
        token: &InstallationToken,
        call: &Call,
        mutation: &'static str,
    ) -> Result<(), ForgeError> {
        let answer = self.send(token, call)?;
        match api::parse_thread_write(&answer, mutation) {
            Ok(ThreadWrite::Done | ThreadWrite::Gone) => Ok(()),
            Ok(ThreadWrite::Refused(why)) => Err(ForgeError::App(format!(
                "GitHub refused the {mutation}: {why}"
            ))),
            Err(e) => Err(ForgeError::App(e.to_string())),
        }
    }
}

// A check run is written with its own token, which asks for the Checks
// permission an installation may not have accepted: GitHub then answers
// 422 to the mint, or 403 to the write.
fn checks_error(e: TokenError) -> ForgeError {
    match e {
        TokenError::Api(ApiError::Refused(422)) => ForgeError::NoPermission(
            "the installation has not accepted Checks (read and write)".to_owned(),
        ),
        e => token_error(e),
    }
}

// A rate limit keeps what its answer said of when to ask again, so the hold
// waits for it. Any other failure is told in words.
fn api_error(e: ApiError) -> ForgeError {
    match e {
        ApiError::RateLimited(cooldown) => ForgeError::Limited {
            said: e.to_string(),
            cooldown,
        },
        e => ForgeError::App(e.to_string()),
    }
}

fn token_error(e: TokenError) -> ForgeError {
    match e {
        TokenError::Api(e) => api_error(e),
        e => ForgeError::App(e.to_string()),
    }
}

impl std::fmt::Debug for AppVoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppVoice").finish_non_exhaustive()
    }
}

impl Voice for AppVoice {
    // An App that is set but cannot mint a token now, such as with GitHub
    // out of reach, still covers the repo: its writes fail and are logged,
    // and nothing is posted as the maintainer in its place.
    fn covers(&self, repo: &ForgeSlug) -> bool {
        !matches!(
            self.tokens.token(repo),
            Err(TokenError::Owner(_) | TokenError::NoApp(_) | TokenError::NotInstalled { .. })
        )
    }

    fn worker_token(&self, repo: &ForgeSlug) -> Result<Option<InstallationToken>, ForgeError> {
        match self.tokens.worker_token(repo) {
            Ok(token) => Ok(Some(token)),
            Err(TokenError::Owner(_) | TokenError::NoApp(_) | TokenError::NotInstalled { .. }) => {
                Ok(None)
            }
            // GitHub out of reach, busy or rate limited: asking again later may work.
            Err(TokenError::Api(e)) if e.passes() && !matches!(e, ApiError::RateLimited(_)) => {
                Err(ForgeError::AppBusy(e.to_string()))
            }
            Err(e) => Err(token_error(e)),
        }
    }

    fn comment(&self, repo: &ForgeSlug, thread: u64, body: &str) -> Result<(), ForgeError> {
        self.write(repo, &Call::comment(repo, thread, body))
            .map(drop)
    }

    fn post(&self, repo: &ForgeSlug, thread: u64, body: &str) -> Result<PostedComment, ForgeError> {
        let answer = self.write(repo, &Call::comment(repo, thread, body))?;
        let (id, created) =
            api::parse_posted(&answer).map_err(|e| ForgeError::App(e.to_string()))?;
        Ok(PostedComment { id, created })
    }

    fn review(
        &self,
        repo: &ForgeSlug,
        number: u64,
        head: &str,
        body: &str,
        comments: &[LineComment],
    ) -> Result<u64, ForgeError> {
        let token = self.token(repo)?;
        let call = Call::review(repo, number, head, body, comments);
        let answer = match self.api.write(&token, &call) {
            // GitHub answers 422 to a comment on a line the diff does not
            // show, so the findings go once more in the body of a review
            // that holds no comment.
            Err(ApiError::Refused(422)) if !comments.is_empty() => {
                let body = with_comments_in_body(body, comments);
                let call = Call::review(repo, number, head, &body, &[]);
                self.send(&token, &call)?
            }
            other => other.map_err(api_error)?,
        };
        Ok(api::parse_review(&answer).unwrap_or(UNREAD_ID))
    }

    fn edit_comment(&self, repo: &ForgeSlug, comment: u64, body: &str) -> Result<(), ForgeError> {
        self.write(repo, &Call::edit_comment(repo, comment, body))
            .map(drop)
    }

    fn thumbs_up(&self, repo: &ForgeSlug, comment: u64) -> Result<(), ForgeError> {
        self.write(repo, &Call::thumbs_up(repo, comment)).map(drop)
    }

    fn create_issue(
        &self,
        repo: &ForgeSlug,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<u64, ForgeError> {
        let answer = self.write(repo, &Call::issue(repo, title, body, labels))?;
        api::parse_number(&answer).map_err(|e| ForgeError::App(e.to_string()))
    }

    fn edit_issue_body(&self, repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError> {
        self.write(repo, &Call::edit_issue(repo, number, body))
            .map(drop)
    }

    fn resolve_thread(&self, repo: &ForgeSlug, thread: &str) -> Result<(), ForgeError> {
        let token = (self.tokens.thread_token(repo)).map_err(token_error)?;
        let call = Call::resolve_thread(thread);
        self.thread_write(&token, &call, "resolveReviewThread")
    }

    fn reply_to_thread(
        &self,
        repo: &ForgeSlug,
        thread: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        let call = Call::reply_to_thread(thread, body);
        let token = self.token(repo)?;
        self.thread_write(&token, &call, "addPullRequestReviewThreadReply")
    }

    fn check_run(
        &self,
        repo: &ForgeSlug,
        head: &str,
        check: &GateCheck,
    ) -> Result<CheckRunId, ForgeError> {
        let answer = self.check_write(repo, &Call::check_run(repo, head, check))?;
        api::parse_check_run(&answer).map_err(|e| ForgeError::App(e.to_string()))
    }

    fn update_check_run(
        &self,
        repo: &ForgeSlug,
        id: CheckRunId,
        check: &GateCheck,
    ) -> Result<(), ForgeError> {
        self.check_write(repo, &Call::update_check_run(repo, id, check))
            .map(drop)
    }

    fn create_label(&self, repo: &ForgeSlug, label: &NewLabel) -> Result<(), ForgeError> {
        self.write(repo, &Call::label(repo, label)).map(drop)
    }

    fn set_issue_label(
        &self,
        repo: &ForgeSlug,
        number: u64,
        label: &str,
        on: bool,
    ) -> Result<(), ForgeError> {
        if on {
            return self
                .write(repo, &Call::put_label(repo, number, label))
                .map(drop);
        }
        let call = Call::take_label(repo, number, label);
        let token = self.token(repo)?;
        match self.api.write(&token, &call) {
            // A label the issue does not carry is as good as taken off.
            Ok(_) | Err(ApiError::Refused(404)) => Ok(()),
            Err(e) => Err(ForgeError::App(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests;
