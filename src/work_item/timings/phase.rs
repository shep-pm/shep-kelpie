//! The phases a work item's wall time is charged to

use serde::{Deserialize, Serialize};

/// A bucket of a work item's wall time
// wire format: changing this is a breaking change to the state file and to `status`
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingPhase {
    /// A worker turn runs
    Worker,
    /// A local review round queues for the GPU
    GpuWait,
    /// A local review round runs, after any GPU wait
    LocalRound,
    /// A Claude review round runs
    ClaudeRound,
    /// A deep review round's reader runs
    DeepRound,
    /// Waiting for CI
    Ci,
    /// Waiting for the CodeRabbit lease and hourly window
    #[serde(rename = "coderabbit_window")]
    CodeRabbitWindow,
    /// Waiting for CodeRabbit's review after a summon
    #[serde(rename = "coderabbit_review")]
    CodeRabbitReview,
    /// Parked on a ruling for the maintainer
    Ruling,
    /// Merged and being cleaned up
    Merge,
    /// A shots run takes screenshots
    Shots,
    /// The project is paused and nothing is running for the work item
    Paused,
    /// Anything else: between steps, and while kelpie is not running
    Other,
}

impl TimingPhase {
    /// Every phase, in the order `status` and the table list them
    pub const ALL: [Self; 13] = [
        Self::Worker,
        Self::GpuWait,
        Self::LocalRound,
        Self::ClaudeRound,
        Self::DeepRound,
        Self::Ci,
        Self::CodeRabbitWindow,
        Self::CodeRabbitReview,
        Self::Ruling,
        Self::Merge,
        Self::Shots,
        Self::Paused,
        Self::Other,
    ];

    /// The name JSON and the table give it
    pub fn name(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::GpuWait => "gpu_wait",
            Self::LocalRound => "local_round",
            Self::ClaudeRound => "claude_round",
            Self::DeepRound => "deep_round",
            Self::Ci => "ci",
            Self::CodeRabbitWindow => "coderabbit_window",
            Self::CodeRabbitReview => "coderabbit_review",
            Self::Ruling => "ruling",
            Self::Merge => "merge",
            Self::Shots => "shots",
            Self::Paused => "paused",
            Self::Other => "other",
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_phases_keep_their_wire_names_in_table_order() {
        let names: Vec<_> = TimingPhase::ALL.iter().map(|p| p.name()).collect();
        assert_eq!(
            names,
            [
                "worker",
                "gpu_wait",
                "local_round",
                "claude_round",
                "deep_round",
                "ci",
                "coderabbit_window",
                "coderabbit_review",
                "ruling",
                "merge",
                "shots",
                "paused",
                "other",
            ]
        );
        for phase in TimingPhase::ALL {
            assert_eq!(serde_json::to_value(phase).unwrap(), json!(phase.name()));
        }
    }
}
