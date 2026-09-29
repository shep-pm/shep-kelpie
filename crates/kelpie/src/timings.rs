//! Where a work item's time goes
//!
//! A work item is always in exactly one [`Bucket`], which its state decides.
//! Every save of the state file closes the stretch since the last one into
//! the bucket the item was in, so a work item's buckets add up to its wall
//! time by construction. Only the GPU wait is carved out afterwards, from
//! the local round the reviewer reports it for.

use serde::{Deserialize, Serialize};

use crate::ports::Timestamp;
use crate::work_item::{CodeRabbitStage, Phase, ReviewCallState, ReviewStage, ReviewerKind};
use crate::work_item::{Turn, WorkItem};

mod table;

pub use table::{Report, Row};

/// Finished work items whose split the state file keeps, oldest dropped first
pub const HISTORY_KEPT: usize = 200;

/// Where a work item spends a stretch of its time
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    /// The worker's turns
    Worker,
    /// A local round waiting for the GPU
    GpuWait,
    /// A local round running
    LocalRound,
    /// A Claude review round
    ClaudeRound,
    /// The judge, on a review round's findings or CodeRabbit's threads
    Judging,
    /// Waiting for CI
    Ci,
    /// Waiting for the CodeRabbit lease and its hourly window
    #[serde(rename = "coderabbit_window")]
    CodeRabbitWindow,
    /// Summoned, waiting for CodeRabbit to review
    #[serde(rename = "coderabbit_review")]
    CodeRabbitReview,
    /// Parked on the maintainer's ruling
    Ruling,
    /// Merging, and cleaning up after
    Merge,
    /// Between steps: waiting for the project to run, the pacer, or kelpie's
    /// own shots and catch-up with `main`
    Idle,
}

impl Bucket {
    /// Every bucket, in the order a table shows them
    pub const ALL: [Bucket; 11] = [
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
        Self::Idle,
    ];
}

/// Seconds spent in each bucket
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Split {
    /// The worker's turns
    pub worker: u64,
    /// A local round waiting for the GPU
    pub gpu_wait: u64,
    /// A local round running
    pub local_round: u64,
    /// A Claude review round
    pub claude_round: u64,
    /// The judge
    pub judging: u64,
    /// Waiting for CI
    pub ci: u64,
    /// Waiting for CodeRabbit's window
    pub coderabbit_window: u64,
    /// CodeRabbit reviewing
    pub coderabbit_review: u64,
    /// Parked on a ruling
    pub ruling: u64,
    /// Merging
    pub merge: u64,
    /// Between steps
    pub idle: u64,
}

impl Split {
    /// Seconds in `bucket`
    pub fn get(&self, bucket: Bucket) -> u64 {
        *self.slot(bucket)
    }

    /// Adds `seconds` to `bucket`
    pub fn add(&mut self, bucket: Bucket, seconds: u64) {
        *self.slot_mut(bucket) += seconds;
    }

    /// Seconds in every bucket together
    pub fn total(&self) -> u64 {
        Bucket::ALL.iter().map(|b| self.get(*b)).sum()
    }

    /// This split and `other` added bucket by bucket
    pub fn plus(mut self, other: &Split) -> Split {
        for bucket in Bucket::ALL {
            self.add(bucket, other.get(bucket));
        }
        self
    }

    fn slot(&self, bucket: Bucket) -> &u64 {
        match bucket {
            Bucket::Worker => &self.worker,
            Bucket::GpuWait => &self.gpu_wait,
            Bucket::LocalRound => &self.local_round,
            Bucket::ClaudeRound => &self.claude_round,
            Bucket::Judging => &self.judging,
            Bucket::Ci => &self.ci,
            Bucket::CodeRabbitWindow => &self.coderabbit_window,
            Bucket::CodeRabbitReview => &self.coderabbit_review,
            Bucket::Ruling => &self.ruling,
            Bucket::Merge => &self.merge,
            Bucket::Idle => &self.idle,
        }
    }

    fn slot_mut(&mut self, bucket: Bucket) -> &mut u64 {
        match bucket {
            Bucket::Worker => &mut self.worker,
            Bucket::GpuWait => &mut self.gpu_wait,
            Bucket::LocalRound => &mut self.local_round,
            Bucket::ClaudeRound => &mut self.claude_round,
            Bucket::Judging => &mut self.judging,
            Bucket::Ci => &mut self.ci,
            Bucket::CodeRabbitWindow => &mut self.coderabbit_window,
            Bucket::CodeRabbitReview => &mut self.coderabbit_review,
            Bucket::Ruling => &mut self.ruling,
            Bucket::Merge => &mut self.merge,
            Bucket::Idle => &mut self.idle,
        }
    }
}

