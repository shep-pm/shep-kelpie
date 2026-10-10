//! Installation tokens, minted from an App's private key
//!
//! A token is asked for with a JWT the App signs: RS256, issued 60 seconds
//! back so a clock running ahead of GitHub's still passes, and lapsing nine
//! minutes on, inside GitHub's ten. Each token reaches one repo alone, and
//! is kept until five minutes before GitHub lets it lapse, then minted again.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use super::api::{ApiError, GithubApi, IssuedToken, Signer};
use super::{App, Apps, Owner, StoreError};
use crate::ports::{Clock, Timestamp};
use crate::settings::ForgeSlug;

/// How far back a JWT says it was issued, for clock drift
const ISSUED_BACK: u64 = 60;

/// How long after now a JWT lapses; GitHub takes ten minutes at most
const LAPSES_IN: u64 = 9 * 60;

/// How long before a token lapses kelpie mints another
const REMINT_BEFORE: u64 = 5 * 60;

/// An installation token, which acts as the App on the repos it is installed on
///
/// `Debug` does not show it.
#[derive(Clone, PartialEq, Eq)]
pub struct InstallationToken(String);

impl InstallationToken {
    /// The token `text`
    pub fn new(text: String) -> Self {
        Self(text)
    }

    /// The token's text, for the call that sends it
    #[inline]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for InstallationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InstallationToken(..)")
    }
}

/// A signed JWT that asks GitHub for an App's installation and tokens
///
/// `Debug` does not show it.
#[derive(Clone, PartialEq, Eq)]
pub struct Jwt(String);

impl Jwt {
    /// The JWT's text, for the call that sends it
    #[inline]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Jwt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Jwt(..)")
    }
}

/// Why no token could be had for a repo
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// The repo's owner is not a GitHub login, with why
    Owner(String),
    /// Kelpie has no App for the repo's owner
    NoApp(Owner),
    /// The owner's App is not installed on the repo
    NotInstalled {
        /// The App
        app: App,
        /// The repo
        repo: ForgeSlug,
    },
    /// The App's files could not be read
    Store(StoreError),
    /// The App's key could not sign a JWT, with why
    Sign(String),
    /// GitHub did not answer as asked
    Api(ApiError),
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Owner(why) => f.write_str(why),
            Self::NoApp(owner) => write!(f, "kelpie has no GitHub App for {owner}"),
            Self::NotInstalled { app, repo } => {
                write!(f, "{} is not installed on {}", app.slug, repo.as_str())
            }
            Self::Store(e) => e.fmt(f),
            Self::Sign(why) => write!(f, "the App's key cannot sign: {why}"),
            Self::Api(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for TokenError {}

impl From<ApiError> for TokenError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

/// Hands out a token that acts as kelpie's App on a repo
pub trait GithubApp: Send + Sync {
    /// A token for `repo`, from the App of its owner, minted again shortly
    /// before the one kept lapses
    ///
    /// # Errors
    ///
    /// [`TokenError`] when there is no App for the owner, it is not
    /// installed on `repo`, or no token could be minted.
    fn token(&self, repo: &ForgeSlug) -> Result<InstallationToken, TokenError>;
}

// Each App's installation on a repo, by the App's id and `owner/name` in
// lower case, and each token by those and the installation it was minted for,
// so a replaced App's tokens are never handed out.
#[derive(Debug, Default)]
struct Kept {
    installations: BTreeMap<(u64, String), u64>,
    tokens: BTreeMap<(u64, String, u64), IssuedToken>,
}

/// [`GithubApp`] over the Apps in kelpie's home
pub struct AppTokens {
    apps: Apps,
    api: Box<dyn GithubApi>,
    signer: Box<dyn Signer>,
    clock: Box<dyn Clock + Sync>,
    // The map is held only to read or write it; each flight is held across
    // the calls to GitHub for its App and repo.
    kept: Mutex<Kept>,
    flights: Mutex<BTreeMap<(u64, String), Arc<Mutex<()>>>>,
}

impl fmt::Debug for AppTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppTokens")
            .field("apps", &self.apps)
            .finish_non_exhaustive()
    }
}

