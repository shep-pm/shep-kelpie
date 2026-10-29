//! The agent board stand-in: what each pick was asked, and what it answers
//!
//! It answers in memory, so a test of dispatch never reaches the dog. It
//! starts by ranking every agent an ask allows in the order the ask lists
//! them, with no wait, and by saying every agent fits every kind. A test puts
//! some of them first, ranks its own order, with its own finishes, ranks
//! nobody, says one agent does not fit a kind, or makes the board unreachable.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::agent_board::Finding;
use crate::lease::door::{FitAsk, Fits, Pick, PickAsk};
use crate::ports::AgentBoard;
use crate::settings::AgentName;

/// Seconds every ranked agent is said to take
const TYPICAL_SECS: u64 = 3600;

#[derive(Debug)]
enum Answers {
    // The ask's own agents, these first in this order, then the rest in the ask's
    Listed(Vec<AgentName>),
    // Each agent with the seconds to its finish
    Ranks(Vec<(AgentName, u64)>),
    Unreachable,
}

impl Default for Answers {
    fn default() -> Self {
        Self::Listed(Vec::new())
    }
}

#[derive(Debug, Default)]
struct Book {
    answers: Answers,
    asks: Vec<PickAsk>,
    // Each agent and kind a test has set an answer for
    fits: BTreeMap<(AgentName, String), Fits>,
    fit_asks: Vec<FitAsk>,
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
        let agents = agents.iter().map(|a| (name(a), TYPICAL_SECS)).collect();
        self.book.lock().unwrap().answers = Answers::Ranks(agents);
    }

    /// Each pick ranks `agents` in this order, each finishing in its seconds
    pub fn ranks_finishing(&self, agents: &[(&str, u64)]) {
        let name = |a: &str| AgentName::try_from(a.to_owned()).unwrap();
        let agents = agents.iter().map(|(a, secs)| (name(a), *secs)).collect();
        self.book.lock().unwrap().answers = Answers::Ranks(agents);
    }

    /// Each pick ranks every agent the ask allows, in its order
    pub fn ranks_listed(&self) {
        self.book.lock().unwrap().answers = Answers::Listed(Vec::new());
    }

    /// Each pick ranks every agent the ask allows, those of `first` it allows
    /// ahead in this order, then the rest in the ask's
    pub fn ranks_first(&self, first: &[&str]) {
        let name = |a: &&str| AgentName::try_from((*a).to_owned()).unwrap();
        let first = first.iter().map(name).collect();
        self.book.lock().unwrap().answers = Answers::Listed(first);
    }

    /// Each fit ask about `agent` and `kind` answers that it fits
    pub fn fits(&self, agent: &str, kind: &str) {
        self.answer_fit(agent, kind, Fits::Yes);
    }

    /// Each fit ask about `agent` and `kind` answers `finding`
    pub fn does_not_fit(&self, agent: &str, kind: &str, finding: Finding) {
        self.answer_fit(agent, kind, Fits::No(finding));
    }

    /// Each pick and each fit ask goes unanswered, as when the dog cannot be reached
    pub fn unreachable(&self) {
        self.book.lock().unwrap().answers = Answers::Unreachable;
    }

    /// What each pick was asked, in order
    pub fn asks(&self) -> Vec<PickAsk> {
        self.book.lock().unwrap().asks.clone()
    }

    /// What each fit ask was asked, in order
    pub fn fit_asks(&self) -> Vec<FitAsk> {
        self.book.lock().unwrap().fit_asks.clone()
    }

    fn answer_fit(&self, agent: &str, kind: &str, fits: Fits) {
        let key = (
            AgentName::try_from(agent.to_owned()).unwrap(),
            kind.to_owned(),
        );
        self.book.lock().unwrap().fits.insert(key, fits);
    }
}

impl AgentBoard for FakeAgentBoard {
    fn pick(&self, item: &PickAsk) -> Option<Vec<Pick>> {
        let mut book = self.book.lock().unwrap();
        book.asks.push(item.clone());
        let order = match &book.answers {
            Answers::Listed(first) => (first.iter())
                .chain(item.agents.iter().filter(|agent| !first.contains(agent)))
                .map(|agent| (agent.clone(), TYPICAL_SECS))
                .collect(),
            Answers::Ranks(agents) => agents.clone(),
            Answers::Unreachable => return None,
        };
        let fits = |(agent, _): &(AgentName, u64)| {
            item.agents.contains(agent)
                && !item.leave_out.contains(agent)
                && item.pin.as_ref().is_none_or(|pin| pin == agent)
        };
        let picks = order.into_iter().filter(fits).map(|(agent, finish)| Pick {
            agent,
            finish,
            wait: 0,
            cost_usd: None,
        });
        Some(picks.collect())
    }

    fn fit(&self, item: &FitAsk) -> Option<Fits> {
        let mut book = self.book.lock().unwrap();
        book.fit_asks.push(item.clone());
        if matches!(book.answers, Answers::Unreachable) {
            return None;
        }
        Some(
            book.fits
                .get(&(item.agent.clone(), item.kind.clone()))
                .cloned()
                .unwrap_or(Fits::Yes),
        )
    }
}
