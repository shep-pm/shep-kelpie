//! The agent board's pick and fit, asked at the dog's door
//!
//! The dog keeps one board for every runner, so a runner asks it which agents
//! may start an item, in the order to try them, and whether an agent still
//! fits a kind of work. A runner that cannot reach the dog, or cannot read its
//! answer, gets none and starts or raises nothing.

use std::path::{Path, PathBuf};

use super::door::{Answer, Ask, FIT, FitAsk, Fits, PICK, Pick, PickAsk};
use super::door_pace::answered;
use crate::ports::AgentBoard;

/// The agent board as the dog at a door keeps it
#[derive(Debug)]
pub struct DoorBoard {
    socket: PathBuf,
}

impl DoorBoard {
    /// The board the dog answers for at `socket`
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }
}

impl AgentBoard for DoorBoard {
    fn pick(&self, item: &PickAsk) -> Option<Vec<Pick>> {
        pick(&self.socket, item)
    }

    fn fit(&self, item: &FitAsk) -> Option<Fits> {
        fits(&self.socket, item)
    }
}

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

/// What the dog at `door` says about `item`, or None when it cannot say
pub fn fits(door: &Path, item: &FitAsk) -> Option<Fits> {
    let ask = Ask {
        fit: Some(item.clone()),
        ..Ask::new(FIT, std::process::id(), "fit")
    };
    match answered(door, &ask)? {
        Answer::Fits(fits) => Some(fits),
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

    #[test]
    fn a_runner_that_cannot_reach_the_dog_gets_no_fit_answer() {
        let item = FitAsk {
            agent: AgentName::kelpies("sonnet-high"),
            kind: "fix".into(),
        };
        let nobody = Path::new("/nonexistent/kelpie/lease.sock");

        assert_eq!(fits(nobody, &item), None);
    }
}
