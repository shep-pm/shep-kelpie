//! Which of the agents that fit a work item the board picks, in order

use std::cmp::Ordering;

use super::Typical;
use crate::agents::{Fit, Tier};
use crate::settings::{AgentName, DEFAULT_SLACK_MINUTES};

/// What a work item asks of the agent the board picks
#[derive(Debug, Clone, Copy, Default)]
pub struct Item<'a> {
    /// Its kind, as `fix`, or None when its issue names none
    pub kind: Option<&'a str>,
    /// The least tier it needs, or None for any
    pub tier: Option<Tier>,
    /// The agent it is pinned to, the only one tried
    pub pin: Option<&'a AgentName>,
    /// Minutes past the soonest finish it allows, [`DEFAULT_SLACK_MINUTES`]
    /// when None, which only an older runner's ask leaves
    pub slack: Option<u32>,
    /// Agents not to pick
    pub leave_out: &'a [AgentName],
    /// The maintainer's ranking of agents, first most wanted, empty for none
    pub prefer: &'a [AgentName],
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
    /// Whether it runs on a local model, which costs nothing when its history names no cost
    pub local: bool,
    /// Whether its wait is a hold on its API key, which ranks it behind every agent that can start
    pub held: bool,
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
/// Those within the slack of the soonest finish come first, an agent held on
/// its API key after every one that is not. A listed agent comes first, in the
/// list's order. The rest go by cost: an agent with a cost comes first,
/// cheapest first, a local agent with no priced history counting as free.
/// Agents with none follow, lowest tier first as a stand-in for price. Ties go
/// soonest first, then by name. The agents past the slack follow, soonest first.
pub fn pick<'a>(item: &Item<'_>, candidates: &[Candidate<'a>]) -> Vec<Choice<'a>> {
    // Each fit beside the cost and tier it is ranked on, and whether it is held.
    let mut fits: Vec<(Choice<'a>, Option<f64>, Tier, bool)> = candidates
        .iter()
        .filter(|c| item.pin.is_none_or(|pin| pin == c.agent))
        .filter(|c| !item.leave_out.contains(c.agent))
        .filter(|c| takes(c.fit, item.kind) && item.tier.is_none_or(|t| c.fit.tier() >= t))
        .filter_map(|c| {
            let wait = c.wait?;
            let choice = Choice {
                agent: c.agent,
                finish: wait.saturating_add(c.typical.secs),
                wait,
                cost_usd: c.typical.cost_usd,
            };
            Some((
                choice,
                c.typical.cost_usd.or(c.local.then_some(0.0)),
                c.fit.tier(),
                c.held,
            ))
        })
        .collect();
    let Some(soonest) = fits.iter().map(|(c, ..)| c.finish).min() else {
        return Vec::new();
    };
    fits.sort_by_key(|(c, ..)| (c.finish, c.agent));
    let (mut eligible, rest): (Vec<_>, Vec<_>) =
        (fits.into_iter()).partition(|(c, ..)| within_slack(c.finish, soonest, item.slack));
    let rank = |agent: &AgentName| item.prefer.iter().position(|p| p == agent);
    eligible.sort_by(|(a, a_cost, a_tier, a_held), (b, b_cost, b_tier, b_held)| {
        a_held.cmp(b_held).then_with(|| {
            preferred(rank(a.agent), rank(b.agent))
                .then_with(|| cheaper(*a_cost, *b_cost, *a_tier, *b_tier))
        })
    });
    eligible.extend(rest);
    eligible.into_iter().map(|(choice, ..)| choice).collect()
}

/// Whether `finish` is within `slack` minutes of `soonest`, both seconds
/// from now, with [`DEFAULT_SLACK_MINUTES`] when `slack` is None
pub fn within_slack(finish: u64, soonest: u64, slack: Option<u32>) -> bool {
    let slack = u64::from(slack.unwrap_or(DEFAULT_SLACK_MINUTES)) * 60;
    finish <= soonest.saturating_add(slack)
}

/// Whether an agent `wait` seconds from starting can start within `slack`
/// minutes, which keeps a named agent on a new item and an item's own agent on
/// its turn however long either takes
pub fn starts_within_slack(wait: u64, slack: Option<u32>) -> bool {
    within_slack(wait, 0, slack)
}

// An item with no kind goes only to an agent whose file names no kinds
pub(super) fn takes(fit: &Fit, kind: Option<&str>) -> bool {
    match (&fit.kinds, kind) {
        (None, _) => true,
        (Some(kinds), Some(kind)) => kinds.iter().any(|k| k == kind),
        (Some(_), None) => false,
    }
}

// A listed agent beats an unlisted one, and an earlier place beats a later
fn preferred(a: Option<usize>, b: Option<usize>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

// A cost beats none; with none on both sides the lower tier stands in for the lower price
fn cheaper(a: Option<f64>, b: Option<f64>, a_tier: Tier, b_tier: Tier) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a_tier.cmp(&b_tier),
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
            local: false,
            held: false,
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
            ["all"],
            "an item with no kind goes only to an agent that takes every kind"
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
    fn a_local_agent_with_no_history_counts_as_free_and_beats_a_priced_one() {
        let (local, sonnet) = (name("local"), name("sonnet"));
        let any = Fit::default();
        let candidates = [
            candidate(&sonnet, &any, Some(0), 600, Some(0.5)),
            Candidate {
                local: true,
                ..candidate(&local, &any, Some(0), 600, None)
            },
        ];
        let choices = pick(&Item::default(), &candidates);
        assert_eq!(order(&choices), ["local", "sonnet"]);
        assert_eq!(
            choices[0].cost_usd, None,
            "its cost is ranked, not reported, as free"
        );
    }

    #[test]
    fn the_last_tie_is_the_agents_name() {
        let (alpha, beta, gamma) = (name("alpha"), name("beta"), name("gamma"));
        let any = Fit::default();
        let candidates = [
            candidate(&gamma, &any, Some(0), 600, Some(1.0)),
            candidate(&beta, &any, Some(0), 600, Some(1.0)),
            candidate(&alpha, &any, Some(0), 600, Some(1.0)),
        ];
        assert_eq!(
            order(&pick(&Item::default(), &candidates)),
            ["alpha", "beta", "gamma"]
        );
        let late = Item {
            slack: Some(0),
            ..Item::default()
        };
        let past = [
            candidate(&alpha, &any, Some(0), 600, Some(1.0)),
            candidate(&gamma, &any, Some(0), 900, Some(1.0)),
            candidate(&beta, &any, Some(0), 900, Some(1.0)),
        ];
        assert_eq!(order(&pick(&late, &past)), ["alpha", "beta", "gamma"]);
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

    #[test]
    fn no_history_hosted_agents_go_lowest_tier_first_whatever_their_names() {
        let (opus, sonnet, haiku) = (name("a-opus"), name("b-sonnet"), name("c-haiku"));
        let (heavy, standard, light) = (
            fit(Tier::Heavy, None),
            fit(Tier::Standard, None),
            fit(Tier::Light, None),
        );
        let candidates = [
            candidate(&opus, &heavy, Some(0), 600, None),
            candidate(&haiku, &light, Some(0), 600, None),
            candidate(&sonnet, &standard, Some(0), 600, None),
        ];
        let choices = pick(&Item::default(), &candidates);
        assert_eq!(order(&choices), ["c-haiku", "b-sonnet", "a-opus"]);
        assert!(
            choices.iter().all(|c| c.cost_usd.is_none()),
            "the tier ranks, it reports no cost"
        );
    }

    #[test]
    fn a_priced_agent_goes_before_every_agent_with_no_cost_and_equal_tiers_go_by_name() {
        let (zed, amy, priced, local) = (name("zed"), name("amy"), name("priced"), name("local"));
        let (light, heavy) = (fit(Tier::Light, None), fit(Tier::Heavy, None));
        let candidates = [
            candidate(&zed, &light, Some(0), 600, None),
            candidate(&amy, &light, Some(0), 600, None),
            candidate(&priced, &heavy, Some(0), 600, Some(9.0)),
            Candidate {
                local: true,
                ..candidate(&local, &heavy, Some(0), 600, None)
            },
        ];
        assert_eq!(
            order(&pick(&Item::default(), &candidates)),
            ["local", "priced", "amy", "zed"]
        );
    }

    #[test]
    fn a_listed_agent_within_the_slack_ranks_before_a_cheaper_unlisted_one() {
        let (listed, cheap) = (name("listed"), name("cheap"));
        let any = Fit::default();
        let candidates = [
            candidate(&cheap, &any, Some(0), 600, Some(0.1)),
            candidate(&listed, &any, Some(0), 900, Some(9.0)),
        ];
        let prefer = [listed.clone()];
        let item = Item {
            prefer: &prefer,
            ..Item::default()
        };
        assert_eq!(order(&pick(&item, &candidates)), ["listed", "cheap"]);
        assert_eq!(
            order(&pick(&Item::default(), &candidates)),
            ["cheap", "listed"],
            "with no list the cheaper goes first"
        );
    }

    #[test]
    fn listed_agents_rank_in_the_lists_order_whatever_they_cost() {
        let (a, b, c) = (name("a"), name("b"), name("c"));
        let any = Fit::default();
        let candidates = [
            candidate(&a, &any, Some(0), 600, Some(0.1)),
            candidate(&b, &any, Some(0), 600, Some(5.0)),
            candidate(&c, &any, Some(0), 600, Some(1.0)),
        ];
        let prefer = [c.clone(), b.clone(), c.clone(), a.clone()];
        let item = Item {
            prefer: &prefer,
            ..Item::default()
        };
        assert_eq!(
            order(&pick(&item, &candidates)),
            ["c", "b", "a"],
            "a name listed twice takes its first place"
        );
    }

    #[test]
    fn a_held_agent_within_the_slack_ranks_behind_every_agent_that_can_start() {
        let (held, listed, cheap, dear) =
            (name("held"), name("listed"), name("cheap"), name("dear"));
        let any = Fit::default();
        let candidates = [
            Candidate {
                held: true,
                ..candidate(&held, &any, Some(300), 600, Some(0.01))
            },
            candidate(&dear, &any, Some(0), 900, Some(9.0)),
            candidate(&cheap, &any, Some(0), 900, Some(1.0)),
            candidate(&listed, &any, Some(0), 900, Some(5.0)),
        ];
        let prefer = [held.clone(), listed.clone()];
        let listed_first = Item {
            prefer: &prefer,
            ..Item::default()
        };
        assert_eq!(
            order(&pick(&Item::default(), &candidates)),
            ["cheap", "listed", "dear", "held"],
            "it is the cheapest and finishes first, yet waits on the hold"
        );
        assert_eq!(
            order(&pick(&listed_first, &candidates)),
            ["listed", "cheap", "dear", "held"],
            "a listed agent on hold does not outrank one that can start"
        );
        let alone = [candidates[0]];
        assert_eq!(order(&pick(&Item::default(), &alone)), ["held"]);
    }

    #[test]
    fn a_listed_agent_past_the_slack_stays_behind_the_agents_within_it() {
        let (soon, late, later) = (name("soon"), name("late"), name("later"));
        let any = Fit::default();
        let candidates = [
            candidate(&later, &any, Some(0), 9000, Some(0.1)),
            candidate(&late, &any, Some(0), 5400, Some(0.1)),
            candidate(&soon, &any, Some(0), 600, Some(9.0)),
        ];
        let prefer = [later.clone(), late.clone()];
        let item = Item {
            prefer: &prefer,
            slack: Some(60),
            ..Item::default()
        };
        assert_eq!(
            order(&pick(&item, &candidates)),
            ["soon", "late", "later"],
            "past the slack they stay soonest first, whatever the list says"
        );
    }

    #[test]
    fn unlisted_agents_follow_the_listed_ones_in_todays_order() {
        let (listed, cheap, dear, unknown) =
            (name("listed"), name("cheap"), name("dear"), name("unknown"));
        let any = Fit::default();
        let candidates = [
            candidate(&unknown, &any, Some(0), 600, None),
            candidate(&dear, &any, Some(0), 600, Some(5.0)),
            candidate(&cheap, &any, Some(0), 600, Some(0.5)),
            candidate(&listed, &any, Some(0), 600, Some(9.0)),
        ];
        let prefer = [listed.clone()];
        let item = Item {
            prefer: &prefer,
            ..Item::default()
        };
        assert_eq!(
            order(&pick(&item, &candidates)),
            ["listed", "cheap", "dear", "unknown"]
        );
    }

    #[test]
    fn a_pin_and_a_left_out_agent_still_beat_the_list() {
        let (first, second) = (name("first"), name("second"));
        let any = Fit::default();
        let candidates = [
            candidate(&first, &any, Some(0), 600, Some(1.0)),
            candidate(&second, &any, Some(0), 600, Some(1.0)),
        ];
        let prefer = [first.clone(), second.clone()];
        let pinned = Item {
            pin: Some(&second),
            prefer: &prefer,
            ..Item::default()
        };
        assert_eq!(order(&pick(&pinned, &candidates)), ["second"]);
        let gone = [first.clone()];
        let handed = Item {
            leave_out: &gone,
            prefer: &prefer,
            ..Item::default()
        };
        assert_eq!(order(&pick(&handed, &candidates)), ["second"]);
    }
}
