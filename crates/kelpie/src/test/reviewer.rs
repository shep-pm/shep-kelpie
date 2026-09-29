//! The rig's local round: a reviewer that answers from a script

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::adapters::LocalReviewer;
use crate::ports::{Finding, LocalRun, Reviewer, ReviewerError};
use crate::settings::LocalRound;
use crate::test::FakeClock;

/// What the stand-in reviewer answers for its next round
#[derive(Debug, Clone)]
pub(crate) enum ScriptedRound {
    /// These findings
    Findings(Vec<Finding>),
    /// Fails with this error
    Fail(ReviewerError),
    /// These findings, after queueing `gpu_wait_seconds` for the GPU in a
    /// round that took `took_seconds` on the rig's clock
    Waited {
        findings: Vec<Finding>,
        gpu_wait_seconds: u64,
        took_seconds: u64,
    },
    /// Fails with this error, after the same wait and time
    FailedWaiting {
        error: ReviewerError,
        gpu_wait_seconds: u64,
        took_seconds: u64,
    },
}

/// One round as the stand-in reviewer saw it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeenRound {
    pub(crate) local: LocalRound,
    pub(crate) worktree: PathBuf,
    pub(crate) base: String,
    pub(crate) out: PathBuf,
    pub(crate) round: u32,
}

/// A local round's stand-in. Clean (no findings) once its script runs out, so
/// tests that do not care about the review loop see it pass straight through.
/// Its start check is the real one, and [`Self::pass_through`] makes its
/// rounds real too.
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeReviewer {
    seen: Arc<Mutex<Vec<SeenRound>>>,
    script: Arc<Mutex<VecDeque<ScriptedRound>>>,
    real: Arc<Mutex<Option<LocalReviewer>>>,
    clock: Option<FakeClock>,
}

impl FakeReviewer {
    /// A stand-in whose scripted rounds that take time move `clock`
    pub(crate) fn on(clock: FakeClock) -> Self {
        Self {
            clock: Some(clock),
            ..Self::default()
        }
    }

    /// Every round asked of it, in order
    pub(crate) fn seen(&self) -> Vec<SeenRound> {
        self.seen.lock().unwrap().clone()
    }

    /// Runs every later round with the real reviewer, after noting it
    pub(crate) fn pass_through(&self) {
        *self.real.lock().unwrap() = Some(LocalReviewer::default());
    }

    /// Queues answers for its next rounds, oldest first
    pub(crate) fn script(&self, rounds: impl IntoIterator<Item = ScriptedRound>) {
        self.script.lock().unwrap().extend(rounds);
    }
}

impl Reviewer for FakeReviewer {
    fn check(&self, local: &LocalRound) -> Result<(), String> {
        LocalReviewer::default().check(local)
    }

    fn round(
        &self,
        local: &LocalRound,
        worktree: &Path,
        base: &str,
        out: &Path,
        round: u32,
    ) -> LocalRun {
        self.seen.lock().unwrap().push(SeenRound {
            local: local.clone(),
            worktree: worktree.to_owned(),
            base: base.to_owned(),
            out: out.to_owned(),
            round,
        });
        if let Some(real) = &*self.real.lock().unwrap() {
            return real.round(local, worktree, base, out, round);
        }
        let (result, gpu_wait_seconds, took_seconds) = match self.script.lock().unwrap().pop_front()
        {
            Some(ScriptedRound::Findings(findings)) => (Ok(findings), 0, 0),
            Some(ScriptedRound::Fail(e)) => (Err(e), 0, 0),
            Some(ScriptedRound::Waited {
                findings,
                gpu_wait_seconds,
                took_seconds,
            }) => (Ok(findings), gpu_wait_seconds, took_seconds),
            Some(ScriptedRound::FailedWaiting {
                error,
                gpu_wait_seconds,
                took_seconds,
            }) => (Err(error), gpu_wait_seconds, took_seconds),
            None => (Ok(Vec::new()), 0, 0),
        };
        if let Some(clock) = &self.clock {
            clock.advance(took_seconds);
        }
        LocalRun {
            result,
            gpu_wait_seconds,
        }
    }
}
