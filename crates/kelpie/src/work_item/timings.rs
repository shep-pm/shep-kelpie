//! Where a work item's wall time goes, by phase
//!
//! Time is charged at each save to the phase the previous state gave the
//! item, so the phases sum to the wall time by construction.

use std::ops::AddAssign;

use serde::{Deserialize, Serialize};

use super::{CodeRabbitStage, Phase, ReviewCallKind, ReviewCallState, ReviewStage, Turn, WorkItem};
use crate::ports::Timestamp;

/// Where a stretch of a work item's time went
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingPhase {
    /// The worker's turn was running
    Worker,
    /// A local round waited for the GPU
    GpuWait,
    /// A local round ran
    LocalRound,
    /// A Claude review round ran
    ClaudeRound,
    /// The judge ran
    Judging,
    /// Waiting for CI
    Ci,
    /// Waiting for CodeRabbit's hourly window, the ready-state settle included
    #[serde(rename = "coderabbit_window")]
    CodeRabbitWindow,
    /// Summoned, waiting for CodeRabbit's review
    #[serde(rename = "coderabbit_review")]
    CodeRabbitReview,
    /// Parked on a ruling
    Ruling,
    /// Merging and cleaning up
    Merge,
    /// A shots run
    Shots,
    /// A turn or round waiting to start, and the moments between steps
    Other,
}

impl TimingPhase {
    /// Every phase, in the order `PhaseSeconds` keeps them
    pub const ALL: [TimingPhase; 12] = [
        Self::Worker,
        Self::GpuWait,
        Self::LocalRound,
        Self::ClaudeRound,
        Self::Judging,
        Self::Ci,
        Self::CodeRabbitWindow,
        Self::CodeRabbitReview,
        Self::Ruling,
        Self::Merge,
        Self::Shots,
        Self::Other,
    ];

    /// The phase's key in the state file
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::GpuWait => "gpu_wait",
            Self::LocalRound => "local_round",
            Self::ClaudeRound => "claude_round",
            Self::Judging => "judging",
            Self::Ci => "ci",
            Self::CodeRabbitWindow => "coderabbit_window",
            Self::CodeRabbitReview => "coderabbit_review",
            Self::Ruling => "ruling",
            Self::Merge => "merge",
            Self::Shots => "shots",
            Self::Other => "other",
        }
    }
}

/// Seconds spent in each [`TimingPhase`]
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhaseSeconds {
    /// The worker's turns
    pub worker: u64,
    /// Local rounds waiting for the GPU
    pub gpu_wait: u64,
    /// Local rounds running
    pub local_round: u64,
    /// Claude review rounds
    pub claude_round: u64,
    /// The judge's calls
    pub judging: u64,
    /// Waiting for CI
    pub ci: u64,
    /// Waiting for CodeRabbit's hourly window
    pub coderabbit_window: u64,
    /// Waiting for CodeRabbit's review after a summon
    pub coderabbit_review: u64,
    /// Parked on rulings
    pub ruling: u64,
    /// Merging and cleaning up
    pub merge: u64,
    /// Shots runs
    pub shots: u64,
    /// Turns and rounds waiting to start, and the moments between steps
    pub other: u64,
}

impl PhaseSeconds {
    /// The seconds in `phase`
    pub fn get(&self, phase: TimingPhase) -> u64 {
        *self.slot(phase)
    }

    /// Adds `seconds` to `phase`
    pub fn add(&mut self, phase: TimingPhase, seconds: u64) {
        let slot = self.slot_mut(phase);
        *slot = slot.saturating_add(seconds);
    }

    /// The seconds in every phase together
    pub fn total(&self) -> u64 {
        TimingPhase::ALL.iter().map(|&p| self.get(p)).sum()
    }

    fn slot(&self, phase: TimingPhase) -> &u64 {
        match phase {
            TimingPhase::Worker => &self.worker,
            TimingPhase::GpuWait => &self.gpu_wait,
            TimingPhase::LocalRound => &self.local_round,
            TimingPhase::ClaudeRound => &self.claude_round,
            TimingPhase::Judging => &self.judging,
            TimingPhase::Ci => &self.ci,
            TimingPhase::CodeRabbitWindow => &self.coderabbit_window,
            TimingPhase::CodeRabbitReview => &self.coderabbit_review,
            TimingPhase::Ruling => &self.ruling,
            TimingPhase::Merge => &self.merge,
            TimingPhase::Shots => &self.shots,
            TimingPhase::Other => &self.other,
        }
    }

