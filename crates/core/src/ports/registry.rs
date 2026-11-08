//! The registry a package an issue needs is published to
//!
//! The issue check reads no network, so the triage poll asks this port, with the
//! runner's lock let go, whether the packages a ready issue's `Needs:` line names
//! are out yet.

use std::fmt;

use crate::board::check::Needed;

/// Reads whether a package version is published
pub trait Registry: Send + Sync {
    /// Whether `needed`'s version is published to its registry: `false` where the
    /// registry answers that it holds no such package or version
    ///
    /// # Errors
    ///
    /// [`RegistryError`], where the registry cannot say: it was not reached, it
    /// refused the read, or it is one the adapter cannot read, such as a private one.
    fn published(&self, needed: &Needed) -> Result<bool, RegistryError>;
}

/// Why a registry could not say whether a package is published
///
/// None of these carry a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// `curl` could not be started, with the OS's reason
    Spawn(String),
    /// `curl` could not reach the registry, with its exit code
    Unreachable(i32),
    /// The registry answered with this HTTP status, which says neither yes nor no
    Refused(u16),
    /// The registry answered in a shape kelpie cannot read
    Unreadable,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run curl: {error}"),
            Self::Unreachable(code) => write!(f, "curl could not reach the registry (exit {code})"),
            Self::Refused(status) => write!(f, "the registry answered HTTP {status}"),
            Self::Unreadable => f.write_str("the registry's answer could not be read"),
        }
    }
}

impl core::error::Error for RegistryError {}
