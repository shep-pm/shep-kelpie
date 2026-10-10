//! A stand-in GitHub for kelpie's App, and a signer that needs no key

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::FakeClock;
use crate::github::tokens::Jwt;
use crate::github::{
    ApiError, AppTokens, Apps, Conversion, GithubApi, InstallationToken, IssuedToken, Pem, Signer,
};
use crate::ports::{Clock, Timestamp};
use crate::settings::ForgeSlug;

/// A key no test would print by chance, so finding it anywhere else is a leak
pub(crate) const PEM: &str =
    "-----BEGIN RSA PRIVATE KEY-----\nkelpie-test-app-key-s3cr3t\n-----END RSA PRIVATE KEY-----\n";

/// What the stand-in was asked, in order
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Asked {
    Convert(String),
    Installation(String),
    AccessToken(u64),
}

#[derive(Debug, Default)]
struct Hub {
    installed: BTreeMap<String, u64>,
    asked: Vec<Asked>,
    jwts: Vec<String>,
    minted: u32,
}

/// GitHub as kelpie's App sees it: one code converts to an App owned by
/// `owner`, and each token lasts an hour from the clock's now
#[derive(Debug, Clone)]
pub(crate) struct FakeGithub {
    hub: Arc<Mutex<Hub>>,
    clock: FakeClock,
    owner: String,
}

impl FakeGithub {
    /// How long a token it mints lasts, as GitHub's do
    pub(crate) const LIFETIME: u64 = 3600;

    pub(crate) fn new(clock: FakeClock, owner: &str) -> Self {
        Self {
            hub: Arc::default(),
            clock,
            owner: owner.to_owned(),
        }
    }

    /// The App the stand-in hands back for a code
    pub(crate) fn conversion(&self) -> Conversion {
        Conversion {
            id: 42,
            slug: format!("kelpie-{}", self.owner.to_ascii_lowercase()),
            client_id: "Iv23liTEST".to_owned(),
            owner: self.owner.clone(),
            pem: Pem::new(PEM.to_owned()),
        }
    }

    /// Installs the App on `repo` as installation `id`
    pub(crate) fn install(&self, repo: &str, id: u64) {
        self.hub
            .lock()
            .unwrap()
            .installed
            .insert(repo.to_owned(), id);
    }

    pub(crate) fn asked(&self) -> Vec<Asked> {
        self.hub.lock().unwrap().asked.clone()
    }

    /// Every JWT a call was made with
    pub(crate) fn jwts(&self) -> Vec<String> {
        self.hub.lock().unwrap().jwts.clone()
    }

    /// Kelpie's App tokens over this stand-in and [`FakeSigner`], kept in `kelpie_home`
    pub(crate) fn tokens(&self, kelpie_home: &Path) -> AppTokens {
        AppTokens::new(
            Apps::under(kelpie_home),
            Box::new(self.clone()),
            Box::new(FakeSigner),
            Box::new(self.clock.clone()),
        )
    }

    /// Registers the App in `kelpie_home` as setup would, and installs it on `repo`
    pub(crate) fn registered(&self, kelpie_home: &Path, repo: &str) {
        Apps::under(kelpie_home).save(&self.conversion()).unwrap();
        self.install(repo, 7);
    }
}

impl GithubApi for FakeGithub {
    fn convert(&self, code: &str) -> Result<Conversion, ApiError> {
        self.hub
            .lock()
            .unwrap()
            .asked
            .push(Asked::Convert(code.to_owned()));
        Ok(self.conversion())
    }

    fn installation(&self, jwt: &Jwt, repo: &ForgeSlug) -> Result<Option<u64>, ApiError> {
        let mut hub = self.hub.lock().unwrap();
        hub.asked
            .push(Asked::Installation(repo.as_str().to_owned()));
        hub.jwts.push(jwt.expose().to_owned());
        Ok(hub.installed.get(repo.as_str()).copied())
    }

    fn access_token(&self, jwt: &Jwt, installation: u64) -> Result<IssuedToken, ApiError> {
        let mut hub = self.hub.lock().unwrap();
        hub.asked.push(Asked::AccessToken(installation));
        hub.jwts.push(jwt.expose().to_owned());
        hub.minted += 1;
        Ok(IssuedToken {
            token: InstallationToken::new(format!("ghs_test{}", hub.minted)),
            expires_at: Timestamp(self.clock.now().0 + Self::LIFETIME),
        })
    }
}

/// Signs anything with one fixed signature
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FakeSigner;

impl FakeSigner {
    /// What every signature is
    pub(crate) const SIGNATURE: &[u8] = b"signed";
}

impl Signer for FakeSigner {
    fn sign(&self, _key: &Path, _input: &[u8]) -> Result<Vec<u8>, String> {
        Ok(Self::SIGNATURE.to_vec())
    }
}
