//! The relay port: sending the maintainer's relay session a ruling

use std::fmt;

use crate::settings::Effort;

/// Sends a ruling to the maintainer's relay session
///
/// The relay is one background Claude Code session, found by its fixed
/// name so a second one is never started. Sending only delivers the
/// message: the relay's own reply, if any, is not read here. The
/// maintainer's answer comes back later through `shep trigger`, on its own.
pub trait Relay: Send + Sync {
    /// Writes the relay's settings and instructions files, and clears a
    /// relay started on older ones, which reads both only when it starts
    ///
    /// Returns whether it cleared. Called before each [`Self::send`].
    ///
    /// # Errors
    ///
    /// [`RelayError`] when the files cannot be written, or a running relay
    /// on older ones could not be deleted.
    fn renew(&self) -> Result<bool, RelayError>;

    /// How many times the relay has been cleared, by any project's runner
    ///
    /// Every project shares the one relay, so a count that moved on since a
    /// runner last read it means a clear took that runner's rulings too.
    ///
    /// # Errors
    ///
    /// [`RelayError::CannotCount`] when the count cannot be read.
    fn clear_count(&self) -> Result<u64, RelayError>;

    /// Sends `message`, starting the relay first if none is running
    ///
    /// `model` and `effort` are passed to `--model`/`--effort` only when a
    /// start is needed: a relay already running keeps what it started with.
    ///
    /// # Errors
    ///
    /// [`RelayError`] when the relay cannot be started or reached.
    fn send(&self, message: &str, model: &str, effort: Effort) -> Result<(), RelayError>;

    /// Sends `message` to the relay if one is running, and never starts one
    ///
    /// A relay started afresh never asked what `message` is about.
    ///
    /// # Errors
    ///
    /// [`RelayError`] when a running relay cannot be reached.
    fn tell(&self, message: &str) -> Result<(), RelayError>;

    /// Deletes the relay, if one exists, conversation included, so kelpie
    /// starts a fresh one next time and nothing a worker's question tried
    /// to carry into it survives the clear
    ///
    /// Counted in [`Self::clear_count`] once it succeeds, whether or not one was
    /// running.
    ///
    /// # Errors
    ///
    /// [`RelayError`] when a running relay could not be deleted, or the
    /// clear not counted. Not an error when none was running.
    fn clear(&self) -> Result<(), RelayError>;
}

/// Why the relay could not be reached
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayError {
    /// The relay session could not be started, with the reason
    CannotStart(String),
    /// The relay was started but never appeared in `claude agents --json --all`
    NeverAppeared,
    /// Its messaging socket could not be reached, with the reason
    Unreachable(String),
    /// A running relay could not be stopped, with the reason
    CannotStop(String),
    /// The count of clears could not be read or added to, with the reason
    CannotCount(String),
}

impl fmt::Display for RelayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CannotStart(reason) => write!(f, "cannot start the relay: {reason}"),
            Self::NeverAppeared => f.write_str("the relay never appeared after starting"),
            Self::Unreachable(reason) => write!(f, "cannot reach the relay: {reason}"),
            Self::CannotStop(reason) => write!(f, "cannot stop the relay: {reason}"),
            Self::CannotCount(reason) => write!(f, "cannot count the relay's clears: {reason}"),
        }
    }
}

impl core::error::Error for RelayError {}
