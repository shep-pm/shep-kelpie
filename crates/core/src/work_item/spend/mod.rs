//! What a work item's calls have cost, by role, and how many qwen rounds ran
//!
//! For `status`, the log and the work item's budget. Nothing here reaches a prompt.

use serde::{Deserialize, Serialize};

use super::{CallRecord, WorkItem};
use crate::ports::{Cost, Role, SessionId, Timestamp, Usage};
use crate::settings::PerItemUsd;

/// What one role's calls have cost
///
/// Every harness reports tokens. Only some report dollars, so `cost_usd`
/// covers the calls that did and `unpriced_calls` counts those that did not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct RoleSpend {
    /// How many calls it made
    pub calls: usize,
    /// The tokens they used, all together
    pub tokens: Usage,
    /// What the calls that reported dollars cost, in US dollars, or null
    /// when none did
    pub cost_usd: Option<f64>,
    /// Calls whose harness reported no dollars
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unpriced_calls: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// What a work item's agent calls have cost, by role
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    /// The worker's turns
    pub worker: RoleSpend,
    /// The reviewers' sessions, on any harness
    pub reviewer: RoleSpend,
}

/// What a work item's priced calls have cost, by role, in the state's own units
///
/// It is what the work item's budget compares with its cap. A call whose
/// harness reports no dollars counts nothing.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Priced {
    /// The worker's turns
    pub worker: Cost,
    /// The reviewers' sessions, on any harness
    pub reviewer: Cost,
}

impl Priced {
    /// Both roles together
    pub fn total(self) -> Cost {
        Cost(self.worker.0.saturating_add(self.reviewer.0))
    }
}

/// The cap an answer to an over-budget ruling lifts `cap` to when its calls have
/// cost `spent`: doubled, and doubled again while that is still below `spent`
pub fn raised_cap(cap: Cost, spent: Cost) -> Cost {
    let mut raised = cap.0.saturating_mul(2).max(1);
    while raised < spent.0 {
        raised = raised.saturating_mul(2);
    }
    Cost(raised)
}

/// A work item's qwen rounds. They cost no money, so it is the count and the
/// time they took.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QwenTally {
    /// Rounds that ran
    pub rounds: u32,
    /// Seconds they took, all together
    pub seconds: u64,
}

impl WorkItem {
    /// What its calls have cost so far, by role
    pub fn spend(&self) -> Spend {
        let mut spend = Spend::default();
        for call in &self.calls {
            let role = match call.role {
                Role::Worker => &mut spend.worker,
                Role::Reviewer => &mut spend.reviewer,
                // The project manager, the representative and the triager are the
                // project's, so no work item records them.
                Role::Pm | Role::Representative | Role::Triager => continue,
            };
            role.calls += 1;
            role.tokens += call.usage;
            match call.unpriced {
                true => role.unpriced_calls += 1,
                false => *role.cost_usd.get_or_insert(0.0) += call.cost.usd(),
            }
        }
        spend
    }

    /// What its priced calls have cost so far, by role
    ///
    /// Only the worker and the reviewers spend on a work item.
    pub fn priced(&self) -> Priced {
        let mut priced = Priced::default();
        for call in self.calls.iter().filter(|c| !c.unpriced) {
            let role = match call.role {
                Role::Worker => &mut priced.worker,
                Role::Reviewer => &mut priced.reviewer,
                Role::Pm | Role::Representative | Role::Triager => continue,
            };
            role.0 = role.0.saturating_add(call.cost.0);
        }
        priced
    }

    /// The most its priced calls may cost before its worker's next turn
    /// waits on a ruling: `per_item`, doubled once for each such ruling answered
    pub fn budget_cap(&self, per_item: PerItemUsd) -> Cost {
        let doubled = 2u64.saturating_pow(self.budget_raises);
        Cost(per_item.cost().0.saturating_mul(doubled))
    }

    /// What its priced calls have cost and the cap they are over, or `None`
    /// while they cost no more than the cap
    pub fn over_budget(&self, per_item: PerItemUsd) -> Option<(Priced, Cost)> {
        let (priced, cap) = (self.priced(), self.budget_cap(per_item));
        (priced.total() > cap).then_some((priced, cap))
    }

    /// Lifts its cap for an answer to an over-budget ruling: one doubling, and
    /// more while its priced calls still cost more than the cap, so one answer
    /// always lets the worker's next turn start
    pub fn raise_budget(&mut self, per_item: PerItemUsd) {
        self.budget_raises = self.budget_raises.saturating_add(1);
        while self.over_budget(per_item).is_some() {
            self.budget_raises = self.budget_raises.saturating_add(1);
        }
    }

    /// Records a call that ended at `at`, whose session had cost
    /// `session_cost` by then, and returns what the call itself cost, or
    /// `None` when its harness reports no cost
    ///
    /// A session's calls each cost the change in it, so a fresh session's
    /// only call costs all of it. A harness that reports no cost leaves the
    /// session's as it was.
    pub fn record_call(
        &mut self,
        role: Role,
        at: Timestamp,
        session: SessionId,
        usage: Usage,
        session_cost: Option<Cost>,
    ) -> Option<Cost> {
        self.record_metered_call(role, at, session, usage, session_cost, None)
    }

    /// Records a call like [`Self::record_call`], priced by `energy` where its
    /// harness reports no session cost: what its gateway's lease used, at the
    /// host's price
    pub fn record_metered_call(
        &mut self,
        role: Role,
        at: Timestamp,
        session: SessionId,
        usage: Usage,
        session_cost: Option<Cost>,
        energy: Option<Cost>,
    ) -> Option<Cost> {
        let before = self.session_cost(&session);
        let session_cost =
            session_cost.or_else(|| energy.map(|e| Cost(before.0.saturating_add(e.0))));
        let unpriced = session_cost.is_none();
        let session_cost = session_cost.unwrap_or(before);
        let cost = Cost(session_cost.0.saturating_sub(before.0));
        self.calls.push(CallRecord {
            role,
            at,
            session,
            usage,
            cost,
            session_cost,
            unpriced,
        });
        (!unpriced).then_some(cost)
    }
}

#[cfg(test)]
mod tests;
