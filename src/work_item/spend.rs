//! What a work item's calls have cost, by role, and how many qwen rounds ran
//!
//! For `status` and the log only. Nothing here reaches a prompt.

use serde::{Deserialize, Serialize};

use super::{CallRecord, WorkItem};
use crate::ports::{Cost, Role, SessionId, Timestamp, Usage};

/// What one role's calls have cost
///
/// Every harness reports tokens. Only some report dollars, so `cost_usd`
/// covers the calls that did and `unpriced_calls` counts those that did not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct RoleSpend {
    /// How many calls it made
    pub calls: usize,
    /// The tokens they used, all together
    pub tokens: Usage,
    /// What the calls that reported dollars cost, in US dollars, or null
    /// when none did
    pub cost_usd: Option<f64>,
    /// Calls whose harness reported no dollars
    #[serde(skip_serializing_if = "is_zero")]
    pub unpriced_calls: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// What a work item's agent calls have cost, by role
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Spend {
    /// The worker's turns
    pub worker: RoleSpend,
    /// The Claude review rounds
    pub reviewer: RoleSpend,
    /// The judge's calls
    pub judge: RoleSpend,
    /// The whole-issue checks
    pub auditor: RoleSpend,
    /// The deep review rounds' sessions
    pub deep_reviewer: RoleSpend,
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
                Role::Judge => &mut spend.judge,
                Role::Auditor => &mut spend.auditor,
                Role::DeepReviewer => &mut spend.deep_reviewer,
                // A planning call runs before any work item opens.
                Role::Planner => continue,
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
        let before = self.session_cost(&session);
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
