//! The GitHub calls an App makes, and the signer its tokens are asked with
//!
//! [`GithubApi`] is the port: the real one runs `curl`, and tests a stand-in
//! GitHub. The answers' JSON is read here, from the shapes GitHub's REST
//! documentation gives for each call.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde_json::Value;

use super::tokens::Jwt;
use crate::ports::{
    CheckRunId, Cooldown, GateCheck, InstallationToken, LineComment, NewLabel, Timestamp,
};
use crate::settings::ForgeSlug;

/// An App's private key in PEM, as the conversion hands it back
///
/// `Debug` does not show it.
#[derive(Clone, PartialEq, Eq)]
pub struct Pem(String);

impl Pem {
    /// The key `text`
    pub fn new(text: String) -> Self {
        Self(text)
    }

    /// The key's text, for the one file kelpie keeps it in
    #[inline]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Pem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Pem(..)")
    }
}

/// What GitHub hands back for a manifest's code: the App it registered
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
    /// The App's id
    pub id: u64,
    /// The App's slug
    pub slug: String,
    /// The App's client id
    pub client_id: String,
    /// The login of the account that owns the App
    pub owner: String,
    /// The App's private key
    pub pem: Pem,
}

/// An App's installation on a repo, as GitHub describes it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Installation {
    /// The installation's id
    pub id: u64,
    /// Whether the installation holds `workflows` at `write`: the App was
    /// given it and the installation accepted it
    pub workflows: bool,
}

/// An installation token and when GitHub lets it lapse
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedToken {
    /// The token
    pub token: InstallationToken,
    /// When it stops working
    pub expires_at: Timestamp,
    /// What GitHub says it holds, such as `contents` to `write`
    pub permissions: BTreeMap<String, String>,
}

/// Why a call to GitHub came to nothing
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// GitHub could not be reached, with why
    Unreachable(String),
    /// GitHub answered with this HTTP status, not the one the call wants
    Refused(u16),
    /// GitHub answered 429, or 403 saying a rate limit was passed, with when
    /// its headers say to ask again
    RateLimited(Cooldown),
    /// GitHub answered 301: the repo was renamed or moved, and kelpie never
    /// follows a redirect with a JWT
    Moved,
    /// GitHub's answer lacked the field named
    Unreadable(&'static str),
}

impl ApiError {
    /// Whether asking again later may succeed with nothing changed
    pub fn passes(&self) -> bool {
        match self {
            Self::Unreachable(_) | Self::RateLimited(_) => true,
            Self::Refused(status) => *status >= 500,
            Self::Moved | Self::Unreadable(_) => false,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(why) => write!(f, "cannot reach GitHub: {why}"),
            Self::Refused(status) => write!(f, "GitHub answered HTTP {status}"),
            Self::RateLimited(_) => f.write_str("GitHub's rate limit for the App is spent for now"),
            Self::Moved => f.write_str(
                "GitHub says the repo moved: put its new `owner/name` in the project's `git.remote`",
            ),
            Self::Unreadable(field) => write!(f, "GitHub's answer has no readable `{field}`"),
        }
    }
}

impl core::error::Error for ApiError {}

/// The GitHub calls an App makes
pub trait GithubApi: Send + Sync {
    /// Converts a manifest's `code` into the App it registered,
    /// `POST /app-manifests/{code}/conversions`, which needs no login
    ///
    /// # Errors
    ///
    /// [`ApiError`] when GitHub cannot be reached or refuses the code.
    fn convert(&self, code: &str) -> Result<Conversion, ApiError>;

    /// The App's installation on `repo`, `GET /repos/{owner}/{repo}/installation`,
    /// or `None` when GitHub says the App is not installed there
    ///
    /// # Errors
    ///
    /// [`ApiError`] when GitHub cannot be reached or refuses the App's `jwt`.
    fn installation(&self, jwt: &Jwt, repo: &ForgeSlug) -> Result<Option<Installation>, ApiError>;

    /// A new token for `installation` that reaches `repo` alone and holds
    /// the permissions `grant` names,
    /// `POST /app/installations/{id}/access_tokens` with
    /// `{"repositories": [<name>], "permissions": {..}}`
    ///
    /// # Errors
    ///
    /// [`ApiError`] when GitHub cannot be reached or refuses the App's `jwt`.
    fn access_token(
        &self,
        jwt: &Jwt,
        installation: u64,
        repo: &ForgeSlug,
        grant: Grant,
    ) -> Result<IssuedToken, ApiError>;

