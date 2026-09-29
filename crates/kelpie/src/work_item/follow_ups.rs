//! Confirmed findings a merged pull request left unfixed

use serde::{Deserialize, Serialize};

use crate::ports::{Finding, Timestamp};

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
    /// When the forge first refused to take them, since it last took one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_refused: Option<Timestamp>,
}
