//! Kelpie's GitHub App, one per repo owner: [`setup`] registers it,
//! [`Apps`] keeps it in kelpie's home, and [`GithubApp`] mints its tokens

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::ForgeSlug;
use crate::totp::private_dir;

pub mod api;
pub mod setup;
pub mod tokens;

pub use api::{ApiError, Conversion, GithubApi, IssuedToken, Pem, Signer};
pub use tokens::{AppTokens, GithubApp, InstallationToken, TokenError};

/// The folder in kelpie's home the Apps are kept in
pub const FOLDER: &str = "github";

const RECORD: &str = "app.json";
const KEY: &str = "key.pem";

/// A repo owner on GitHub, a user or an organization, in lower case
///
/// GitHub reads a login in any case, so an owner's folder is named in one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Owner(String);

impl Owner {
    /// The owner of `repo`
    ///
    /// # Errors
    ///
    /// Why the owner is not a GitHub login, as [`Owner::try_from`] says.
    pub fn of(repo: &ForgeSlug) -> Result<Self, String> {
        Self::try_from(repo.owner())
    }

    /// The login, in lower case
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for Owner {
    type Error = String;

    fn try_from(login: &str) -> Result<Self, Self::Error> {
        let allowed = |c: char| c.is_ascii_alphanumeric() || c == '-';
        if login.is_empty()
            || login.len() > 39
            || login.starts_with('-')
            || !login.chars().all(allowed)
        {
            return Err(format!("{login:?} is not a GitHub login"));
        }
        Ok(Self(login.to_ascii_lowercase()))
    }
}

impl std::fmt::Display for Owner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An App kelpie registered, as [`Apps`] keeps it
// wire format: changing this is a breaking change to `app.json`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    /// The App's id
    pub id: u64,
    /// The App's slug, which its pages and its bot's login are named for
    pub slug: String,
    /// The App's client id, which its tokens are asked for as
    pub client_id: String,
    /// The account that owns the App, the only one it installs on
    pub owner: String,
}

impl App {
    /// The page that installs the App on an owner's repos
    pub fn install_url(&self) -> String {
        format!("https://github.com/apps/{}/installations/new", self.slug)
    }
}

/// Why an App's files could not be read or written, naming the file and
/// never the key in it
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The key or a folder above it is a link, someone else's, or open to
    /// others, so it may have been read or replaced
    Exposed(PathBuf),
    /// A file could not be read or written, or is not what kelpie wrote, with why
    Unusable(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exposed(path) => write!(
                f,
                "{} may be used by others: it must be yours, not a link, and `chmod 700` for a \
                 folder or `chmod 600` for the key; register the App again with `--replace` if \
                 someone else may have read it",
                path.display()
            ),
            Self::Unusable(why) => f.write_str(why),
        }
    }
}

fn unusable(path: &Path, e: &io::Error) -> StoreError {
    StoreError::Unusable(format!("cannot use {}: {}", path.display(), e.kind()))
}

impl core::error::Error for StoreError {}

/// Kelpie's Apps, one folder per owner under kelpie's home
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Apps {
    folder: PathBuf,
}

impl Apps {
    /// The Apps kept in kelpie's home at `kelpie_home`
    pub fn under(kelpie_home: &Path) -> Self {
        Self {
            folder: kelpie_home.join(FOLDER),
        }
    }

    /// The App for `owner`, or `None` when kelpie has none
    ///
    /// # Errors
    ///
    /// [`StoreError`] when its files cannot be read, or the key or a folder
    /// above it is exposed.
    pub fn get(&self, owner: &Owner) -> Result<Option<App>, StoreError> {
        let record = self.record(owner);
        let text = match fs::read_to_string(&record) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(unusable(&record, &e)),
        };
        for path in [self.folder.clone(), self.owned(owner), self.key(owner)] {
            private(&path)?;
        }
        serde_json::from_str(&text).map(Some).map_err(|_| {
            StoreError::Unusable(format!(
                "{} is not an App kelpie wrote: `shep kelpie github setup --replace` writes it again",
                record.display()
            ))
        })
    }

    /// Keeps the App GitHub converted, in place of any App its owner had
    ///
    /// The folders are made the maintainer's alone first, and the key goes
    /// in before the record, so an App on record always has its key.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when GitHub's owner is not a login, a folder is
    /// someone else's or a link, or a file cannot be written.
    pub fn save(&self, conversion: &Conversion) -> Result<App, StoreError> {
        let owner = Owner::try_from(conversion.owner.as_str()).map_err(StoreError::Unusable)?;
        let app = App {
            id: conversion.id,
            slug: conversion.slug.clone(),
            client_id: conversion.client_id.clone(),
            owner: conversion.owner.clone(),
        };
        let folder = self.owned(&owner);
        private_dir(&folder).map_err(|e| unusable(&folder, &e))?;
        for path in [&self.folder, &folder] {
            let meta = fs::symlink_metadata(path).map_err(|e| unusable(path, &e))?;
            if meta.file_type().is_symlink() || meta.uid() != nix::unistd::getuid().as_raw() {
                return Err(StoreError::Exposed(path.clone()));
            }
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|e| unusable(path, &e))?;
        }
        let record = serde_json::to_string_pretty(&app).expect("an App serializes");
        put(&self.key(&owner), conversion.pem.expose().as_bytes())?;
        put(&self.record(&owner), record.as_bytes())?;
        Ok(app)
    }

    /// The private key's file for `owner`, which only a signer reads
    pub fn key(&self, owner: &Owner) -> PathBuf {
        self.owned(owner).join(KEY)
    }

    fn owned(&self, owner: &Owner) -> PathBuf {
        self.folder.join(owner.as_str())
    }

    fn record(&self, owner: &Owner) -> PathBuf {
        self.owned(owner).join(RECORD)
    }
}

// Refuses `path` when it is a link, someone else's, or open to others.
fn private(path: &Path) -> Result<(), StoreError> {
    let meta = fs::symlink_metadata(path).map_err(|e| unusable(path, &e))?;
    let theirs = meta.uid() != nix::unistd::getuid().as_raw();
    if meta.file_type().is_symlink() || theirs || meta.mode() & 0o077 != 0 {
        return Err(StoreError::Exposed(path.to_owned()));
    }
    Ok(())
}

// Writes `bytes` beside `path`, readable by its owner alone, syncs it and
// renames it over `path`.
fn put(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let fresh = path.with_extension(format!("new.{}", std::process::id()));
    let _ = fs::remove_file(&fresh);
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&fresh)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| fs::rename(&fresh, path));
    let _ = fs::remove_file(&fresh);
    written.map_err(|e| unusable(path, &e))
}

#[cfg(test)]
mod tests;