    /// Makes `call` as the App whose installation `token` is, and returns
    /// the answer's body
    ///
    /// # Errors
    ///
    /// [`ApiError`] when GitHub cannot be reached or does not take the call.
    fn write(&self, token: &InstallationToken, call: &Call) -> Result<String, ApiError>;
}

/// The GraphQL mutation that resolves a review thread
pub const RESOLVE_THREAD: &str = "mutation($id: ID!) { resolveReviewThread(input: {threadId: $id}) \
    { thread { isResolved } } }";

/// The GraphQL mutation that replies on a review thread
pub const REPLY_TO_THREAD: &str = "mutation($id: ID!, $body: String!) { \
    addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $id, body: $body}) \
    { comment { id } } }";

/// How a write is sent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// `POST`
    Post,
    /// `PATCH`
    Patch,
    /// `DELETE`
    Delete,
}

/// One write kelpie makes as its App, in the shape GitHub's REST
/// documentation gives for it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// How it is sent
    pub verb: Verb,
    /// Its path under the API's root, such as `/repos/o/n/issues/4/comments`
    pub path: String,
    /// Its JSON body, when it has one
    pub body: Option<String>,
}

impl Call {
    /// A comment on issue or pull request `thread`,
    /// `POST /repos/{owner}/{repo}/issues/{thread}/comments`
    pub fn comment(repo: &ForgeSlug, thread: u64, body: &str) -> Self {
        Self::post(
            format!("/repos/{}/issues/{thread}/comments", repo.as_str()),
            serde_json::json!({ "body": body }),
        )
    }

    /// A review of pull request `number` at `head` that only comments, with
    /// a comment on each line of `comments`,
    /// `POST /repos/{owner}/{repo}/pulls/{number}/reviews`
    pub fn review(
        repo: &ForgeSlug,
        number: u64,
        head: &str,
        body: &str,
        comments: &[LineComment],
    ) -> Self {
        let comments: Vec<Value> = (comments.iter())
            .map(|c| {
                serde_json::json!({
                    "path": c.path,
                    "line": c.line,
                    "side": "RIGHT",
                    "body": c.body,
                })
            })
            .collect();
        Self::post(
            format!("/repos/{}/pulls/{number}/reviews", repo.as_str()),
            serde_json::json!({
                "commit_id": head,
                "body": body,
                "event": "COMMENT",
                "comments": comments,
            }),
        )
    }

    /// Comment `comment` rewritten to say `body`,
    /// `PATCH /repos/{owner}/{repo}/issues/comments/{comment}`
    pub fn edit_comment(repo: &ForgeSlug, comment: u64, body: &str) -> Self {
        Self {
            verb: Verb::Patch,
            path: format!("/repos/{}/issues/comments/{comment}", repo.as_str()),
            body: Some(serde_json::json!({ "body": body }).to_string()),
        }
    }

    /// A thumbs up on comment `comment`,
    /// `POST /repos/{owner}/{repo}/issues/comments/{comment}/reactions`
    pub fn thumbs_up(repo: &ForgeSlug, comment: u64) -> Self {
        Self::post(
            format!(
                "/repos/{}/issues/comments/{comment}/reactions",
                repo.as_str()
            ),
            serde_json::json!({ "content": "+1" }),
        )
    }

    /// A new issue, `POST /repos/{owner}/{repo}/issues`
    pub fn issue(repo: &ForgeSlug, title: &str, body: &str, labels: &[&str]) -> Self {
        Self::post(
            format!("/repos/{}/issues", repo.as_str()),
            serde_json::json!({ "title": title, "body": body, "labels": labels }),
        )
    }

    /// Issue `number`'s body replaced with `body`, its title as it stands,
    /// `PATCH /repos/{owner}/{repo}/issues/{number}`
    pub fn edit_issue(repo: &ForgeSlug, number: u64, body: &str) -> Self {
        Self {
            verb: Verb::Patch,
            path: format!("/repos/{}/issues/{number}", repo.as_str()),
            body: Some(serde_json::json!({ "body": body }).to_string()),
        }
    }

    /// A new label on the repo, `POST /repos/{owner}/{repo}/labels`
    pub fn label(repo: &ForgeSlug, label: &NewLabel<'_>) -> Self {
        Self::post(
            format!("/repos/{}/labels", repo.as_str()),
            serde_json::json!({
                "name": label.name,
                "color": label.color,
                "description": label.description,
            }),
        )
    }

