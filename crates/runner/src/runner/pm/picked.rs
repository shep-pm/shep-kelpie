//! One entry of the project manager's `pick`: a ready issue, and what its
//! work item needs

use std::num::NonZeroU32;

use serde_json::{Map, Value};

use super::act::{WORDS, plain};
use super::answer::ticket;
use crate::agents::Tier;
use crate::settings::AgentName;
use crate::ticket::TicketId;
use crate::work_item::ReviewPlan;

/// What a `pick` entry holds, as a dropped one names it
pub(super) const PICK: &str = "a ticket id, or an object with an `issue` and any of a `tier`, an \
                               implementer as `agent`, `slack` in minutes, `light` as a boolean \
                               and a `review` of `reads` and what they are `about`";

/// The keys a picked object takes
const KEYS: [&str; 6] = ["issue", "tier", "agent", "slack", "light", "review"];

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
    /// Whether it called its issue a small change
    pub(in crate::runner) light: bool,
    /// The review its work item's pull request gets
    pub(in crate::runner) review: Option<ReviewPlan>,
}

impl Picked {
    /// A pick of `issue` that says nothing more of it
    pub(super) fn of(issue: TicketId) -> Self {
        Self {
            issue,
            tier: None,
            agent: None,
            slack: None,
            light: false,
            review: None,
        }
    }
}

