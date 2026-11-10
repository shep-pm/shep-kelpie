//! The package fetcher port: a work item's pinned packages, fetched into the shared cache
//!
//! A worker's sandbox may not write the shared cargo cache, so a pinned crate the cache lacks
//! fails its build and the worker copies the whole registry. The fetch runs on the host before
//! the turn, outside any sandbox, so the cache holds every crate the lockfile pins.

use std::path::PathBuf;
use std::time::Duration;

/// One `cargo fetch` to run
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoFetch {
    /// A `Cargo.toml`, with its `Cargo.lock` beside it
    pub manifest: PathBuf,
    /// The folder it runs in: one no worker may write, so it holds nothing a worker put there
    ///
    /// Cargo and rustup read config and toolchain files from the folder they run in and run what
    /// those name, outside any sandbox. A worker can write its worktree and its build folder.
    pub cwd: PathBuf,
    /// How long it may run before it is killed
    pub ceiling: Duration,
}

/// How a fetch ended
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Every crate the lockfile pins is in the cache
    Fetched,
    /// It exited with a failure
    Failed {
        /// The last [`OUTPUT_KEPT`](super::OUTPUT_KEPT) bytes of what it wrote, cut by [`tail`](super::tail)
        output: String,
    },
    /// It ran past its ceiling and was killed
    TimedOut,
    /// It never ran to a result: the command could not start, or the runner is stopping
    NotRun {
        /// Why
        reason: String,
    },
}

/// Fetches a project's pinned packages on the host
pub trait PackageFetcher: Send + Sync {
    /// Runs `run` to its end, or to its ceiling
    ///
    /// Never fails: a fetch that could not happen is [`FetchOutcome::NotRun`].
    fn cargo_fetch(&self, run: &CargoFetch) -> FetchOutcome;
}
