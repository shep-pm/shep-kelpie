//! The board's answer to a `pick` ask, and its part of `status`

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::View;
use crate::agent_board::{self, Candidate, History, Item};
use crate::agents::{Agent, Tier};
use crate::dog::availability::{self, Availability};
use crate::dog::door::{Place, say};
use crate::dog::lock_desk;
use crate::ipc::ServerWriteHalf;
use crate::lease::door::{Answer, Ask, Pick, PickAsk};
use crate::pacer::{self, Assessment};
use crate::ports::Timestamp;
use crate::settings::Limit;

impl View {
    /// Each implementer's typical time and cost for an item of no kind, and its wait
    ///
    /// A cost or a wait the board cannot say is null.
    pub fn status(&self, shown: &Availability, now: Timestamp) -> Value {
        let history = self.history();
        let agents = self.implementers.iter().map(|(name, agent)| {
            let tier = agent.fit.tier();
            let typical = history.item(name.as_str(), tier, None);
            let turn = history.turn_secs(name.as_str(), tier, None);
            let line = json!({
                "secs": typical.secs,
                "cost_usd": typical.cost_usd,
                "wait": wait(agent, shown, turn, now),
            });
            (name.to_string(), line)
        });
        Value::Object(agents.collect())
    }

    fn history(&self) -> History<'_> {
        let tiers: BTreeMap<&str, Tier> = (self.implementers.iter())
            .map(|(name, agent)| (name.as_str(), agent.fit.tier()))
            .collect();
        let tier_of = |name: &str| tiers.get(name).copied();
        History::new(self.ledgers.iter().flat_map(|l| l.iter()), tier_of)
    }
}

// The implementers `ask` names, in the order the board tries them for its item at `now`.
fn answer(view: &View, shown: &Availability, ask: &PickAsk, now: Timestamp) -> Vec<Pick> {
    let history = view.history();
    let kind = ask.kind.as_deref();
    let candidates: Vec<_> = (view.implementers.iter())
        .filter(|(name, _)| ask.agents.contains(name))
        .map(|(name, agent)| {
            let tier = agent.fit.tier();
            let turn = history.turn_secs(name.as_str(), tier, kind);
            Candidate {
                agent: name,
                fit: &agent.fit,
                wait: wait(agent, shown, turn, now),
                typical: history.item(name.as_str(), tier, kind),
            }
        })
        .collect();
    let item = Item {
        kind,
        tier: ask.tier,
        pin: ask.pin.as_ref(),
        slack: ask.slack,
        leave_out: &ask.leave_out,
    };
    (agent_board::pick(&item, &candidates).into_iter())
        .map(|choice| Pick {
            agent: choice.agent.clone(),
            finish: choice.finish,
            wait: choice.wait,
            cost_usd: choice.cost_usd,
        })
        .collect()
}

// Seconds before `agent` can start, from its model's load or its plan's
// window, or None with no window. A model the dog reads no load for, behind
// a gateway other than paddock, starts at once. An unread plan waits a recheck.
fn wait(agent: &Agent, shown: &Availability, turn_secs: u64, now: Timestamp) -> Option<u64> {
    let load = (agent.runs.gateway()).and_then(|(gateway, model)| shown.load(gateway, model));
    if let Some(load) = load {
        return agent_board::wait(agent_board::Availability::Gateway(load), turn_secs, now);
    }
    let hold = match agent.runs.session() {
        Some((_, Limit::Account(account))) => Some(match shown.usage(*account) {
            Some(usage) => pacer::window(*account, &usage),
            None => Assessment::unreadable(now, "its plan is not read yet".into()).window,
        }),
        _ => None,
    };
    let availability = match &hold {
        Some(hold) => agent_board::Availability::Plan(hold.as_ref()),
        None => agent_board::Availability::Open,
    };
    agent_board::wait(availability, turn_secs, now)
}

/// Answers a runner's `pick` ask and ends the visit
pub(in crate::dog) async fn visit(place: &Place, ask: Ask, write: &mut ServerWriteHalf) {
    let Some(item) = ask.pick else {
        let why = "a pick ask names the work item it picks for".to_owned();
        return say(write, &Answer::Error(why)).await;
    };
    let now = lock_desk(&place.desk).desk.book.now();
    let shown = availability::lock(&place.shown).clone();
    let view = super::lock(&place.board).view();
    say(write, &Answer::Picks(answer(&view, &shown, &item, now))).await;
}