impl AppTokens {
    /// Tokens for the Apps in `apps`, asked of `api` with JWTs `signer` signs
    pub fn new(
        apps: Apps,
        api: Box<dyn GithubApi>,
        signer: Box<dyn Signer>,
        clock: Box<dyn Clock + Sync>,
    ) -> Self {
        Self {
            apps,
            api,
            signer,
            clock,
            kept: Mutex::new(Kept::default()),
            flights: Mutex::default(),
        }
    }

    fn jwt(&self, app: &App, owner: &Owner, now: Timestamp) -> Result<Jwt, TokenError> {
        let input = signing_input(&app.client_id, now);
        let signature = (self.signer)
            .sign(&self.apps.key(owner), input.as_bytes())
            .map_err(TokenError::Sign)?;
        Ok(Jwt(format!("{input}.{}", base64url(&signature))))
    }
}

impl GithubApp for AppTokens {
    fn token(&self, repo: &ForgeSlug) -> Result<InstallationToken, TokenError> {
        let owner = Owner::of(repo).map_err(TokenError::Owner)?;
        let app = (self.apps.get(&owner).map_err(TokenError::Store)?)
            .ok_or_else(|| TokenError::NoApp(owner.clone()))?;
        let key = (app.id, repo.as_str().to_ascii_lowercase());
        // One flight per App and repo, held across the calls: two askers for
        // one repo never mint at once, and a slow answer stalls no other repo.
        let flight = {
            let mut flights = self.flights.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(flights.entry(key.clone()).or_default())
        };
        let _flight = flight.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.clock.now();
        let token_key = |id: u64| (key.0, key.1.clone(), id);
        let installation = {
            let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
            let installation = kept.installations.get(&key).copied();
            let issued = installation.and_then(|id| kept.tokens.get(&token_key(id)));
            if let Some(issued) = issued
                && now.0.saturating_add(REMINT_BEFORE) < issued.expires_at.0
            {
                return Ok(issued.token.clone());
            }
            installation
        };
        let jwt = self.jwt(&app, &owner, now)?;
        let id = match installation {
            Some(id) => id,
            None => {
                (self.api.installation(&jwt, repo)?).ok_or_else(|| TokenError::NotInstalled {
                    app: app.clone(),
                    repo: repo.clone(),
                })?
            }
        };
        let issued = match self.api.access_token(&jwt, id, repo) {
            Ok(issued) => issued,
            // The App was taken off the repo, or its installation is gone.
            Err(e @ ApiError::Refused(401 | 404)) => {
                let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
                kept.tokens.remove(&token_key(id));
                kept.installations.remove(&key);
                return Err(e.into());
            }
            Err(e) => return Err(e.into()),
        };
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.tokens.insert(token_key(id), issued.clone());
        kept.installations.insert(key, id);
        Ok(issued.token)
    }
}

/// The JWT's header and claims, each base64url, joined by a dot: what the key signs
pub(crate) fn signing_input(client_id: &str, now: Timestamp) -> String {
    let header = r#"{"alg":"RS256","typ":"JWT"}"#;
    let claims = serde_json::json!({
        "iat": now.0.saturating_sub(ISSUED_BACK),
        "exp": now.0.saturating_add(LAPSES_IN),
        "iss": client_id,
    });
    format!(
        "{}.{}",
        base64url(header.as_bytes()),
        base64url(claims.to_string().as_bytes())
    )
}

/// RFC 4648 base64url without padding, as a JWT carries each part
pub(crate) fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, &b| n << 8 | u32::from(b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // A lazy `derive(Debug)` would print the JWT into a log.
    #[test]
    fn debug_never_shows_a_jwt() {
        let jwt = Jwt("eyJhbGciOi.s3cr3t.sig".to_owned());
        assert_eq!(format!("{jwt:?}"), "Jwt(..)");
    }
}
