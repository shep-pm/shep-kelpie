//! Installation tokens, minted from an App's private key
//!
//! A token is asked for with a JWT the App signs: RS256, issued 60 seconds
//! back so a clock running ahead of GitHub's still passes, and lapsing nine
//! minutes on, inside GitHub's ten. Each token reaches one repo alone, and
//! is kept until five minutes before GitHub lets it lapse, then minted again.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use super::api::{ApiError, GithubApi, Grant, IssuedToken, Purpose, Signer};
use super::{App, Apps, Owner, StoreError};
use crate::ports::{Clock, InstallationToken, Timestamp};
use crate::settings::ForgeSlug;

/// How far back a JWT says it was issued, for clock drift
const ISSUED_BACK: u64 = 60;

/// How long after now a JWT lapses; GitHub takes ten minutes at most
const LAPSES_IN: u64 = 9 * 60;

/// How long before a token lapses kelpie mints another
const REMINT_BEFORE: u64 = 5 * 60;

/// How long an App found not installed on a repo is believed, so a post to a
/// repo it does not cover costs no JWT and no call each time
const NOT_INSTALLED_FOR: u64 = 10 * 60;

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
    /// The App was never given the Contents permission a worker's token
    /// holds: it was registered before workers held its tokens
    NoContents(App),
    /// GitHub made a worker's token hold more than the permission to push and
    /// to change workflow files, or not the push, with what it holds
    TooBroad(String),
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
            Self::NoContents(app) => write!(
                f,
                "{} was never given the Contents permission (read and write) a worker's \
                 token needs: add it in the App's settings, then accept it on the installation",
                app.slug
            ),
            Self::TooBroad(held) => write!(
                f,
                "GitHub made the worker's token hold {held}, which is not the permission to \
                 push and nothing else"
            ),
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

    /// A token for a worker's calls on `repo`: minted fresh, so it has its
    /// whole hour to run, and holding the permission to push, and to change
    /// workflow files where the App has that permission, and nothing that can
    /// comment, review, approve or merge
    ///
    /// # Errors
    ///
    /// As [`GithubApp::token`], and also when GitHub refuses the permission
    /// because the App was never given it.
    fn worker(&self, repo: &ForgeSlug) -> Result<WorkerToken, TokenError>;

    /// The token [`GithubApp::worker`] mints, and no more
    ///
    /// # Errors
    ///
    /// As [`GithubApp::worker`].
    fn worker_token(&self, repo: &ForgeSlug) -> Result<InstallationToken, TokenError> {
        self.worker(repo).map(|worker| worker.token)
    }

    /// A token to resolve a review thread on `repo`: minted fresh and never
    /// kept, since it can push, which the token of [`GithubApp::token`] cannot
    ///
    /// # Errors
    ///
    /// As [`GithubApp::token`].
    fn thread_token(&self, repo: &ForgeSlug) -> Result<InstallationToken, TokenError>;

    /// A token to write check runs on `repo` with, kept like
    /// [`GithubApp::token`] but apart from it: it holds Checks, written, and
    /// nothing else
    ///
    /// # Errors
    ///
    /// As [`GithubApp::token`]. GitHub answers 422, as
    /// [`TokenError::Api`]`(`[`ApiError::Refused`]`(422))`, when the
    /// installation has not accepted the Checks permission.
    fn checks_token(&self, repo: &ForgeSlug) -> Result<IssuedToken, TokenError>;

    /// A token to read `repo` with, kept until it nears its end like
    /// [`GithubApp::token`] but apart from it: it holds what the App has been
    /// given of the read permissions kelpie's reads use, and writes nothing
    ///
    /// The token's `permissions` say which of them the App holds, since an App
    /// registered before kelpie read through it lacks some.
    ///
    /// # Errors
    ///
    /// As [`GithubApp::token`].
    fn reader(&self, repo: &ForgeSlug) -> Result<IssuedToken, TokenError>;
}

/// A worker's token and what it lets the worker change beyond its branch
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerToken {
    /// The token
    pub token: InstallationToken,
    /// Whether it lets the worker push a change to a workflow file, which
    /// runs with the repo's secrets
    pub workflows: bool,
}

// An App's id and a repo's `owner/name` in lower case
type RepoKey = (u64, String);

// Each App's installation on a repo, by the App's id and `owner/name` in
// lower case, and each token by those and the installation it was minted for,
// so a replaced App's tokens are never handed out.
#[derive(Debug, Default)]
struct Kept {
    installations: BTreeMap<(u64, String), u64>,
    tokens: BTreeMap<(u64, String, u64, Purpose), IssuedToken>,
    // Until when an App is believed not installed on a repo
    not_installed: BTreeMap<RepoKey, Timestamp>,
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
    flights: Mutex<BTreeMap<RepoKey, Arc<Mutex<()>>>>,
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
        self.mint(repo, Grant::Kelpie).map(|issued| issued.token)
    }

    fn thread_token(&self, repo: &ForgeSlug) -> Result<InstallationToken, TokenError> {
        self.mint(repo, Grant::Threads).map(|issued| issued.token)
    }

    fn checks_token(&self, repo: &ForgeSlug) -> Result<IssuedToken, TokenError> {
        self.mint(repo, Grant::Checks)
    }

    fn reader(&self, repo: &ForgeSlug) -> Result<IssuedToken, TokenError> {
        match self.mint(repo, Grant::Reader) {
            // GitHub answers 422 to a request for a permission the App lacks.
            Err(TokenError::Api(ApiError::Refused(422))) => self.mint(repo, Grant::ReaderCore),
            minted => minted,
        }
    }

    fn worker(&self, repo: &ForgeSlug) -> Result<WorkerToken, TokenError> {
        match (self.mint(repo, Grant::Worker { workflows: false })).and_then(pushes_only) {
            // GitHub answers 422 to a request for a permission the App lacks.
            Err(TokenError::Api(ApiError::Refused(422))) => {
                let owner = Owner::of(repo).map_err(TokenError::Owner)?;
                match self.apps.get(&owner).map_err(TokenError::Store)? {
                    Some(app) => Err(TokenError::NoContents(app)),
                    None => Err(TokenError::NoApp(owner)),
                }
            }
            minted => minted.map(|issued| WorkerToken {
                workflows: issued
                    .permissions
                    .get("workflows")
                    .is_some_and(|l| l == "write"),
                token: issued.token,
            }),
        }
    }
}

