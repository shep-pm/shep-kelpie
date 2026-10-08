//! The local round's port: the trait a round runs through, and why one fails

use std::fmt;

use super::{Finding, ModelSeat};
use crate::settings::LocalRound;
use crate::worktree::Linked;

/// Whether a local round waits for the GPU or runs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundStage {
    /// The round queues for the GPU
    Queued,
    /// The round runs, or no longer queues
    Running,
}

/// Runs one local round, of the kind the project's settings choose
pub trait Reviewer: Send + Sync {
    /// Checks, as the runner starts, that `local` can run: its command is
    /// there, or its endpoint answers
    ///
    /// # Errors
    ///
    /// Why it cannot, naming the command or the endpoint.
    fn check(&self, local: &LocalRound) -> Result<(), String>;

    /// Runs `local` for round `round` against `worktree`'s diff from `base`,
    /// usually `origin/main`, writing its findings under `out`
    ///
    /// `criteria` is what the issue asks for, which the round checks the
    /// diff against, as its prompt or a file the command is pointed at.
    /// The worker writes `worktree`, so git is run on it only as checked
    /// against its repo.
    ///
    /// # Errors
    ///
    /// [`ReviewerError`] when the round cannot be run or did not finish,
    /// or when `worktree`'s git dirs are not its repo's.
    fn round(
        &self,
        local: &LocalRound,
        worktree: Linked<'_>,
        base: &str,
        out: &std::path::Path,
        round: u32,
        criteria: &str,
    ) -> Result<Vec<Finding>, ReviewerError>;

    /// Runs `local` as [`Reviewer::round`] does, telling `watch` when the
    /// round starts or stops queueing for the GPU
    ///
    /// A round that never queues never calls `watch`, which is what this
    /// default does.
    ///
    /// # Errors
    ///
    /// As [`Reviewer::round`].
    #[expect(clippy::too_many_arguments, reason = "`round`'s own, and the watch")]
    fn round_watched(
        &self,
        local: &LocalRound,
        worktree: Linked<'_>,
        base: &str,
        out: &std::path::Path,
        round: u32,
        criteria: &str,
        _watch: &(dyn Fn(RoundStage) + Sync),
    ) -> Result<Vec<Finding>, ReviewerError> {
        self.round(local, worktree, base, out, round, criteria)
    }

    /// Where the local model sat when a round last looked, for `status`
    ///
    /// None until a round has read it, and where there is nothing to read:
    /// no Ollama host in the settings, or a server with no `/api/ps`.
    fn seat(&self) -> Option<ModelSeat> {
        None
    }
}

/// Why a local round did not produce findings
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewerError {
    /// The command could not be started, with the OS's reason
    Spawn(String),
    /// The command ran and exited unsuccessfully, with this on stderr
    Failed(String),
    /// The command exited successfully but left no completion marker
    Incomplete,
    /// The round was ended because the runner is stopping
    Stopped,
    /// The endpoint answered with something other than a chat completion
    Unreadable(String),
    /// The model sits partly or wholly on the CPU, so the round was not run
    Spilled(String),
}

impl fmt::Display for ReviewerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run the local round: {error}"),
            Self::Failed(stderr) => write!(f, "the local round failed: {}", stderr.trim()),
            Self::Incomplete => f.write_str("the local round left no completion marker"),
            Self::Stopped => f.write_str("the local round was stopped with the runner"),
            Self::Unreadable(reply) => write!(f, "the local round's reply is unreadable: {reply}"),
            Self::Spilled(reason) => write!(f, "the local round did not run: {reason}"),
        }
    }
}

impl core::error::Error for ReviewerError {}
