//! The sandbox port: a whole process, and everything it starts, held to a policy
//!
//! Kelpie runs every agent call inside a sandbox that the operating system
//! enforces, whatever the harness does or fails to do. The policy names what
//! the process may write, read and reach. Everything else is refused, and a
//! sandbox that cannot be set up refuses to run the process at all.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

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
    /// Hosts it may never reach, whatever the allowed hosts say, on any port
    pub denied_hosts: Vec<String>,
    /// Addresses an allowed host name may not resolve to
    pub denied_addresses: Vec<String>,
    /// A host it may reach only through a process outside, and no other way
    pub forward: Option<Forward>,
    /// Whether it may listen on a local port, as a dev server does
    pub listen: bool,
    /// The macOS services it may look up, beyond the sandbox's own
    pub services: Vec<String>,
    /// Whether it may ask macOS to verify a certificate, which Go's TLS needs
    pub verify_tls: bool,
}

/// A host whose traffic the sandbox's proxy hands to a Unix socket, not the network
///
/// The sandbox needs neither the host nor the socket: the proxy runs outside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    /// The host the sandboxed process dials
    pub host: String,
    /// The socket a process outside serves, which only the proxy dials
    pub socket: PathBuf,
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

/// A sandbox that keeps some paths unread by every call it runs, whatever
/// the call's policy says
///
/// Kelpie's own Codex login is kept this way, so a call on any harness
/// leaves it unread. A call that names a file there in its policy's `read`
/// may still read that file.
#[derive(Debug, Clone)]
pub struct Unreadable {
    inner: Arc<dyn Sandbox>,
    paths: Vec<String>,
}

impl Unreadable {
    /// `inner`, with `paths`, absolute or under `~/` with `**` globs,
    /// added to every policy's `no_read`
    pub fn new(inner: Arc<dyn Sandbox>, paths: Vec<String>) -> Self {
        Self { inner, paths }
    }
}

impl Sandbox for Unreadable {
    fn wrap(
        &self,
        policy: &Policy,
        settings: &Path,
        command: &Command,
    ) -> Result<Command, SandboxError> {
        let mut policy = policy.clone();
        policy.no_read.extend(self.paths.iter().cloned());
        self.inner.wrap(&policy, settings, command)
    }
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // Keeps the last policy it was handed.
    #[derive(Debug, Default)]
    struct Seen(Mutex<Option<Policy>>);

    impl Sandbox for Seen {
        fn wrap(
            &self,
            policy: &Policy,
            _: &Path,
            command: &Command,
        ) -> Result<Command, SandboxError> {
            *self.0.lock().unwrap() = Some(policy.clone());
            let mut same = Command::new(command.get_program());
            same.args(command.get_args());
            Ok(same)
        }
    }

    #[test]
    fn every_call_leaves_the_paths_unread_beside_its_own() {
        let seen = Arc::new(Seen::default());
        let sandbox = Unreadable::new(seen.clone(), vec!["/k/codex/**".into()]);
        let policy = Policy {
            no_read: vec!["~/.ssh/**".into()],
            read: vec![PathBuf::from("/k/codex/auth.json")],
            ..Policy::default()
        };
        sandbox
            .wrap(&policy, Path::new("/s.json"), &Command::new("true"))
            .unwrap();
        let wrapped = seen.0.lock().unwrap().clone().unwrap();
        assert_eq!(wrapped.no_read, ["~/.ssh/**", "/k/codex/**"]);
        assert_eq!(wrapped.read, policy.read);
    }
}
