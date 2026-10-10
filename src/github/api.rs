//! The GitHub calls an App makes, and the signer its tokens are asked with
//!
//! [`GithubApi`] is the port: the real one runs `curl`, and tests a stand-in
//! GitHub. The answers' JSON is read here, from the shapes GitHub's REST
//! documentation gives for each call.

use std::fmt;
use std::path::Path;

use serde_json::Value;

use super::tokens::{InstallationToken, Jwt};
use crate::ports::{NewLabel, Timestamp};
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

/// An installation token and when GitHub lets it lapse
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedToken {
    /// The token
    pub token: InstallationToken,
    /// When it stops working
    pub expires_at: Timestamp,
}

/// Why a call to GitHub came to nothing
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// GitHub could not be reached, with why
    Unreachable(String),
    /// GitHub answered with this HTTP status, not the one the call wants
    Refused(u16),
    /// GitHub answered 429, or 403 saying a rate limit was passed
    RateLimited,
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
            Self::Unreachable(_) | Self::RateLimited => true,
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
            Self::RateLimited => f.write_str("GitHub's rate limit for the App is spent for now"),
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

    /// The id of the App's installation on `repo`, `GET /repos/{owner}/{repo}/installation`,
    /// or `None` when GitHub says the App is not installed there
    ///
    /// # Errors
    ///
    /// [`ApiError`] when GitHub cannot be reached or refuses the App's `jwt`.
    fn installation(&self, jwt: &Jwt, repo: &ForgeSlug) -> Result<Option<u64>, ApiError>;

    /// A new token for `installation` that reaches `repo` alone,
    /// `POST /app/installations/{id}/access_tokens` with `{"repositories": [<name>]}`
    ///
    /// # Errors
    ///
    /// [`ApiError`] when GitHub cannot be reached or refuses the App's `jwt`.
    fn access_token(
        &self,
        jwt: &Jwt,
        installation: u64,
        repo: &ForgeSlug,
    ) -> Result<IssuedToken, ApiError>;

    /// Makes `call` as the App whose installation `token` is, and returns
    /// the answer's body
    ///
    /// # Errors
    ///
    /// [`ApiError`] when GitHub cannot be reached or does not take the call.
    fn write(&self, token: &InstallationToken, call: &Call) -> Result<String, ApiError>;
}

/// How a write is sent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// `POST`
    Post,
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

    /// A new issue, `POST /repos/{owner}/{repo}/issues`
    pub fn issue(repo: &ForgeSlug, title: &str, body: &str, labels: &[&str]) -> Self {
        Self::post(
            format!("/repos/{}/issues", repo.as_str()),
            serde_json::json!({ "title": title, "body": body, "labels": labels }),
        )
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

    fn post(path: String, body: Value) -> Self {
        Self {
            verb: Verb::Post,
            path,
            body: Some(body.to_string()),
        }
    }
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
/// telling a rate limit from a refusal by `body`
pub fn refusal(status: u16, body: &str) -> ApiError {
    match status {
        301 => ApiError::Moved,
        429 => ApiError::RateLimited,
        403 if body.to_ascii_lowercase().contains("rate limit") => ApiError::RateLimited,
        status => ApiError::Refused(status),
    }
}

/// The body of an access token's request, which limits the token to `repo`
pub fn token_request(repo: &ForgeSlug) -> String {
    serde_json::json!({ "repositories": [repo.name()] }).to_string()
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

/// The installation's id in `GET /repos/{owner}/{repo}/installation`'s answer
///
/// # Errors
///
/// [`ApiError::Unreadable`] when it has none.
pub fn parse_installation(body: &str) -> Result<u64, ApiError> {
    let v: Value = serde_json::from_str(body).map_err(|_| ApiError::Unreadable("id"))?;
    v["id"].as_u64().ok_or(ApiError::Unreadable("id"))
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
    Ok(IssuedToken {
        token: InstallationToken::new(token.to_owned()),
        expires_at: Timestamp(expires_at),
    })
}
