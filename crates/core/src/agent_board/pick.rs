//! Which of the agents that fit a work item the board picks, in order

use std::cmp::Ordering;

use super::Typical;
use crate::agents::{Fit, Tier};
use crate::settings::AgentName;

/// The minutes past the soonest finish an item allows when its pick names no slack
pub const DEFAULT_SLACK_MINUTES: u32 = 60;

/// What a work item asks of the agent the board picks
#[derive(Debug, Clone, Copy, Default)]
pub struct Item<'a> {
    /// Its kind, as `fix`, or None when its issue names none
    pub kind: Option<&'a str>,
    /// The least tier it needs, or None for any
    pub tier: Option<Tier>,
    /// The agent it is pinned to, the only one tried
    pub pin: Option<&'a AgentName>,
    /// Minutes past the soonest finish it allows, [`DEFAULT_SLACK_MINUTES`] when None
    pub slack: Option<u32>,
    /// Agents not to pick
    pub leave_out: &'a [AgentName],
}

/// An agent the board may pick, with what it read of it
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    /// Its name
    pub agent: &'a AgentName,
    /// Its file's kinds and tier
    pub fit: &'a Fit,
    /// Seconds before it can start, or None when it has no window
    pub wait: Option<u64>,
    /// What it typically takes for the item
    pub typical: Typical,
}

/// An agent the board would pick
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Choice<'a> {
    /// Its name
    pub agent: &'a AgentName,
    /// Seconds from now to its finish: its wait and its typical time
    pub finish: u64,
    /// Seconds before it can start
    pub wait: u64,
    /// What it typically costs in US dollars, or None when unknown
    pub cost_usd: Option<f64>,
}

