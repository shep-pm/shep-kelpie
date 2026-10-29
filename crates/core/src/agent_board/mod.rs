//! The agent board's estimate: when each agent would finish a work item, and which to pick
//!
//! [`History`] reads an agent's typical time and cost from the ledger's merged
//! items, [`wait`] reads how long its model's load or its plan's hold keeps it
//! from starting, and [`pick`] orders the agents that fit an item. Every
//! duration is in seconds but a slack, in minutes.

mod pick;
mod record;
mod typical;
mod wait;

pub use pick::{Candidate, Choice, Item, pick, within_slack};
pub use record::{HEAVY_FIX_TURNS, Record, records};
pub use typical::{DEFAULT_SECS, History, SAMPLES, Typical};
pub use wait::{Availability, wait};
