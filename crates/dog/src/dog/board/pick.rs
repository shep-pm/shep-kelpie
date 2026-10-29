//! The board's answer to a `pick` ask, and its part of `status`

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::View;
use crate::agent_board::{self, Candidate, DEFAULT_SECS, History, Item, Typical};
use crate::agents::{Agent, Role, Tier};
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
        let agents = self.implementers().map(|(name, agent)| {
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

    // The ledger's items, each with its agent's tier. A finished line names
    // the implementer that built the item, so a reviewer's name reaches none
    // and its tier counts toward no median.
    fn history(&self) -> History<'_> {
        let tiers: BTreeMap<&str, Tier> = (self.implementers())
            .map(|(name, agent)| (name.as_str(), agent.fit.tier()))
            .collect();
        let tier_of = |name: &str| tiers.get(name).copied();
        History::new(self.ledgers.iter().flat_map(|l| l.iter()), tier_of)
    }
}

// The agents `ask` names, in the order the board tries them for its item at `now`.
fn answer(view: &View, shown: &Availability, ask: &PickAsk, now: Timestamp) -> Vec<Pick> {
    let history = view.history();
    let kind = ask.kind.as_deref();
    let candidates: Vec<_> = (view.ranked.iter())
        .filter(|(name, _)| ask.agents.contains(name))
        .map(|(name, agent)| {
            let (typical, turn) = estimate(&history, name.as_str(), agent, kind);
            Candidate {
                agent: name,
                fit: &agent.fit,
                wait: wait(agent, shown, turn, now),
                typical,
                local: !(agent.runs.session()).is_some_and(|(_, limit)| limit.is_account()),
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

// What `agent` typically takes for an item of `kind`, and one of its turns:
// the ledger's medians for an implementer, and the default for a reviewer,
// whose items no ledger line names.
fn estimate(
    history: &History<'_>,
    name: &str,
    agent: &Agent,
    kind: Option<&str>,
) -> (Typical, u64) {
    if agent.role != Role::Implementer {
        return (Typical::default(), DEFAULT_SECS);
    }
    let tier = agent.fit.tier();
    (
        history.item(name, tier, kind),
        history.turn_secs(name, tier, kind),
    )
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
