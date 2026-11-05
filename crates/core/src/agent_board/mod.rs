//! The agent board's estimate: when each agent would finish a work item, and which to pick
//!
//! [`History`] reads an agent's typical time and cost from the ledger's merged
//! items, [`wait`] reads how long its model's load or its plan's hold keeps it
//! from starting, [`pick`] orders the agents that fit an item, and [`fit`]
//! reads which agents do clearly worse at a kind than another that takes it.
//! Every duration is in seconds but a slack, in minutes.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::settings::AgentName;

mod fit;
mod pick;
mod record;
mod typical;
mod wait;

pub use fit::{FIT_AGAIN, FIT_ITEMS, Finding, Measure, Taker, fit};
pub use pick::{Candidate, Choice, Item, pick, starts_within_slack, within_slack};
pub use record::{HEAVY_FIX_TURNS, Record, records};
pub use typical::{DEFAULT_SECS, History, SAMPLES, Typical};
pub use wait::{Availability, wait};

/// The maintainer's say in the board's order, in `[kelpie.agent_board]`
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentBoardSettings {
    /// Agents in the order the board prefers them among those that finish
    /// within the slack, first most wanted. Empty ranks none.
    #[serde(default)]
    pub prefer: Vec<AgentName>,
}
