//! The phases a work item's wall time is charged to

use serde::{Deserialize, Serialize};

/// A bucket of a work item's wall time
// wire format: changing this is a breaking change to the state file and to `status`
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingPhase {
    /// A worker turn runs
    Worker,
    /// A review runs: a round queued for the GPU or running on any harness,
    /// and a review bot's lease, window and review
    Review,
    /// Waiting for CI
    Ci,
    /// Parked on a ruling for the maintainer
    Ruling,
    /// Merged and being cleaned up
    Merge,
    /// Anything else: between steps, while the project is paused and while
    /// kelpie is not running
    Other,
}

impl TimingPhase {
    /// Every phase, in the order `status` and the table list them
    pub const ALL: [Self; 6] = [
        Self::Worker,
        Self::Review,
        Self::Ci,
        Self::Ruling,
        Self::Merge,
        Self::Other,
    ];

    /// The name JSON and the table give it
    pub fn name(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Review => "review",
            Self::Ci => "ci",
            Self::Ruling => "ruling",
            Self::Merge => "merge",
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
            ["worker", "review", "ci", "ruling", "merge", "other"]
        );
        for phase in TimingPhase::ALL {
            assert_eq!(serde_json::to_value(phase).unwrap(), json!(phase.name()));
        }
    }
}
