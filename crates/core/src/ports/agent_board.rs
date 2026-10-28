//! The agent board, as a runner asks it which agent starts a work item
//!
//! The dog keeps the one board every runner asks (`crate::lease::door_pick`).
//! A runner asks it through this port, so a test ranks the agents in memory.

use std::fmt;

use crate::lease::door::{Pick, PickAsk};

/// The board a runner asks, at dispatch, which agents may start a work item
pub trait AgentBoard: Send + Sync + fmt::Debug {
    /// The agents that may start `item`, best first, or None when the board cannot be asked
    ///
    /// An empty list is the board's answer that no allowed agent fits.
    fn pick(&self, item: &PickAsk) -> Option<Vec<Pick>>;
}