// What GitHub says a worker's token holds must be the permission to push,
// the permission to change workflow files where the App has it, and the read
// of metadata every token has, whatever was asked for.
fn pushes_only(issued: IssuedToken) -> Result<IssuedToken, TokenError> {
    let holds = |name: &str, level: &str| issued.permissions.get(name).is_some_and(|l| l == level);
    let only = (issued.permissions.iter()).all(|(name, level)| match name.as_str() {
        "contents" | "metadata" => true,
        "workflows" => level == "write",
        _ => false,
    });
    match holds("contents", "write") && only {
        true => Ok(issued),
        false => Err(TokenError::TooBroad(
            (issued.permissions.iter())
                .map(|(name, level)| format!("{name}: {level}"))
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

impl AppTokens {
    // A token of kelpie's kept until it nears its end. A worker's and a
    // thread's are never kept: both can push, so kelpie's own posts must not
    // be handed them. A worker's `grant` names no `workflows`: the
    // installation says whether to ask for it.
    fn mint(&self, repo: &ForgeSlug, grant: Grant) -> Result<IssuedToken, TokenError> {
        let owner = Owner::of(repo).map_err(TokenError::Owner)?;
        let app = (self.apps.get(&owner).map_err(TokenError::Store)?)
            .ok_or_else(|| TokenError::NoApp(owner.clone()))?;
        let key = (app.id, repo.as_str().to_ascii_lowercase());
        // One flight per App and repo, held across the calls: two askers for
        // one repo never mint at once, and a slow answer stalls no other repo.
        let flight = {
            let mut flights = self.flights.lock().unwrap_or_else(PoisonError::into_inner);
            // A flight nobody holds or waits on is dropped, so the map holds
            // only the repos in flight now.
            flights.retain(|k, flight| *k == key || Arc::strong_count(flight) > 1);
            Arc::clone(flights.entry(key.clone()).or_default())
        };
        let _flight = flight.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.clock.now();
        let kept_for = grant.kept_for();
        let token_key = |id: u64, purpose: Purpose| (key.0, key.1.clone(), id, purpose);
        let installation = {
            let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
            let installation = kept.installations.get(&key).copied();
            let issued = installation
                .zip(kept_for)
                .and_then(|(id, purpose)| kept.tokens.get(&token_key(id, purpose)));
            if let Some(issued) = issued
                && now.0.saturating_add(REMINT_BEFORE) < issued.expires_at.0
            {
                return Ok(issued.clone());
            }
            if let Some(until) = kept.not_installed.get(&key)
                && now < *until
            {
                return Err(TokenError::NotInstalled {
                    app: app.clone(),
                    repo: repo.clone(),
                });
            }
            // A worker's token asks for what the installation holds now, so
            // the installation is read afresh, and a permission the
            // maintainer accepted since takes effect on the next call.
            installation.filter(|_| !matches!(grant, Grant::Worker { .. }))
        };
        let jwt = self.jwt(&app, &owner, now)?;
        let (id, workflows) = match installation {
            Some(id) => (id, false),
            None => match self.api.installation(&jwt, repo)? {
                Some(found) => (found.id, found.workflows),
                None => {
                    // Believed for a while, so a repo the App does not cover costs no
                    // JWT and no call each time it is asked about.
                    let until = Timestamp(now.0.saturating_add(NOT_INSTALLED_FOR));
                    let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
                    kept.not_installed.insert(key, until);
                    return Err(TokenError::NotInstalled {
                        app,
                        repo: repo.clone(),
                    });
                }
            },
        };
        let grant = match grant {
            Grant::Worker { .. } => Grant::Worker { workflows },
            kelpie => kelpie,
        };
        let issued = match self.api.access_token(&jwt, id, repo, grant) {
            Ok(issued) => issued,
            // The App was taken off the repo, or its installation is gone.
            Err(e @ ApiError::Refused(401 | 404)) => {
                let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
                kept.tokens
                    .retain(|k, _| (k.0, &k.1, k.2) != (key.0, &key.1, id));
                kept.installations.remove(&key);
                return Err(e.into());
            }
            Err(e) => return Err(e.into()),
        };
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.not_installed.remove(&key);
        if let Some(purpose) = kept_for {
            kept.tokens.insert(token_key(id, purpose), issued.clone());
        }
        kept.installations.insert(key, id);
        Ok(issued)
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
