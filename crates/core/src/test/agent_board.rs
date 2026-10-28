//! The agent board stand-in: what each pick was asked, and what it answers
//!
//! It answers in memory, so a test of dispatch never reaches the dog. It
//! starts by ranking every agent an ask allows in the order the ask lists
//! them, with no wait. A test ranks its own order, ranks nobody, or makes
//! the board unreachable.

use std::sync::{Arc, Mutex};

use crate::lease::door::{Pick, PickAsk};
use crate::ports::AgentBoard;
use crate::settings::AgentName;

/// Seconds every ranked agent is said to take
const TYPICAL_SECS: u64 = 3600;

#[derive(Debug, Default)]
enum Answers {
    // The ask's own agents, in its order
    #[default]
    Listed,
    Ranks(Vec<AgentName>),
    Unreachable,
}

#[derive(Debug, Default)]
struct Book {
    answers: Answers,
    asks: Vec<PickAsk>,
}

/// A board that ranks what a test sets
#[derive(Debug, Clone, Default)]
pub struct FakeAgentBoard {
    book: Arc<Mutex<Book>>,
}

impl FakeAgentBoard {
    /// Each pick ranks `agents` in this order, those the ask allows and pins,
    /// and nobody when `agents` is empty
    pub fn ranks(&self, agents: &[&str]) {
        let name = |a: &&str| AgentName::try_from((*a).to_owned()).unwrap();
        let agents = agents.iter().map(name).collect();
        self.book.lock().unwrap().answers = Answers::Ranks(agents);
    }

    /// Each pick ranks every agent the ask allows, in its order
    pub fn ranks_listed(&self) {
        self.book.lock().unwrap().answers = Answers::Listed;
    }

    /// Each pick goes unanswered, as when the dog cannot be reached
    pub fn unreachable(&self) {
        self.book.lock().unwrap().answers = Answers::Unreachable;
    }

    /// What each pick was asked, in order
    pub fn asks(&self) -> Vec<PickAsk> {
        self.book.lock().unwrap().asks.clone()
    }
}

impl AgentBoard for FakeAgentBoard {
    fn pick(&self, item: &PickAsk) -> Option<Vec<Pick>> {
        let mut book = self.book.lock().unwrap();
        book.asks.push(item.clone());
        let order = match &book.answers {
            Answers::Listed => item.agents.clone(),
            Answers::Ranks(agents) => agents.clone(),
            Answers::Unreachable => return None,
        };
        let fits = |agent: &AgentName| {
            item.agents.contains(agent)
                && !item.leave_out.contains(agent)
                && item.pin.as_ref().is_none_or(|pin| pin == agent)
        };
        let picks = order.into_iter().filter(fits).map(|agent| Pick {
            agent,
            finish: TYPICAL_SECS,
            wait: 0,
            cost_usd: None,
        });
        Some(picks.collect())
    }
}