    /// `label` put on issue `number`,
    /// `POST /repos/{owner}/{repo}/issues/{number}/labels`
    pub fn put_label(repo: &ForgeSlug, number: u64, label: &str) -> Self {
        Self::post(
            format!("/repos/{}/issues/{number}/labels", repo.as_str()),
            serde_json::json!({ "labels": [label] }),
        )
    }

    /// `label` taken off issue `number`,
    /// `DELETE /repos/{owner}/{repo}/issues/{number}/labels/{name}`
    pub fn take_label(repo: &ForgeSlug, number: u64, label: &str) -> Self {
        Self {
            verb: Verb::Delete,
            path: format!(
                "/repos/{}/issues/{number}/labels/{}",
                repo.as_str(),
                path_segment(label)
            ),
            body: None,
        }
    }

    /// A check run on commit `head`, `POST /repos/{owner}/{repo}/check-runs`
    pub fn check_run(repo: &ForgeSlug, head: &str, check: &GateCheck) -> Self {
        let mut body = check_body(check);
        body["name"] = Value::from(check.name.as_str());
        body["head_sha"] = Value::from(head);
        Self::post(format!("/repos/{}/check-runs", repo.as_str()), body)
    }

    /// Check run `id` rewritten to show `check`,
    /// `PATCH /repos/{owner}/{repo}/check-runs/{id}`
    pub fn update_check_run(repo: &ForgeSlug, id: CheckRunId, check: &GateCheck) -> Self {
        Self {
            verb: Verb::Patch,
            path: format!("/repos/{}/check-runs/{}", repo.as_str(), id.0),
            body: Some(check_body(check).to_string()),
        }
    }

    /// Review thread `thread` resolved, `POST /graphql` with
    /// [`RESOLVE_THREAD`]
    pub fn resolve_thread(thread: &str) -> Self {
        Self::graphql(RESOLVE_THREAD, serde_json::json!({ "id": thread }))
    }

    /// A reply on review thread `thread`, `POST /graphql` with
    /// [`REPLY_TO_THREAD`]
    pub fn reply_to_thread(thread: &str, body: &str) -> Self {
        Self::graphql(
            REPLY_TO_THREAD,
            serde_json::json!({ "id": thread, "body": body }),
        )
    }

    fn graphql(query: &str, variables: Value) -> Self {
        Self::post(
            "/graphql".to_owned(),
            serde_json::json!({ "query": query, "variables": variables }),
        )
    }

    fn post(path: String, body: Value) -> Self {
        Self {
            verb: Verb::Post,
            path,
            body: Some(body.to_string()),
        }
    }
}

// What a check run's create and update share: its status, how it ended and
// the title and summary it shows.
fn check_body(check: &GateCheck) -> Value {
    let mut body = serde_json::json!({
        "status": check.status(),
        "output": { "title": check.title, "summary": check.summary },
    });
    if let Some(conclusion) = check.conclusion() {
        body["conclusion"] = Value::from(conclusion);
    }
    body
}