// An object of a `reads` of at least 1 and an optional `about`, and no key else.
fn review_plan(value: &Value) -> Option<ReviewPlan> {
    let fields: &Map<String, Value> = value.as_object()?;
    if fields.keys().any(|k| k != "reads" && k != "about") {
        return None;
    }
    let reads = NonZeroU32::new(u32::try_from(fields.get("reads")?.as_u64()?).ok()?)?;
    let about = match fields.get("about").filter(|v| !v.is_null()) {
        Some(about) => Some(plain(about.as_str()?, WORDS)).filter(|a| !a.is_empty()),
        None => None,
    };
    Some(ReviewPlan { reads, about })
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
    let light = match given("light") {
        Some(light) => light.as_bool()?,
        None => false,
    };
    let review = match given("review") {
        Some(review) => Some(review_plan(review)?),
        None => None,
    };
    Some(Picked {
        issue: ticket(fields.get("issue")?)?,
        tier,
        agent,
        slack,
        light,
        review,
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
                    light: false,
                    review: None,
                },
                Picked::of(ticket(14)),
            ]
        );
        assert_eq!(answer.unread, Vec::<String>::new());
        let one = read("{\"pick\": {\"issue\": 12, \"slack\": 30}}").unwrap();
        assert_eq!(one.picks[0].slack, Some(30));
    }

    #[test]
    fn a_pick_may_carry_a_light_tag_and_a_non_boolean_one_drops_the_entry() {
        let answer = read(
            "{\"pick\": [{\"issue\": 12, \"light\": true}, {\"issue\": 14, \"light\": false}, \
             {\"issue\": 15, \"light\": null}, {\"issue\": 16}, 17]}",
        )
        .unwrap();
        let lights: Vec<_> = answer.picks.iter().map(|p| p.light).collect();
        assert_eq!(lights, [true, false, false, false, false]);
        assert_eq!(answer.unread, Vec::<String>::new());

        let bad = ["\"yes\"", "1", "0", "[]"];
        for light in bad {
            let one = read(&format!(
                "{{\"pick\": {{\"issue\": 12, \"light\": {light}}}}}"
            ))
            .unwrap();
            assert_eq!(one.picks, [], "light {light}");
            assert_eq!(one.unread.len(), 1, "light {light}");
        }
    }

    #[test]
    fn a_pick_with_a_field_it_does_not_take_or_a_malformed_one_is_named_and_the_rest_count() {
        let bad = [
            "{\"issue\": 12, \"model\": \"opus\"}",
            "{\"issue\": 12, \"tier\": \"huge\"}",
            "{\"issue\": 12, \"review\": 2}",
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
             implementer as `agent`, `slack` in minutes, `light` as a boolean and a `review` \
             of `reads` and what they are `about`, not {\"issue\":12,\"model\":\"opus\"}"
        );
        let one = read("{\"pick\": {\"issue\": 12, \"tier\": \"huge\"}}").unwrap();
        assert_eq!(
            (one.picks, one.unread),
            (
                vec![],
                vec![format!(
                    "`pick`: {PICK}, not {{\"issue\":12,\"tier\":\"huge\"}}"
                )]
            )
        );
    }

    fn plan(reads: u32, about: Option<&str>) -> ReviewPlan {
        ReviewPlan {
            reads: NonZeroU32::new(reads).unwrap(),
            about: about.map(str::to_owned),
        }
    }

    #[test]
    fn a_pick_may_carry_a_review_plan_of_reads_and_what_they_are_for() {
        let answer = read(
            "{\"pick\": [{\"issue\": 12, \"review\": {\"reads\": 2, \"about\": \"the \
             migration's rollback path\"}}, {\"issue\": 14, \"review\": {\"reads\": 1}}, \
             {\"issue\": 15, \"review\": null}, {\"issue\": 16, \"review\": {\"reads\": 3, \
             \"about\": null}}]}",
        )
        .unwrap();
        let plans: Vec<_> = answer.picks.iter().map(|p| p.review.clone()).collect();
        assert_eq!(
            plans,
            [
                Some(plan(2, Some("the migration's rollback path"))),
                Some(plan(1, None)),
                None,
                Some(plan(3, None)),
            ]
        );
        assert_eq!(answer.unread, Vec::<String>::new());
    }

    #[test]
    fn a_review_plan_with_no_reads_or_zero_reads_drops_the_entry() {
        let bad = [
            "{}",
            "{\"about\": \"x\"}",
            "{\"reads\": 0}",
            "{\"reads\": -1}",
            "{\"reads\": 1.5}",
            "{\"reads\": \"2\"}",
            "{\"reads\": null}",
            "{\"reads\": 5000000000}",
            "{\"reads\": 2, \"about\": 7}",
            "2",
            "[]",
        ];
        for review in bad {
            let one = read(&format!(
                "{{\"pick\": {{\"issue\": 12, \"review\": {review}}}}}"
            ))
            .unwrap();
            assert_eq!(one.picks, [], "review {review}");
            assert_eq!(one.unread.len(), 1, "review {review}");
        }
    }

    #[test]
    fn a_review_plan_with_a_key_it_does_not_take_drops_the_entry() {
        let one =
            read("{\"pick\": {\"issue\": 12, \"review\": {\"reads\": 2, \"depth\": 3}}}").unwrap();
        assert_eq!(
            (one.picks, one.unread),
            (
                vec![],
                vec![format!(
                    "`pick`: {PICK}, not {{\"issue\":12,\"review\":{{\"depth\":3,\"reads\":2}}}}"
                )]
            )
        );
    }

    #[test]
    fn a_plans_about_is_cut_and_a_blank_one_reads_as_none() {
        let long = "word ".repeat(200);
        let answer = read(&format!(
            "{{\"pick\": [{{\"issue\": 12, \"review\": {{\"reads\": 2, \"about\": \"{long}\"}}}}, \
             {{\"issue\": 14, \"review\": {{\"reads\": 2, \"about\": \"  \\n \"}}}}, \
             {{\"issue\": 15, \"review\": {{\"reads\": 2, \"about\": \"a\\nb\\u0007  c\"}}}}]}}"
        ))
        .unwrap();
        let abouts: Vec<_> = answer
            .picks
            .iter()
            .map(|p| p.review.clone().unwrap().about)
            .collect();
        let cut = format!("{} …", "word ".repeat(60).trim_end());
        assert_eq!(abouts, [Some(cut), None, Some("a b c".to_owned())]);
    }
}
