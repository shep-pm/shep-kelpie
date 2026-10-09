//! What a finished work item spent, as its record keeps it: calls, tokens,
//! units and cost by role, its local rounds, and how often it went round
//! its loop

use serde::{Deserialize, Serialize};

use super::{QwenTally, WorkItem};
use crate::ports::{Cost, Role, Usage};

/// How often a work item went round its loop: review rounds, fix turns
/// and rulings
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    /// Reviewers' rounds that came back with an answer, a second look
    /// counted with its first
    #[serde(default)]
    pub review_rounds: u32,
    /// Worker turns sent to fix a round's held findings
    #[serde(default)]
    pub fix_turns: u32,
    /// Rulings raised for it
    #[serde(default)]
    pub rulings: u32,
    /// Worker turns sent to fix a red CI run, which `ci.fix_attempts` caps
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ci_fix_turns: u32,
    /// The worker's calls that reported no usage: failed, timed out,
    /// stopped or unreadable, which no call record holds
    #[serde(default, skip_serializing_if = "is_zero")]
    pub worker_unreported: u32,
    /// The same for the reviewers' sessions
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reviewer_unreported: u32,
}

impl Counts {
    /// Whether nothing has been counted
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }
}

/// What one role's calls cost a work item
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleTally {
    /// How many calls it made
    pub calls: u32,
    /// The tokens they used, all together
    pub tokens: Usage,
    /// Those tokens in the control room's units
    pub units: u64,
    /// What the calls that reported dollars cost
    pub cost: Cost,
    /// Calls whose harness reported no dollars
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unpriced_calls: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// What a work item spent, by role, with its local rounds and its counts
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tally {
    /// The worker's turns
    pub worker: RoleTally,
    /// The reviewers' sessions
    pub reviewer: RoleTally,
    /// The local rounds, which cost no money, and their seconds
    pub qwen: QwenTally,
    /// Its review rounds, fix turns and rulings
    pub counts: Counts,
}

impl Tally {
    /// What the priced calls of every role cost
    pub fn cost(&self) -> Cost {
        Cost(self.worker.cost.0.saturating_add(self.reviewer.cost.0))
    }

    /// Every role's units together
    pub fn units(&self) -> u64 {
        self.worker.units.saturating_add(self.reviewer.units)
    }

    /// Every role's calls that reported no dollars
    pub fn unpriced_calls(&self) -> u32 {
        (self.worker.unpriced_calls).saturating_add(self.reviewer.unpriced_calls)
    }
}

impl WorkItem {
    /// What it has spent so far, as a finished work item's record keeps it
    pub fn tally(&self) -> Tally {
        let mut tally = Tally {
            qwen: self.qwen,
            counts: self.counts,
            ..Tally::default()
        };
        for call in &self.calls {
            let role = match call.role {
                Role::Worker => &mut tally.worker,
                Role::Reviewer => &mut tally.reviewer,
                // Neither is ever a work item's call.
                Role::IssueWriter | Role::Pm => continue,
            };
            role.calls = role.calls.saturating_add(1);
            role.tokens += call.usage;
            // Each call's own rounding, as its ledger line has it
            role.units = role.units.saturating_add(crate::usage::units(call.usage));
            match call.unpriced {
                true => role.unpriced_calls = role.unpriced_calls.saturating_add(1),
                false => role.cost = Cost(role.cost.0.saturating_add(call.cost.0)),
            }
        }
        let unreported = [
            (&mut tally.worker, self.counts.worker_unreported),
            (&mut tally.reviewer, self.counts.reviewer_unreported),
        ];
        for (role, unreported) in unreported {
            role.calls = role.calls.saturating_add(unreported);
            role.unpriced_calls = role.unpriced_calls.saturating_add(unreported);
        }
        tally
    }
}

#[cfg(test)]
mod tests {
    use crate::ports::{Cost, Role, SessionId, Timestamp, Usage};
    use crate::test::a_work_item;

    // Two calls of four cache-read tokens each round to no units apiece, as
    // their ledger lines do, where their sum would round to one.
    #[test]
    fn units_are_each_call_s_own_and_unreported_calls_count_unpriced() {
        let mut item = a_work_item();
        item.calls.clear();
        let reads = Usage {
            cache_read: 4,
            ..Usage::default()
        };
        for at in [1, 2] {
            let session = SessionId("5e55".into());
            item.record_call(Role::Worker, Timestamp(at), session, reads, Some(Cost(0)));
        }
        item.counts.worker_unreported = 1;
        item.counts.reviewer_unreported = 2;
        let tally = item.tally();
        assert_eq!((tally.worker.units, tally.worker.tokens.cache_read), (0, 8));
        assert_eq!((tally.worker.calls, tally.worker.unpriced_calls), (3, 1));
        assert_eq!(
            (tally.reviewer.calls, tally.reviewer.unpriced_calls),
            (2, 2)
        );
    }
}
