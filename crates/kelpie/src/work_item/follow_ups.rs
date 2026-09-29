//! Confirmed findings a merged pull request left unfixed

use serde::{Deserialize, Serialize};

use crate::ports::Finding;

/// Confirmed findings a merged pull request left unfixed, waiting to be filed
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowUps {
    /// The findings still to file
    pub findings: Vec<Finding>,
    /// Whether the maintainer said yes to filing them, or the project is on
    /// `auto` and nobody is asked
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ruled: bool,
    /// How many times the forge refused to take them
    #[serde(default, skip_serializing_if = "super::is_zero")]
    pub failures: u32,
}
