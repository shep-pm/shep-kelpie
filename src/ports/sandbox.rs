//! The sandbox port: a whole process, and everything it starts, held to a policy
//!
//! Kelpie runs every agent call inside a sandbox that the operating system
//! enforces, whatever the harness does or fails to do. The policy names what
//! the process may write, read and reach. Everything else is refused, and a
//! sandbox that cannot be set up refuses to run the process at all.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What a sandboxed process may do, beyond reading files
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// Folders and files it may write, and everything under them
    pub write: Vec<PathBuf>,
    /// Paths under those it may not write. `**` matches folders at any depth.
    pub no_write: Vec<PathBuf>,
    /// Paths it may not read, absolute or under `~/`, with `**` globs
    pub no_read: Vec<String>,
    /// Paths it may read inside `no_read` after all
    pub read: Vec<PathBuf>,
    /// The hosts it may reach, and no others. A leading `*.` covers subdomains.
    pub hosts: Vec<String>,
    /// The Unix sockets it may connect to
    pub sockets: Vec<PathBuf>,
    /// Whether it may listen on a local port, as a dev server does
    pub listen: bool,
    /// The macOS services it may look up, beyond the sandbox's own
    pub services: Vec<String>,
    /// Whether it may ask macOS to verify a certificate, which Go's TLS needs
    pub verify_tls: bool,
}

/// Runs a process inside a [`Policy`]
pub trait Sandbox: Send + Sync + fmt::Debug {
    /// `command`, with its folder and variables, as it runs inside `policy`
    ///
    /// The sandbox's own settings are written to `settings`, which must be a
    /// file the sandboxed process cannot write.
    ///
    /// # Errors
    ///
    /// [`SandboxError`] when the sandbox is missing or its settings cannot be written.
    fn wrap(
        &self,
        policy: &Policy,
        settings: &Path,
        command: &Command,
    ) -> Result<Command, SandboxError>;
}

/// Why a process could not be put in its sandbox
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// The sandbox program is not at this path
    Missing(PathBuf),
    /// The sandbox's settings could not be written at this path, for this reason
    Settings(PathBuf, String),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(path) => write!(
                f,
                "the sandbox runtime is missing at {}: run `shep kelpie tools install`",
                path.display()
            ),
            Self::Settings(path, reason) => write!(
                f,
                "cannot write the sandbox's settings at {}: {reason}",
                path.display()
            ),
        }
    }
}

impl core::error::Error for SandboxError {}
