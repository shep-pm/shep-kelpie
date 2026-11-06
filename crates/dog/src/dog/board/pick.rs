//! The board's answer to a `pick` ask, and its part of `status`

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::View;
use crate::agent_board::{self, Candidate, DEFAULT_SECS, History, Item, Record, Typical};
use crate::agents::{Agent, Role, Tier};
use crate::dog::availability::{self, Availability};
use crate::dog::door::{Place, say};
use crate::dog::lock_desk;
use crate::ipc::ServerWriteHalf;
use crate::lease::door::{Answer, Ask, Pick, PickAsk};
use crate::pacer::{self, Assessment, Hold, HoldKind};
use crate::ports::Timestamp;
use crate::settings::{AgentName, Limit};

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

    /// Each agent and kind the ledgers give a record to
    pub(super) fn tallies(&self) -> BTreeMap<(String, Option<String>), Record> {
        agent_board::records(self.ledgers.iter().flat_map(|l| l.iter()))
    }

    /// Each agent's track record per kind of work, from every project's ledger
    ///
    /// Keyed by agent and then by kind, `none` for items of no kind. A rate,
    /// median or cost the ledger cannot say is null.
    pub fn records(&self) -> Value {
        let mut agents = serde_json::Map::new();
        for ((agent, kind), record) in self.tallies() {
            let kinds = agents.entry(agent).or_insert_with(|| json!({}));
            kinds[kind.as_deref().unwrap_or("none")] = json!({
                "items": record.items,
                "merged": record.merged,
                "closed": record.closed,
                "merge_rate": record.merge_rate(),
                "median_hours": record.median_hours,
                "review_rounds": record.review_rounds,
                "fix_turns": record.fix_turns,
                "heavy_fix_items": record.heavy_fix_items,
                "rulings": record.rulings,
                "median_cost_usd": record.median_cost_usd,
            });
        }
        Value::Object(agents)
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

// What the board read of each of `names` for an item of `kind` at `now`.
fn candidates<'a>(
    view: &'a View,
    shown: &Availability,
    names: &[AgentName],
    kind: Option<&str>,
    now: Timestamp,
) -> Vec<Candidate<'a>> {
    let history = view.history();
    (view.ranked.iter())
        .filter(|(name, _)| names.contains(name))
        .map(|(name, agent)| {
            let (typical, turn) = estimate(&history, name.as_str(), agent, kind);
            Candidate {
                agent: name,
                fit: &agent.fit,
                wait: wait(agent, shown, turn, now),
                typical,
                local: !(agent.runs.session()).is_some_and(|(_, limit)| !limit.is_local()),
                held: bills_api(agent) && shown.api_held(now).is_some(),
            }
        })
        .collect()
}

// What the dog reads of `candidate`, which starts in `wait` seconds. The ask's
// runner orders it, so the cost, tier, local and held flags and the list's
// place all go with it.
fn read(view: &View, candidate: &Candidate<'_>, wait: u64) -> Pick {
    Pick {
        cost_usd: candidate.typical.cost_usd,
        tier: Some(candidate.fit.tier()),
        local: candidate.local,
        held: candidate.held,
        listed: view.prefer.iter().position(|p| p == candidate.agent),
        ..Pick::new(
            candidate.agent.clone(),
            wait.saturating_add(candidate.typical.secs),
            wait,
        )
    }
}

// The agents `ask` names that fit its item and have a window at `now`, soonest
// finish first. The dog ranks none: the runner does, from what each carries.
pub(super) fn answer(
    view: &View,
    shown: &Availability,
    ask: &PickAsk,
    now: Timestamp,
) -> Vec<Pick> {
    let kind = ask.kind.as_deref();
    let candidates = candidates(view, shown, &ask.agents, kind, now);
    let item = Item {
        kind,
        tier: ask.tier,
        pin: ask.pin.as_ref(),
        leave_out: &ask.leave_out,
    };
    (agent_board::pick(&item, &candidates).into_iter())
        .filter_map(|choice| {
            let candidate = candidates.iter().find(|c| c.agent == choice.agent)?;
            Some(read(view, candidate, choice.wait))
        })
        .collect()
}

// Each of `names` the dog has a window for, read for an item of no kind as
// `status` reads it, whatever kinds its file takes.
pub(super) fn reading(
    view: &View,
    shown: &Availability,
    names: &[AgentName],
    now: Timestamp,
) -> Vec<Pick> {
    let candidates = candidates(view, shown, names, None, now);
    (candidates.iter())
        .filter_map(|c| Some(read(view, c, c.wait?)))
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

// Whether `agent` is billed to the API key
fn bills_api(agent: &Agent) -> bool {
    (agent.runs.session()).is_some_and(|(model, _)| model.harness.bills_api())
}

// The API's refusal as a hold, which every agent billed to the key waits out.
fn api_hold(shown: &Availability, now: Timestamp) -> Option<Hold> {
    Some(Hold {
        kind: HoldKind::Window,
        reason: "the API refused the key".into(),
        until: shown.api_held(now)?,
    })
}

// Seconds before `agent` can start, from its model's load, its plan's
// window or the API's hold, or None with no window. A model the dog reads no load for, behind
// a gateway other than paddock, starts at once. An unread plan waits a recheck.
fn wait(agent: &Agent, shown: &Availability, turn_secs: u64, now: Timestamp) -> Option<u64> {
    let load = (agent.runs.gateway()).and_then(|(gateway, model)| shown.load(gateway, model));
    if let Some(load) = load {
        return agent_board::wait(agent_board::Availability::Gateway(load), turn_secs, now);
    }
    let hold = match agent.runs.session() {
        Some(_) if bills_api(agent) => Some(api_hold(shown, now)),
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

/// Answers a runner's `pick` ask, for a work item or for the agents it names, and ends the visit
pub(in crate::dog) async fn visit(place: &Place, ask: Ask, write: &mut ServerWriteHalf) {
    if ask.pick.is_none() && ask.reading.is_empty() {
        let why = "a pick ask names the work item it picks for".to_owned();
        return say(write, &Answer::Error(why)).await;
    }
    let now = lock_desk(&place.desk).desk.book.now();
    let shown = availability::lock(&place.shown).clone();
    let view = super::lock(&place.board).view();
    let picks = match &ask.pick {
        Some(item) if ask.reading.is_empty() => answer(&view, &shown, item, now),
        _ => reading(&view, &shown, &ask.reading, now),
    };
    say(write, &Answer::Picks(picks)).await;
}