/// The candidates that fit `item`, in the order the board tries them
///
/// Those that finish within the item's slack of the soonest come first,
/// cheapest first and then soonest, an unknown cost last. The rest follow,
/// soonest first. Empty when none fits.
pub fn pick<'a>(item: &Item<'_>, candidates: &[Candidate<'a>]) -> Vec<Choice<'a>> {
    let mut fits: Vec<Choice<'a>> = candidates
        .iter()
        .filter(|c| item.pin.is_none_or(|pin| pin == c.agent))
        .filter(|c| !item.leave_out.contains(c.agent))
        .filter(|c| takes(c.fit, item.kind) && item.tier.is_none_or(|t| c.fit.tier() >= t))
        .filter_map(|c| {
            let wait = c.wait?;
            Some(Choice {
                agent: c.agent,
                finish: wait.saturating_add(c.typical.secs),
                wait,
                cost_usd: c.typical.cost_usd,
            })
        })
        .collect();
    let Some(soonest) = fits.iter().map(|c| c.finish).min() else {
        return fits;
    };
    let slack = u64::from(item.slack.unwrap_or(DEFAULT_SLACK_MINUTES)) * 60;
    let within = soonest.saturating_add(slack);
    fits.sort_by_key(|c| c.finish);
    let (mut eligible, rest): (Vec<_>, Vec<_>) = fits.into_iter().partition(|c| c.finish <= within);
    eligible.sort_by(|a, b| cheaper(a.cost_usd, b.cost_usd));
    eligible.extend(rest);
    eligible
}

// An agent whose file names no kinds takes every kind, and an item with no kind takes any agent
fn takes(fit: &Fit, kind: Option<&str>) -> bool {
    match (&fit.kinds, kind) {
        (Some(kinds), Some(kind)) => kinds.iter().any(|k| k == kind),
        _ => true,
    }
}

fn cheaper(a: Option<f64>, b: Option<f64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Kinds;

    fn name(agent: &str) -> AgentName {
        AgentName::try_from(agent.to_owned()).unwrap()
    }

    fn fit(tier: Tier, kinds: Option<&[&str]>) -> Fit {
        Fit {
            tier: Some(tier),
            kinds: kinds.map(|k| {
                Kinds::try_from(k.iter().map(|k| (*k).to_owned()).collect::<Vec<_>>()).unwrap()
            }),
            ..Fit::default()
        }
    }

    fn candidate<'a>(
        agent: &'a AgentName,
        fit: &'a Fit,
        wait: Option<u64>,
        secs: u64,
        cost_usd: Option<f64>,
    ) -> Candidate<'a> {
        Candidate {
            agent,
            fit,
            wait,
            typical: Typical { secs, cost_usd },
        }
    }

    fn order<'a>(choices: &[Choice<'a>]) -> Vec<&'a str> {
        choices.iter().map(|c| c.agent.as_str()).collect()
    }

    #[test]
    fn the_cheapest_within_the_slack_comes_first_then_the_rest_soonest_first() {
        let (local, sonnet, opus, slow) =
            (name("local"), name("sonnet"), name("opus"), name("slow"));
        let any = Fit::default();
        let candidates = [
            candidate(&slow, &any, Some(9000), 3600, Some(0.1)),
            candidate(&opus, &any, Some(0), 1800, Some(3.0)),
            candidate(&local, &any, Some(2400), 3600, Some(0.2)),
            candidate(&sonnet, &any, Some(0), 2400, Some(1.2)),
        ];
        let choices = pick(&Item::default(), &candidates);
        // soonest is opus at 1800; local at 6000 is past it by more than an hour
        assert_eq!(order(&choices), ["sonnet", "opus", "local", "slow"]);
        assert_eq!(
            choices[2],
            Choice {
                agent: &local,
                finish: 6000,
                wait: 2400,
                cost_usd: Some(0.2)
            }
        );
    }

    #[test]
    fn a_slack_of_zero_keeps_only_the_soonest_in_front() {
        let (fast, cheap) = (name("fast"), name("cheap"));
        let any = Fit::default();
        let candidates = [
            candidate(&cheap, &any, Some(0), 1801, Some(0.1)),
            candidate(&fast, &any, Some(0), 1800, Some(5.0)),
        ];
        let item = Item {
            slack: Some(0),
            ..Item::default()
        };
        assert_eq!(order(&pick(&item, &candidates)), ["fast", "cheap"]);
    }

    #[test]
    fn a_pin_keeps_only_itself_and_a_left_out_agent_is_never_picked() {
        let (local, sonnet) = (name("local"), name("sonnet"));
        let any = Fit::default();
        let candidates = [
            candidate(&local, &any, Some(0), 600, Some(0.1)),
            candidate(&sonnet, &any, Some(0), 600, Some(1.0)),
        ];
        let pinned = Item {
            pin: Some(&sonnet),
            ..Item::default()
        };
        assert_eq!(order(&pick(&pinned, &candidates)), ["sonnet"]);
        let gone = [sonnet.clone()];
        let handed = Item {
            leave_out: &gone,
            ..Item::default()
        };
        assert_eq!(order(&pick(&handed, &candidates)), ["local"]);
        let both = Item {
            pin: Some(&sonnet),
            leave_out: &gone,
            ..Item::default()
        };
        assert!(pick(&both, &candidates).is_empty());
    }

    #[test]
    fn a_tier_floor_drops_the_agents_below_it() {
        let (local, sonnet, opus) = (name("local"), name("sonnet"), name("opus"));
        let (light, standard, heavy) = (
            fit(Tier::Light, None),
            Fit::default(),
            fit(Tier::Heavy, None),
        );
        let candidates = [
            candidate(&local, &light, Some(0), 600, Some(0.1)),
            candidate(&sonnet, &standard, Some(0), 600, Some(1.0)),
            candidate(&opus, &heavy, Some(0), 600, Some(3.0)),
        ];
        let item = Item {
            tier: Some(Tier::Standard),
            ..Item::default()
        };
        assert_eq!(order(&pick(&item, &candidates)), ["sonnet", "opus"]);
    }

    #[test]
    fn a_kind_keeps_the_agents_that_take_it_or_take_every_kind() {
        let (docs, fixer, all) = (name("docs"), name("fixer"), name("all"));
        let (writes, fixes, any) = (
            fit(Tier::Light, Some(&["docs"])),
            fit(Tier::Light, Some(&["fix", "refactor"])),
            Fit::default(),
        );
        let candidates = [
            candidate(&docs, &writes, Some(0), 600, Some(0.1)),
            candidate(&fixer, &fixes, Some(0), 600, Some(0.2)),
            candidate(&all, &any, Some(0), 600, Some(0.3)),
        ];
        let item = Item {
            kind: Some("fix"),
            ..Item::default()
        };
        assert_eq!(order(&pick(&item, &candidates)), ["fixer", "all"]);
        assert_eq!(
            order(&pick(&Item::default(), &candidates)),
            ["docs", "fixer", "all"],
            "an item with no kind takes any agent"
        );
    }

    #[test]
    fn an_agent_with_no_window_is_never_picked_and_none_left_is_empty() {
        let (local, sonnet) = (name("local"), name("sonnet"));
        let any = Fit::default();
        let candidates = [
            candidate(&local, &any, None, 600, Some(0.1)),
            candidate(&sonnet, &any, Some(0), 600, Some(1.0)),
        ];
        assert_eq!(order(&pick(&Item::default(), &candidates)), ["sonnet"]);
        assert!(pick(&Item::default(), &candidates[..1]).is_empty());
    }

    #[test]
    fn an_equal_cost_goes_soonest_first_and_an_unknown_cost_last() {
        let (later, sooner, unknown) = (name("later"), name("sooner"), name("unknown"));
        let any = Fit::default();
        let candidates = [
            candidate(&unknown, &any, Some(0), 600, None),
            candidate(&later, &any, Some(0), 1200, Some(1.0)),
            candidate(&sooner, &any, Some(0), 900, Some(1.0)),
        ];
        assert_eq!(
            order(&pick(&Item::default(), &candidates)),
            ["sooner", "later", "unknown"]
        );
    }
}
