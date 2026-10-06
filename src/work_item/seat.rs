//! Whether a work item holds one of the project's slots

use serde::{Deserialize, Serialize};

use super::{Phase, WorkItem};

/// Whether a work item holds a slot
///
/// A slot bounds model calls: a worker's turn or a review. An item parked
/// on a ruling gives its slot up, and goes on without one through a merge,
/// CI or its end. Entering a phase that calls a model again, it waits for a
/// slot ahead of any new work item.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Seat {
    /// It holds a slot
    #[default]
    Held,
    /// It holds none, in a phase that calls no model
    Without,
    /// It holds none, and waits for one to call a model
    Waiting,
}

impl Seat {
    /// Whether it holds a slot, as a state file leaves unsaid
    pub fn is_held(&self) -> bool {
        *self == Self::Held
    }
}

impl WorkItem {
    /// Whether it is parked on a ruling, which holds no slot
    pub fn parked(&self) -> bool {
        matches!(self.phase, Phase::Ruling { .. })
    }

    /// Whether its phase calls a model, a worker's turn or a review's, and
    /// so needs a slot
    pub fn calls_a_model(&self) -> bool {
        matches!(self.phase, Phase::Implement | Phase::Review(_))
    }
}
