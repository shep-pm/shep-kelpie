//! One entry of the project manager's `pick`: a ready issue, and what its
//! work item needs

use serde_json::Value;

use super::answer::ticket;
use crate::agents::Tier;
use crate::settings::AgentName;
use crate::ticket::TicketId;

/// What a `pick` entry holds, as a dropped one names it
pub(super) const PICK: &str = "a ticket id, or an object with an `issue` and any of a `tier`, an \
                               implementer as `agent` and `slack` in minutes";

/// The keys a picked object takes
const KEYS: [&str; 4] = ["issue", "tier", "agent", "slack"];

/// A ready issue the project manager picked, and what it said its work item needs
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct Picked {
    /// The ready issue
    pub(in crate::runner) issue: TicketId,
    /// The least tier its work item needs
    pub(in crate::runner) tier: Option<Tier>,
    /// The implementer its work item is pinned to
    pub(in crate::runner) agent: Option<AgentName>,
    /// The minutes past its quickest finish its work item may take
    pub(in crate::runner) slack: Option<u32>,
}

impl Picked {
    /// A pick of `issue` that says nothing more of it
    pub(super) fn of(issue: TicketId) -> Self {
        Self {
            issue,
            tier: None,
            agent: None,
            slack: None,
        }
    }
}

// A ticket id alone, or an object of no keys but a pick's, each well formed.
pub(super) fn picked(value: &Value) -> Option<Picked> {
    if let Some(issue) = ticket(value) {
        return Some(Picked::of(issue));
    }
    let fields = value.as_object()?;
    if fields.keys().any(|k| !KEYS.contains(&k.as_str())) {
        return None;
    }
    let given = |key: &str| fields.get(key).filter(|v| !v.is_null());
    let tier = match given("tier") {
        Some(tier) => Some(serde_json::from_value::<Tier>(tier.clone()).ok()?),
        None => None,
    };
    let agent = match given("agent") {
        Some(agent) => Some(AgentName::try_from(agent.as_str()?.trim().to_owned()).ok()?),
        None => None,
    };
    let slack = match given("slack") {
        Some(slack) => Some(u32::try_from(slack.as_u64()?).ok()?),
        None => None,
    };
    Some(Picked {
        issue: ticket(fields.get("issue")?)?,
        tier,
        agent,
        slack,
    })
}

#[cfg(test)]
mod tests {
    use super::super::answer::read;
    use super::*;
    use crate::test::ticket;

    #[test]
    fn a_pick_is_a_ticket_or_an_issue_with_its_tier_agent_and_slack() {
        let answer = read(
            "{\"pick\": [9, {\"issue\": \"#12\", \"tier\": \"heavy\", \"agent\": \"opus-high\", \
             \"slack\": 0}, {\"issue\": 14, \"tier\": null}]}",
        )
        .unwrap();
        assert_eq!(
            answer.picks,
            [
                Picked::of(ticket(9)),
                Picked {
                    issue: ticket(12),
                    tier: Some(Tier::Heavy),
                    agent: Some(AgentName::kelpies("opus-high")),
                    slack: Some(0),
                },
                Picked::of(ticket(14)),
            ]
        );
        assert_eq!(answer.unread, Vec::<String>::new());
        let one = read("{\"pick\": {\"issue\": 12, \"slack\": 30}}").unwrap();
        assert_eq!(one.picks[0].slack, Some(30));
    }

    #[test]
    fn a_pick_with_a_field_it_does_not_take_or_a_malformed_one_is_named_and_the_rest_count() {
        let bad = [
            "{\"issue\": 12, \"model\": \"opus\"}",
            "{\"issue\": 12, \"tier\": \"huge\"}",
            "{\"issue\": 12, \"slack\": -5}",
            "{\"issue\": 12, \"slack\": \"soon\"}",
            "{\"issue\": 12, \"agent\": \"Opus High\"}",
            "{\"tier\": \"light\"}",
        ];
        let answer = read(&format!("{{\"pick\": [9, {}]}}", bad.join(", "))).unwrap();
        assert_eq!(answer.picks, [Picked::of(ticket(9))]);
        assert_eq!(answer.unread.len(), bad.len(), "{:?}", answer.unread);
        assert_eq!(
            answer.unread[0],
            "`pick`: a ticket id, or an object with an `issue` and any of a `tier`, an \
             implementer as `agent` and `slack` in minutes, not {\"issue\":12,\"model\":\"opus\"}"
        );
    }
}
