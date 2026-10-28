//! The agent board's pick for a work item, asked at the dog's door
//!
//! The dog keeps one board for every runner, so a runner asks it which agents
//! may start an item, in the order to try them. A runner that cannot reach
//! the dog, or cannot read its answer, gets none and starts nothing.

use std::path::Path;

use super::door::{Answer, Ask, PICK, Pick, PickAsk};
use super::door_pace::answered;

/// The agents the dog at `door` would start `item` on, in order, or None when it cannot say
///
/// An empty list is the dog's answer that no allowed agent fits.
pub fn pick(door: &Path, item: &PickAsk) -> Option<Vec<Pick>> {
    let ask = Ask {
        pick: Some(item.clone()),
        ..Ask::new(PICK, std::process::id(), "pick")
    };
    match answered(door, &ask)? {
        Answer::Picks(picks) => Some(picks),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AgentName;

    #[test]
    fn a_runner_that_cannot_reach_the_dog_gets_no_pick() {
        let item = PickAsk {
            kind: None,
            tier: None,
            pin: None,
            slack: None,
            agents: vec![AgentName::kelpies("sonnet-high")],
            leave_out: Vec::new(),
        };
        let nobody = Path::new("/nonexistent/kelpie/lease.sock");

        assert_eq!(pick(nobody, &item), None);
    }
}
