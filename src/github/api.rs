//! The GitHub calls an App makes, and the signer its tokens are asked with
//!
//! [`GithubApi`] is the port: the real one runs `curl`, and tests a stand-in
//! GitHub. The answers' JSON is read here, from the shapes GitHub's REST
//! documentation gives for each call.

use std::fmt;
use std::path::Path;

use serde_json::Value;

use super::tokens::{InstallationToken, Jwt};
use crate::ports::Timestamp;
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
    /// GitHub's answer lacked the field named
    Unreadable(&'static str),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(why) => write!(f, "cannot reach GitHub: {why}"),
            Self::Refused(status) => write!(f, "GitHub answered HTTP {status}"),
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

    /// A new token for `installation`, `POST /app/installations/{id}/access_tokens`
    ///
    /// # Errors
    ///
    /// [`ApiError`] when GitHub cannot be reached or refuses the App's `jwt`.
    fn access_token(&self, jwt: &Jwt, installation: u64) -> Result<IssuedToken, ApiError>;
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
