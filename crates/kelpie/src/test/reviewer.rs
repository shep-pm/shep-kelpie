//! The rig's local round: every round noted, each scripted or run for real

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::FakeClock;
use crate::adapters::LocalReviewer;
use crate::ports::{Finding, Reviewer, ReviewerError, Round};
use crate::settings::LocalRound;

/// What the stand-in reviewer answers for its next round
#[derive(Debug, Clone)]
pub(crate) enum ScriptedRound {
    /// These findings
    Findings(Vec<Finding>),
    /// Fails with this error
    Fail(ReviewerError),
    /// These findings, after the rig's clock has run `gpu_wait` seconds
    /// queued for the GPU and `run` seconds running
    Slow {
        gpu_wait: u64,
        run: u64,
        findings: Vec<Finding>,
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
/// rounds real too. A [`ScriptedRound::Slow`] round moves `clock`, the rig's.
#[derive(Debug, Clone)]
pub(crate) struct FakeReviewer {
    clock: FakeClock,
    seen: Arc<Mutex<Vec<SeenRound>>>,
    script: Arc<Mutex<VecDeque<ScriptedRound>>>,
    real: Arc<Mutex<Option<LocalReviewer>>>,
}

impl FakeReviewer {
    pub(crate) fn on(clock: FakeClock) -> Self {
        Self {
            clock,
            seen: Arc::default(),
            script: Arc::default(),
            real: Arc::default(),
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
    ) -> Result<Round, ReviewerError> {
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
        let quick = |findings| Round {
            findings,
            gpu_wait: Duration::ZERO,
        };
        match self.script.lock().unwrap().pop_front() {
            Some(ScriptedRound::Findings(findings)) => Ok(quick(findings)),
            Some(ScriptedRound::Fail(e)) => Err(e),
            Some(ScriptedRound::Slow {
                gpu_wait,
                run,
                findings,
            }) => {
                self.clock.advance(gpu_wait + run);
                Ok(Round {
                    findings,
                    gpu_wait: Duration::from_secs(gpu_wait),
                })
            }
            None => Ok(Round::default()),
        }
    }
}
