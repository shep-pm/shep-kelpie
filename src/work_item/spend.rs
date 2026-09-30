//! What a work item's calls have cost, by role, and how many qwen rounds ran
//!
//! For `status` and the log only. Nothing here reaches a prompt.

use serde::{Deserialize, Serialize};

use super::{CallRecord, WorkItem};
use crate::ports::{Cost, Role, SessionId, Timestamp, Usage};

/// What one role's calls have cost
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct RoleSpend {
    /// How many calls it made
    pub calls: usize,
    /// What they cost, in US dollars
    pub cost_usd: f64,
}

/// What a work item's Claude calls have cost, by role
///
/// The relay's calls are not here: the relay is one session for every
/// ruling, and kelpie only delivers to it, so no cost comes back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Spend {
    /// The worker's turns
    pub worker: RoleSpend,
    /// The Claude review rounds
    pub reviewer: RoleSpend,
    /// The judge's calls
    pub judge: RoleSpend,
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
                // A planning call runs before any work item opens.
                Role::Planner => continue,
            };
            role.calls += 1;
            role.cost_usd += call.cost.usd();
        }
        spend
    }

    /// Records a call that ended at `at`, whose session had cost
    /// `session_cost` by then, and returns what the call itself cost
    ///
    /// A session's calls each cost the change in it, so a fresh session's
    /// only call costs all of it.
    pub fn record_call(
        &mut self,
        role: Role,
        at: Timestamp,
        session: SessionId,
        usage: Usage,
        session_cost: Cost,
    ) -> Cost {
        let before = self.session_cost(&session);
        let cost = Cost(session_cost.0.saturating_sub(before.0));
        self.calls.push(CallRecord {
            role,
            at,
            session,
            usage,
            cost,
            session_cost,
        });
        cost
    }
}