    fn slot_mut(&mut self, phase: TimingPhase) -> &mut u64 {
        match phase {
            TimingPhase::Worker => &mut self.worker,
            TimingPhase::GpuWait => &mut self.gpu_wait,
            TimingPhase::LocalRound => &mut self.local_round,
            TimingPhase::ClaudeRound => &mut self.claude_round,
            TimingPhase::Judging => &mut self.judging,
            TimingPhase::Ci => &mut self.ci,
            TimingPhase::CodeRabbitWindow => &mut self.coderabbit_window,
            TimingPhase::CodeRabbitReview => &mut self.coderabbit_review,
            TimingPhase::Ruling => &mut self.ruling,
            TimingPhase::Merge => &mut self.merge,
            TimingPhase::Shots => &mut self.shots,
            TimingPhase::Other => &mut self.other,
        }
    }
}

impl AddAssign for PhaseSeconds {
    fn add_assign(&mut self, other: Self) {
        for phase in TimingPhase::ALL {
            self.add(phase, other.get(phase));
        }
    }
}

/// A work item's wall time since it started, split by phase
///
/// `seconds` always totals `charged - started`.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Timings {
    /// When counting began. None for a work item saved before timings
    /// existed, until its first charge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started: Option<Timestamp>,
    /// The time up to which `seconds` is charged
    #[serde(skip_serializing_if = "Option::is_none")]
    pub charged: Option<Timestamp>,
    /// Where the charged time went
    pub seconds: PhaseSeconds,
}

impl Timings {
    /// Timings that start counting at `now`
    pub fn starting(now: Timestamp) -> Self {
        Self {
            started: Some(now),
            charged: Some(now),
            seconds: PhaseSeconds::default(),
        }
    }

    /// Charges the time since the last charge to `phase`
    ///
    /// With no mark yet it only sets one. A clock that has gone back adds
    /// nothing and leaves the mark where it was.
    pub fn charge(&mut self, phase: TimingPhase, now: Timestamp) {
        let Some(charged) = self.charged else {
            self.started.get_or_insert(now);
            self.charged = Some(now);
            return;
        };
        if now > charged {
            self.seconds.add(phase, now.0 - charged.0);
            self.charged = Some(now);
        }
    }

    /// A copy charged to `now`, leaving this one as it is
    pub fn at(&self, phase: TimingPhase, now: Timestamp) -> Self {
        let mut charged = *self;
        charged.charge(phase, now);
        charged
    }

    /// Moves up to `seconds` from `from` to `to`, keeping the total
    pub fn reassign(&mut self, from: TimingPhase, to: TimingPhase, seconds: u64) {
        let moved = seconds.min(self.seconds.get(from));
        *self.seconds.slot_mut(from) -= moved;
        self.seconds.add(to, moved);
    }
}

impl WorkItem {
    /// The phase of its time it is in now
    pub fn timing_phase(&self) -> TimingPhase {
        if matches!(self.turn, Turn::Running { .. }) {
            return TimingPhase::Worker;
        }
        if let ReviewCallState::Running { kind, .. } = self.review_call {
            return match kind {
                Some(ReviewCallKind::Local) => TimingPhase::LocalRound,
                Some(ReviewCallKind::Claude) => TimingPhase::ClaudeRound,
                Some(ReviewCallKind::Judge) => TimingPhase::Judging,
                Some(ReviewCallKind::Shots) => TimingPhase::Shots,
                None => TimingPhase::Other,
            };
        }
        match &self.phase {
            Phase::Ci { .. } => TimingPhase::Ci,
            Phase::CodeRabbit(CodeRabbitStage::Lease { .. }) => TimingPhase::CodeRabbitWindow,
            Phase::CodeRabbit(CodeRabbitStage::Summoned { .. }) => TimingPhase::CodeRabbitReview,
            Phase::CodeRabbit(CodeRabbitStage::Judging { .. }) => TimingPhase::Judging,
            Phase::Review(review) if matches!(review.stage, ReviewStage::Judging { .. }) => {
                TimingPhase::Judging
            }
            Phase::Ruling { .. } => TimingPhase::Ruling,
            Phase::Merge { .. } | Phase::Done { .. } => TimingPhase::Merge,
            Phase::Implement
            | Phase::Review(_)
            | Phase::CodeRabbit(CodeRabbitStage::Fixing { .. }) => TimingPhase::Other,
        }
    }

    /// Its timings charged to `now`, by the phase it is in
    pub fn timings_at(&self, now: Timestamp) -> Timings {
        self.timings.at(self.timing_phase(), now)
    }

    /// Charges the time since its last charge to the phase it is in
    pub fn charge_time(&mut self, now: Timestamp) {
        let phase = self.timing_phase();
        self.timings.charge(phase, now);
    }
}

#[cfg(test)]
mod tests;
