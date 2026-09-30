//! The rig's local round: scripted rounds, and where its model sits

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::adapters::LocalReviewer;
use crate::ports::{Finding, ModelSeat, Reviewer, ReviewerError};
use crate::settings::LocalRound;

/// What the stand-in reviewer answers for its next round
#[derive(Debug, Clone)]
pub(crate) enum ScriptedRound {
    /// These findings
    Findings(Vec<Finding>),
    /// Fails with this error
    Fail(ReviewerError),
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
    seat: Arc<Mutex<Option<ModelSeat>>>,
}

impl FakeReviewer {
    /// Every round asked of it, in order
    pub(crate) fn seen(&self) -> Vec<SeenRound> {
        self.seen.lock().unwrap().clone()
    }

    /// Runs every later round with the real reviewer, after noting it
    pub(crate) fn pass_through(&self) {
        *self.real.lock().unwrap() = Some(LocalReviewer::default());
    }

    /// Where its model sat, for `status`, as the last round's look at it
    pub(crate) fn set_seat(&self, seat: Option<ModelSeat>) {
        *self.seat.lock().unwrap() = seat;
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
    ) -> Result<Vec<Finding>, ReviewerError> {
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
        match self.script.lock().unwrap().pop_front() {
            Some(ScriptedRound::Findings(findings)) => Ok(findings),
            Some(ScriptedRound::Fail(e)) => Err(e),
            None => Ok(Vec::new()),
        }
    }

    fn seat(&self) -> Option<ModelSeat> {
        self.seat.lock().unwrap().clone()
    }
}
