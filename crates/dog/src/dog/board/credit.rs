//! The dog's answer to a `credit` ask: what the month has spent of the API credit
//!
//! The API credit is one key shared by every project, so the month's spend
//! needs every project's ledger, and the board is the one place that rereads
//! all of them. Each ledger's calls are summed by agent and by UTC month as
//! the board reads it, so an ask at a month's end reads a sum it already has.
//! The dog asks no API what the credit has left, since there is no balance to
//! read: the sum is kelpie's own record.

use std::collections::BTreeMap;

use super::{View, lock};
use crate::dog::door::{Place, say};
use crate::dog::lock_desk;
use crate::ipc::ServerWriteHalf;
use crate::lease::door::{Answer, Credit, month_around};
use crate::ports::Timestamp;
use crate::settings::AgentName;
use crate::usage::Line;

/// What one project's calls cost each agent they name, by the UTC month they ended in
pub(super) type Spent = BTreeMap<(AgentName, Timestamp), f64>;

/// `lines`' calls, summed by agent and by the UTC month each ended in
///
/// A line that names no agent, or names no cost, adds nothing. Which of these
/// agents bills the API is read from the book when the dog answers, so a line
/// of an agent whose file is gone, or that no longer names `login: api`, is
/// left out then rather than here.
pub(super) fn spent(lines: &[Line]) -> Spent {
    let mut spent = Spent::new();
    for line in lines {
        let Line::Call(call) = line else { continue };
        let (Some(agent), Some(usd)) = (call.agent.as_deref(), call.cost_usd) else {
            continue;
        };
        let Ok(agent) = AgentName::try_from(agent.to_owned()) else {
            continue;
        };
        *spent.entry((agent, month_around(call.at).0)).or_default() += usd;
    }
    spent
}

impl View {
    /// What the agents the book bills to the API key spent in the month `now` falls in
    ///
    /// An agent counts by its file as the dog holds it now, whatever its role: a
    /// project manager or a triager on `login: api` spends the same credit as a
    /// worker.
    pub(super) fn credit(&self, now: Timestamp) -> Credit {
        let (since, resets_at) = month_around(now);
        let spent_usd = self
            .ledgers
            .iter()
            .flat_map(|ledger| ledger.spent.iter())
            .filter(|((_, month), _)| *month == since)
            .filter(|((agent, _), _)| self.api.contains(agent))
            .map(|(_, usd)| *usd)
            .sum();
        Credit {
            spent_usd,
            monthly_usd: self.monthly_credit(),
            since: since.0,
            resets_at: resets_at.0,
        }
    }
}

/// Answers a runner's `credit` ask with the month's API spend, and ends the visit
pub(in crate::dog) async fn visit(place: &Place, write: &mut ServerWriteHalf) {
    let now = lock_desk(&place.desk).desk.book.now();
    let credit = lock(&place.board).view().credit(now);
    say(write, &Answer::Credit(credit)).await;
}