/// A work item's clock: when it began, and the split so far
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timings {
    /// When the clock began
    pub started: Timestamp,
    // The last save, and the bucket the item was in from then on
    since: Timestamp,
    bucket: Bucket,
    split: Split,
}

impl Timings {
    /// A clock that begins at `now`, in `bucket`
    pub fn begin(now: Timestamp, bucket: Bucket) -> Self {
        Self {
            started: now,
            since: now,
            bucket,
            split: Split::default(),
        }
    }

    /// Closes the stretch since the last tick into the bucket it was spent
    /// in, and carries on in `bucket`
    pub fn tick(&mut self, now: Timestamp, bucket: Bucket) {
        let now = now.0.max(self.since.0);
        self.split.add(self.bucket, now - self.since.0);
        self.since = Timestamp(now);
        self.bucket = bucket;
    }

    /// The split as of `now`, the running stretch included
    pub fn as_of(&self, now: Timestamp) -> Split {
        let mut clock = self.clone();
        clock.tick(now, self.bucket);
        clock.split
    }

    /// Moves up to `seconds` of the local round's time to the GPU wait, as
    /// of `now`
    pub fn gpu_waited(&mut self, now: Timestamp, seconds: u64) {
        self.tick(now, self.bucket);
        let moved = seconds.min(self.split.local_round);
        self.split.local_round -= moved;
        self.split.gpu_wait += moved;
    }

    /// Ends the clock at `now`, and returns what it kept
    pub fn close(mut self, now: Timestamp) -> Split {
        self.tick(now, self.bucket);
        self.split
    }
}

/// A finished work item's split, as the run history keeps it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finished {
    /// The work item's issue
    pub issue: u64,
    /// Its pull request, if it had one
    pub pull_request: Option<u64>,
    /// Whether the pull request merged
    pub merged: bool,
    /// When its clock began
    pub started: Timestamp,
    /// When it finished
    pub ended: Timestamp,
    /// Where its time went
    pub split: Split,
}

impl Finished {
    /// Seconds from its first save to its finish
    pub fn wall(&self) -> u64 {
        self.ended.0.saturating_sub(self.started.0)
    }
}

impl WorkItem {
    /// The bucket its state puts it in
    ///
    /// `local` is whether the project runs a local round, which decides who
    /// reviews an odd round.
    pub fn bucket(&self, local: bool) -> Bucket {
        match (&self.turn, &self.phase) {
            (Turn::Running { .. }, _) => Bucket::Worker,
            (Turn::Failed { .. }, _) | (_, Phase::Ruling { .. }) => Bucket::Ruling,
            (_, Phase::Implement) => Bucket::Idle,
            (_, Phase::Review(review)) => match (&review.stage, self.review_call) {
                (ReviewStage::Judging { .. }, _) => Bucket::Judging,
                (ReviewStage::Fixing { .. }, _) | (ReviewStage::Round, ReviewCallState::Idle) => {
                    Bucket::Idle
                }
                (ReviewStage::Round, ReviewCallState::Running { .. }) => {
                    match review.reviewer(local) {
                        ReviewerKind::Local => Bucket::LocalRound,
                        ReviewerKind::Claude => Bucket::ClaudeRound,
                    }
                }
            },
            (_, Phase::Ci { .. }) => Bucket::Ci,
            (_, Phase::CodeRabbit(stage)) => match stage {
                CodeRabbitStage::Lease { .. } => Bucket::CodeRabbitWindow,
                CodeRabbitStage::Summoned { .. } => Bucket::CodeRabbitReview,
                CodeRabbitStage::Judging { .. } => Bucket::Judging,
                CodeRabbitStage::Fixing { .. } => Bucket::Idle,
            },
            (_, Phase::Merge { .. } | Phase::Done { .. }) => Bucket::Merge,
        }
    }

    /// Closes the stretch since the last save, then carries on in the
    /// bucket its state now puts it in
    pub fn clock(&mut self, now: Timestamp, local: bool) {
        let bucket = self.bucket(local);
        match &mut self.timings {
            Some(timings) => timings.tick(now, bucket),
            None => self.timings = Some(Timings::begin(now, bucket)),
        }
    }

    /// Its split as of `now`, if its clock has begun
    pub fn split(&self, now: Timestamp) -> Option<Split> {
        self.timings.as_ref().map(|t| t.as_of(now))
    }

    /// Ends its clock at `now` into the run history's record, if it began
    pub fn finished(&self, now: Timestamp, merged: bool) -> Option<Finished> {
        let timings = self.timings.clone()?;
        let started = timings.started;
        let split = timings.close(now);
        Some(Finished {
            issue: self.issue,
            pull_request: self.pull_request,
            merged,
            started,
            ended: Timestamp(started.0 + split.total()),
            split,
        })
    }
}

#[cfg(test)]
mod scenarios;
#[cfg(test)]
mod tests;
