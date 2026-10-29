//! The agent board, as a runner asks it which agent starts a work item
//!
//! The dog keeps the one board every runner asks (`crate::lease::door_pick`).
//! A runner asks it through this port, so a test ranks the agents in memory.

use std::fmt;

use crate::lease::door::{FitAsk, Fits, Pick, PickAsk};

/// The board a runner asks, at dispatch, which agents may start a work item
pub trait AgentBoard: Send + Sync + fmt::Debug {
    /// The agents that may start `item`, best first, or None when the board cannot be asked
    ///
    /// An empty list is the board's answer that no allowed agent fits.
    fn pick(&self, item: &PickAsk) -> Option<Vec<Pick>>;

    /// What the board says about the agent and kind `item` names, or None
    /// when it cannot be asked
    ///
    /// [`Fits::No`] is the finding a runner raises an `agent-fit` ruling from.
    fn fit(&self, item: &FitAsk) -> Option<Fits>;
}
