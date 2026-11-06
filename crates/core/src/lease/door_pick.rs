//! The agent board's pick and fit, asked at the dog's door
//!
//! The dog keeps one board for every runner, so a runner asks it which agents
//! may start an item and whether an agent still fits a kind of work. The dog
//! reads and orders none: the runner ranks its answer. A runner that cannot
//! reach the dog, or cannot read its answer, gets none and starts or raises
//! nothing.

use std::path::{Path, PathBuf};

use super::door::{Answer, Ask, FIT, FitAsk, Fits, PICK, Pick, PickAsk, REFUSED};
use super::door_pace::answered;
use crate::agent_board::rank;
use crate::api_refusal::Refusal;
use crate::ports::{AgentBoard, Timestamp};
use crate::settings::AgentName;

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

    fn reading(&self, agents: &[AgentName]) -> Option<Vec<Pick>> {
        reading(&self.socket, agents)
    }

    fn fit(&self, item: &FitAsk) -> Option<Fits> {
        fits(&self.socket, item)
    }

    fn refused(&self, refusal: &Refusal) -> Option<Timestamp> {
        refused(&self.socket, refusal)
    }
}

/// The agents the dog at `door` would start `item` on, in order, or None when it cannot say
///
/// The dog reads each agent and the runner ranks them with the ask's slack.
/// An empty list is the dog's answer that no allowed agent fits.
pub fn pick(door: &Path, item: &PickAsk) -> Option<Vec<Pick>> {
    let ask = Ask {
        pick: Some(item.clone()),
        ..Ask::new(PICK, std::process::id(), "pick")
    };
    match answered(door, &ask)? {
        Answer::Picks(picks) => Some(rank(picks, item.slack)),
        _ => None,
    }
}

/// What the dog at `door` reads of each of `agents` it has a window for, in no order, or None when it cannot say
pub fn reading(door: &Path, agents: &[AgentName]) -> Option<Vec<Pick>> {
    let ask = Ask {
        reading: agents.to_vec(),
        ..Ask::new(PICK, std::process::id(), "reading")
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

/// Tells the dog at `door` the API refused its key, and when it holds the API until, or None when it cannot say
pub fn refused(door: &Path, refusal: &Refusal) -> Option<Timestamp> {
    let ask = Ask {
        refused: Some(*refusal),
        ..Ask::new(REFUSED, std::process::id(), "refused")
    };
    match answered(door, &ask)? {
        Answer::Held(until) => Some(Timestamp(until)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_refusal::Refusal;

    #[test]
    fn a_runner_that_cannot_reach_the_dog_gets_no_reading() {
        let agents = [AgentName::kelpies("sonnet-high")];
        let nobody = Path::new("/nonexistent/kelpie/lease.sock");

        assert_eq!(reading(nobody, &agents), None);
    }

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
    fn a_runner_that_cannot_reach_the_dog_reports_no_refusal() {
        let nobody = Path::new("/nonexistent/kelpie/lease.sock");

        assert_eq!(refused(nobody, &Refusal::NoCredit), None);
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