// `text` as one path segment: all but letters, digits and `-_.~` is
// percent-encoded, so a label's `:` or space cannot end the segment.
fn path_segment(text: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            byte => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The number in a new issue's answer
///
/// # Errors
///
/// [`ApiError::Unreadable`] when it has none.
pub fn parse_number(body: &str) -> Result<u64, ApiError> {
    let v: Value = serde_json::from_str(body).map_err(|_| ApiError::Unreadable("number"))?;
    v["number"].as_u64().ok_or(ApiError::Unreadable("number"))
}

/// The id of the review a post's answer describes
///
/// # Errors
///
/// [`ApiError::Unreadable`] when the answer has no id.
pub fn parse_review(body: &str) -> Result<u64, ApiError> {
    let v: Value = serde_json::from_str(body).map_err(|_| ApiError::Unreadable("id"))?;
    v["id"].as_u64().ok_or(ApiError::Unreadable("id"))
}

/// The id of the check run a create's answer describes
///
/// # Errors
///
/// [`ApiError::Unreadable`] when the answer has no id.
pub fn parse_check_run(body: &str) -> Result<CheckRunId, ApiError> {
    let v: Value = serde_json::from_str(body).map_err(|_| ApiError::Unreadable("id"))?;
    v["id"]
        .as_u64()
        .map(CheckRunId)
        .ok_or(ApiError::Unreadable("id"))
}

/// The id and creation time of the comment a post's answer describes
///
/// # Errors
///
/// [`ApiError::Unreadable`] when the answer has no id or no time.
pub fn parse_posted(body: &str) -> Result<(u64, Timestamp), ApiError> {
    let v: Value = serde_json::from_str(body).map_err(|_| ApiError::Unreadable("id"))?;
    let id = v["id"].as_u64().ok_or(ApiError::Unreadable("id"))?;
    let created = v["created_at"]
        .as_str()
        .and_then(|t| t.parse::<jiff::Timestamp>().ok());
    let created = created.and_then(|t| u64::try_from(t.as_second()).ok());
    let created = created.ok_or(ApiError::Unreadable("created_at"))?;
    Ok((id, Timestamp(created)))
}

/// What GraphQL answered a write on a review thread
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadWrite {
    /// The mutation took effect
    Done,
    /// GitHub has no such thread, so there is nothing to write on
    Gone,
    /// GitHub refused it, with the first message it gave
    Refused(String),
}

/// What a GraphQL `answer` says of the thread mutation `mutation`
///
/// GraphQL answers HTTP 200 with its errors in the body.
///
/// # Errors
///
/// [`ApiError::Unreadable`] when the answer is not JSON, or holds neither
/// errors nor the mutation's result.
pub fn parse_thread_write(answer: &str, mutation: &'static str) -> Result<ThreadWrite, ApiError> {
    let v: Value = serde_json::from_str(answer).map_err(|_| ApiError::Unreadable(mutation))?;
    if let Some(errors) = v["errors"].as_array().filter(|e| !e.is_empty()) {
        let message = |e: &Value| e["message"].as_str().unwrap_or_default().to_owned();
        let gone = errors.iter().any(|e| {
            e["type"] == "NOT_FOUND"
                || message(e).contains("Could not resolve to a node with the global id")
        });
        return Ok(match gone {
            true => ThreadWrite::Gone,
            false => ThreadWrite::Refused(message(&errors[0])),
        });
    }
    let result = &v["data"][mutation];
    if mutation == "resolveReviewThread" && result["thread"]["isResolved"] != true {
        return Err(ApiError::Unreadable(mutation));
    }
    match result.is_object() {
        true => Ok(ThreadWrite::Done),
        false => Err(ApiError::Unreadable(mutation)),
    }
}

/// Signs with an App's private key: RS256, an RSA signature over SHA-256
pub trait Signer: Send + Sync {
    /// The signature of `input` by the key in the file `key`
    ///
    /// # Errors
    ///
    /// Why the key could not sign.
    fn sign(&self, key: &Path, input: &[u8]) -> Result<Vec<u8>, String>;
}

/// The error for an answer of `status` that is not the call's success,
/// telling a rate limit from a refusal by `body`, and carrying what the
/// answer's headers said of when to ask again as `cooldown`
pub fn refusal(status: u16, body: &str, cooldown: Cooldown) -> ApiError {
    match status {
        301 => ApiError::Moved,
        429 => ApiError::RateLimited(cooldown),
        403 if body.to_ascii_lowercase().contains("rate limit") => ApiError::RateLimited(cooldown),
        status => ApiError::Refused(status),
    }
}

/// What an installation token may do, which is less than the App may
///
/// A token asks for permissions the App holds and gets those alone, so a
/// token is never more than the call that uses it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// Kelpie's own posts: issues and pull requests, written
    Kelpie,
    /// A worker's: contents, written, which lets it push its branch, and
    /// workflows, written, where the installation holds it, which lets it
    /// change a workflow file. Nothing else: no comment, review, approval or
    /// merge.
    Worker {
        /// Whether the token asks for `workflows` too
        workflows: bool,
    },
    /// The resolve of a review thread: pull requests and contents, written
    ///
    /// GitHub refuses `resolveReviewThread` to a token without Contents
    /// written, though its documentation does not say so. It is minted for
    /// that call alone, because it can push, which kelpie's posts cannot.
    Threads,
    /// Kelpie's check runs: checks, written. Kept apart from the posting token
    /// so an installation that has not accepted Checks still posts comments.
    Checks,
    /// Kelpie's reads of the board, pull requests, reviews and checks: issues,
    /// pull requests, checks and commit statuses, read. Nothing is written.
    Reader,
    /// What [`Grant::Reader`] asks for less the checks and statuses, for an App
    /// registered before it asked for them: GitHub answers 422 to a request
    /// for a permission the App was never given
    ReaderCore,
}

impl Grant {
    /// What the token is kept for until it nears its end, or none when it is
    /// minted for one call. The posting token and the reading one are kept
    /// apart.
    pub(crate) fn kept_for(self) -> Option<Purpose> {
        match self {
            Self::Kelpie => Some(Purpose::Posts),
            Self::Reader | Self::ReaderCore => Some(Purpose::Reads),
            Self::Checks => Some(Purpose::Checks),
            Self::Worker { .. } | Self::Threads => None,
        }
    }
}

/// What a kept token is for
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Purpose {
    /// Kelpie's posts
    Posts,
    /// Kelpie's reads
    Reads,
    /// Kelpie's check runs
    Checks,
}

/// The body of an access token's request, which limits the token to `repo`
/// and to what `grant` allows
pub fn token_request(repo: &ForgeSlug, grant: Grant) -> String {
    let permissions = match grant {
        Grant::Kelpie => serde_json::json!({ "issues": "write", "pull_requests": "write" }),
        Grant::Worker { workflows: false } => serde_json::json!({ "contents": "write" }),
        Grant::Worker { workflows: true } => {
            serde_json::json!({ "contents": "write", "workflows": "write" })
        }
        Grant::Threads => serde_json::json!({ "contents": "write", "pull_requests": "write" }),
        Grant::Checks => serde_json::json!({ "checks": "write" }),
        Grant::Reader => serde_json::json!({
            "issues": "read",
            "pull_requests": "read",
            "checks": "read",
            "statuses": "read",
        }),
        Grant::ReaderCore => serde_json::json!({ "issues": "read", "pull_requests": "read" }),
    };
    serde_json::json!({ "permissions": permissions, "repositories": [repo.name()] }).to_string()
}

/// The App in a conversion's answer
///
/// # Errors
///
/// [`ApiError::Unreadable`] naming the first field missing.
pub fn parse_conversion(body: &str) -> Result<Conversion, ApiError> {
    let v: Value = serde_json::from_str(body).map_err(|_| ApiError::Unreadable("id"))?;
    let text = |field: &'static str, at: &Value| {
        (at.as_str().filter(|s| !s.is_empty()).map(str::to_owned))
            .ok_or(ApiError::Unreadable(field))
    };
    Ok(Conversion {
        id: v["id"].as_u64().ok_or(ApiError::Unreadable("id"))?,
        slug: text("slug", &v["slug"])?,
        client_id: text("client_id", &v["client_id"])?,
        owner: text("owner.login", &v["owner"]["login"])?,
        pem: Pem(text("pem", &v["pem"])?),
    })
}

/// The installation in `GET /repos/{owner}/{repo}/installation`'s answer
///
/// # Errors
///
/// [`ApiError::Unreadable`] when it has no id.
pub fn parse_installation(body: &str) -> Result<Installation, ApiError> {
    let v: Value = serde_json::from_str(body).map_err(|_| ApiError::Unreadable("id"))?;
    Ok(Installation {
        id: v["id"].as_u64().ok_or(ApiError::Unreadable("id"))?,
        workflows: v["permissions"]["workflows"].as_str() == Some("write"),
    })
}

/// The token in an access token's answer, and when it lapses
///
/// # Errors
///
/// [`ApiError::Unreadable`] naming the field missing or unreadable.
pub fn parse_token(body: &str) -> Result<IssuedToken, ApiError> {
    let v: Value = serde_json::from_str(body).map_err(|_| ApiError::Unreadable("token"))?;
    let token =
        (v["token"].as_str().filter(|t| !t.is_empty())).ok_or(ApiError::Unreadable("token"))?;
    let expires_at = (v["expires_at"].as_str())
        .and_then(|at| at.parse::<jiff::Timestamp>().ok())
        .and_then(|at| u64::try_from(at.as_second()).ok())
        .ok_or(ApiError::Unreadable("expires_at"))?;
    let permissions = (v["permissions"].as_object().into_iter().flatten())
        .filter_map(|(name, level)| Some((name.clone(), level.as_str()?.to_owned())))
        .collect();
    Ok(IssuedToken {
        token: InstallationToken::new(token.to_owned()),
        expires_at: Timestamp(expires_at),
        permissions,
    })
}
