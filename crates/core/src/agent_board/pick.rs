//! Which of the agents that fit a work item the board reads, and the order to try them

use std::cmp::Ordering;

use super::Typical;
use crate::agents::{Fit, Tier};
use crate::lease::door::Pick;
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

/// The candidates that fit `item`, soonest finish first, then by name
///
/// A fit has a window, takes the item's kind, meets its tier, is not left
/// out, and is the pinned agent when the item has a pin. [`rank`] orders
/// them by cost and the rest.
pub fn pick<'a>(item: &Item<'_>, candidates: &[Candidate<'a>]) -> Vec<Choice<'a>> {
    let mut fits: Vec<_> = candidates
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
    fits.sort_by_key(|c| (c.finish, c.agent));
    fits
}

/// `picks` in the order the board tries them
///
/// An agent that can start within `slack` minutes (see [`starts_within_slack`])
/// is available, and the available come first: one held on its API key after
/// every one that is not, then a listed agent first, in the list's order, then
/// cost, a local agent with no priced history counting as free. An agent with
/// no cost follows, lowest tier first as a stand-in for price. Ties go soonest
/// finish first, then by name. The rest follow, soonest start first, then by name.
pub fn rank(picks: Vec<Pick>, slack: Option<u32>) -> Vec<Pick> {
    let (mut available, mut rest): (Vec<_>, Vec<_>) =
        (picks.into_iter()).partition(|p| starts_within_slack(p.wait, slack));
    available.sort_by(|a, b| {
        a.held
            .cmp(&b.held)
            .then_with(|| preferred(a.listed, b.listed))
            .then_with(|| cheaper(priced(a), priced(b), a.tier, b.tier))
            .then_with(|| (a.finish, &a.agent).cmp(&(b.finish, &b.agent)))
    });
    rest.sort_by(|a, b| (a.wait, &a.agent).cmp(&(b.wait, &b.agent)));
    available.extend(rest);
    available
}

// What an agent is ranked on: its cost, or $0 for a local agent with none
fn priced(pick: &Pick) -> Option<f64> {
    pick.cost_usd.or(pick.local.then_some(0.0))
}

/// Whether an agent `wait` seconds from starting can start within `slack`
/// minutes, with [`DEFAULT_SLACK_MINUTES`] when `slack` is None
///
/// This keeps a named agent on a new item and an item's own agent on its turn
/// however long either takes, and marks an agent available to [`rank`].
pub fn starts_within_slack(wait: u64, slack: Option<u32>) -> bool {
    wait <= u64::from(slack.unwrap_or(DEFAULT_SLACK_MINUTES)) * 60
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
fn cheaper(a: Option<f64>, b: Option<f64>, a_tier: Option<Tier>, b_tier: Option<Tier>) -> Ordering {
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

    // An agent the dog read, finishing in `finish` seconds at `cost_usd`, of the standard tier.
    fn read(agent: &AgentName, finish: u64, cost_usd: Option<f64>) -> Pick {
        Pick {
            cost_usd,
            tier: Some(Tier::Standard),
            ..Pick::new(agent.clone(), finish, 0)
        }
    }

    fn ranked(picks: Vec<Pick>, slack: Option<u32>) -> Vec<String> {
        rank(picks, slack)
            .iter()
            .map(|p| p.agent.to_string())
            .collect()
    }

    #[test]
    fn the_boards_answer_goes_soonest_first_whatever_the_cost_or_the_list() {
        let (local, dear, held, cheap) = (name("local"), name("dear"), name("held"), name("cheap"));
        let any = Fit::default();
        let candidates = [
            candidate(&cheap, &any, Some(0), 900, Some(0.1)),
            Candidate {
                held: true,
                ..candidate(&held, &any, Some(300), 300, Some(0.01))
            },
            candidate(&dear, &any, Some(0), 600, Some(9.0)),
            Candidate {
                local: true,
                ..candidate(&local, &any, Some(0), 1200, None)
            },
        ];
        let choices = pick(&Item::default(), &candidates);
        assert_eq!(order(&choices), ["dear", "held", "cheap", "local"]);
        assert_eq!(
            choices[0],
            Choice {
                agent: &dear,
                finish: 600,
                wait: 0,
                cost_usd: Some(9.0)
            }
        );
    }

    #[test]
    fn a_tie_on_the_finish_goes_by_name() {
        let (alpha, beta, gamma) = (name("alpha"), name("beta"), name("gamma"));
        let any = Fit::default();
        let candidates = [
            candidate(&gamma, &any, Some(0), 600, Some(1.0)),
            candidate(&alpha, &any, Some(100), 500, Some(5.0)),
            candidate(&beta, &any, Some(0), 600, None),
        ];
        assert_eq!(
            order(&pick(&Item::default(), &candidates)),
            ["alpha", "beta", "gamma"]
        );
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
        assert_eq!(order(&pick(&item, &candidates)), ["opus", "sonnet"]);
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
        assert_eq!(order(&pick(&item, &candidates)), ["all", "fixer"]);
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
    fn the_cheapest_agent_that_can_start_in_time_comes_first_however_late_it_finishes() {
        let (local, sonnet, opus, slow) =
            (name("local"), name("sonnet"), name("opus"), name("slow"));
        let picks = vec![
            read(&slow, 12_600, Some(0.1)),
            read(&opus, 1800, Some(3.0)),
            Pick {
                wait: 2400,
                ..read(&local, 6000, Some(0.2))
            },
            read(&sonnet, 2400, Some(1.2)),
        ];
        let ranked = rank(picks, None);
        let order: Vec<_> = ranked.iter().map(|p| p.agent.as_str()).collect();
        assert_eq!(order, ["slow", "local", "sonnet", "opus"]);
        assert_eq!(
            ranked[1],
            Pick {
                wait: 2400,
                ..read(&local, 6000, Some(0.2))
            },
            "ranking changes none of what it read"
        );
    }

    #[test]
    fn a_slack_of_zero_keeps_only_the_agents_free_now_in_front() {
        let (free, queued) = (name("free"), name("queued"));
        let picks = vec![
            Pick {
                wait: 1,
                ..read(&queued, 600, Some(0.1))
            },
            read(&free, 1800, Some(5.0)),
        ];
        assert_eq!(ranked(picks, Some(0)), ["free", "queued"]);
    }

    #[test]
    fn an_agent_past_the_slack_follows_soonest_start_first() {
        let (now, near, far, nearer_by_name) =
            (name("now"), name("near"), name("far"), name("a-near"));
        let waiting = |agent, wait, finish, cost_usd| Pick {
            wait,
            ..read(agent, finish, cost_usd)
        };
        let picks = vec![
            waiting(&far, 1200, 1300, Some(0.01)),
            waiting(&near, 700, 5000, Some(0.01)),
            waiting(&nearer_by_name, 700, 9000, Some(9.0)),
            read(&now, 3000, Some(9.0)),
        ];
        assert_eq!(
            ranked(picks, Some(10)),
            ["now", "a-near", "near", "far"],
            "past ten minutes they go by start, whatever they cost or finish"
        );
    }

    #[test]
    fn with_none_available_the_soonest_to_start_still_comes_first() {
        let (busy, queued) = (name("busy"), name("queued"));
        let picks = vec![
            Pick {
                wait: 900,
                ..read(&busy, 1000, Some(0.1))
            },
            Pick {
                wait: 300,
                ..read(&queued, 5000, Some(9.0))
            },
        ];
        assert_eq!(ranked(picks, Some(0)), ["queued", "busy"]);
    }

    #[test]
    fn no_agents_rank_as_none() {
        assert!(rank(Vec::new(), None).is_empty());
    }

    #[test]
    fn a_local_agent_with_no_history_counts_as_free_and_beats_a_priced_one() {
        let (local, sonnet) = (name("local"), name("sonnet"));
        let picks = vec![
            read(&sonnet, 600, Some(0.5)),
            Pick {
                local: true,
                ..read(&local, 600, None)
            },
        ];
        let ranked = rank(picks, None);
        assert_eq!(ranked[0].agent.as_str(), "local");
        assert_eq!(
            ranked[0].cost_usd, None,
            "its cost is ranked, not reported, as free"
        );
    }

    #[test]
    fn the_last_tie_is_the_agents_name() {
        let (alpha, beta, gamma) = (name("alpha"), name("beta"), name("gamma"));
        let picks = vec![
            read(&gamma, 600, Some(1.0)),
            read(&beta, 600, Some(1.0)),
            read(&alpha, 600, Some(1.0)),
        ];
        assert_eq!(ranked(picks, None), ["alpha", "beta", "gamma"]);
        let past = vec![
            read(&alpha, 600, Some(1.0)),
            read(&gamma, 900, Some(1.0)),
            read(&beta, 900, Some(1.0)),
        ];
        assert_eq!(ranked(past, Some(0)), ["alpha", "beta", "gamma"]);
    }

    #[test]
    fn an_equal_cost_goes_soonest_first_and_an_unknown_cost_last() {
        let (later, sooner, unknown) = (name("later"), name("sooner"), name("unknown"));
        let picks = vec![
            read(&unknown, 600, None),
            read(&later, 1200, Some(1.0)),
            read(&sooner, 900, Some(1.0)),
        ];
        assert_eq!(ranked(picks, None), ["sooner", "later", "unknown"]);
    }

    #[test]
    fn no_history_hosted_agents_go_lowest_tier_first_whatever_their_names() {
        let (opus, sonnet, haiku) = (name("a-opus"), name("b-sonnet"), name("c-haiku"));
        let tiered = |agent, tier| Pick {
            tier: Some(tier),
            ..read(agent, 600, None)
        };
        let picks = vec![
            tiered(&opus, Tier::Heavy),
            tiered(&haiku, Tier::Light),
            tiered(&sonnet, Tier::Standard),
        ];
        let ranked = rank(picks, None);
        let order: Vec<_> = ranked.iter().map(|p| p.agent.as_str()).collect();
        assert_eq!(order, ["c-haiku", "b-sonnet", "a-opus"]);
        assert!(
            ranked.iter().all(|p| p.cost_usd.is_none()),
            "the tier ranks, it reports no cost"
        );
    }

    #[test]
    fn a_priced_agent_goes_before_every_agent_with_no_cost_and_equal_tiers_go_by_name() {
        let (zed, amy, priced, local) = (name("zed"), name("amy"), name("priced"), name("local"));
        let tiered = |agent, tier, cost_usd| Pick {
            tier: Some(tier),
            ..read(agent, 600, cost_usd)
        };
        let picks = vec![
            tiered(&zed, Tier::Light, None),
            tiered(&amy, Tier::Light, None),
            tiered(&priced, Tier::Heavy, Some(9.0)),
            Pick {
                local: true,
                ..tiered(&local, Tier::Heavy, None)
            },
        ];
        assert_eq!(ranked(picks, None), ["local", "priced", "amy", "zed"]);
    }

    #[test]
    fn an_agent_with_no_tier_ranks_before_one_with_a_tier_when_neither_has_a_cost() {
        let (older, newer) = (name("older"), name("newer"));
        let picks = vec![
            read(&newer, 600, None),
            Pick {
                tier: None,
                ..read(&older, 600, None)
            },
        ];
        assert_eq!(ranked(picks, None), ["older", "newer"]);
    }

    #[test]
    fn a_listed_agent_within_the_slack_ranks_before_a_cheaper_unlisted_one() {
        let (listed, cheap) = (name("listed"), name("cheap"));
        let on_the_list = Pick {
            listed: Some(0),
            ..read(&listed, 900, Some(9.0))
        };
        let picks = vec![read(&cheap, 600, Some(0.1)), on_the_list.clone()];
        assert_eq!(ranked(picks, None), ["listed", "cheap"]);
        let unlisted = vec![read(&cheap, 600, Some(0.1)), read(&listed, 900, Some(9.0))];
        assert_eq!(
            ranked(unlisted, None),
            ["cheap", "listed"],
            "with no list the cheaper goes first"
        );
    }

    #[test]
    fn listed_agents_rank_in_the_lists_order_whatever_they_cost() {
        let (a, b, c) = (name("a"), name("b"), name("c"));
        let listed = |agent, place, cost_usd| Pick {
            listed: Some(place),
            ..read(agent, 600, cost_usd)
        };
        let picks = vec![
            listed(&a, 2, Some(0.1)),
            listed(&b, 1, Some(5.0)),
            listed(&c, 0, Some(1.0)),
        ];
        assert_eq!(ranked(picks, None), ["c", "b", "a"]);
    }

    #[test]
    fn a_held_agent_within_the_slack_ranks_behind_every_agent_that_can_start() {
        let (held, listed, cheap, dear) =
            (name("held"), name("listed"), name("cheap"), name("dear"));
        let held_first = |place| Pick {
            held: true,
            listed: place,
            wait: 300,
            ..read(&held, 900, Some(0.01))
        };
        let picks = |place| {
            vec![
                held_first(place),
                read(&dear, 900, Some(9.0)),
                read(&cheap, 900, Some(1.0)),
                Pick {
                    listed: place.map(|p| p + 1),
                    ..read(&listed, 900, Some(5.0))
                },
            ]
        };
        assert_eq!(
            ranked(picks(None), None),
            ["cheap", "listed", "dear", "held"],
            "it is the cheapest and finishes first, yet waits on the hold"
        );
        assert_eq!(
            ranked(picks(Some(0)), None),
            ["listed", "cheap", "dear", "held"],
            "a listed agent on hold does not outrank one that can start"
        );
        assert_eq!(ranked(vec![held_first(None)], None), ["held"]);
    }

    #[test]
    fn a_listed_agent_past_the_slack_stays_behind_the_agents_within_it() {
        let (soon, late, later) = (name("soon"), name("late"), name("later"));
        let listed = |agent, place, wait| Pick {
            listed: Some(place),
            wait,
            ..read(agent, wait + 600, Some(0.1))
        };
        let picks = vec![
            listed(&later, 0, 5400),
            listed(&late, 1, 4200),
            read(&soon, 600, Some(9.0)),
        ];
        assert_eq!(
            ranked(picks, Some(60)),
            ["soon", "late", "later"],
            "past the slack they go soonest start first, whatever the list says"
        );
    }

    #[test]
    fn unlisted_agents_follow_the_listed_ones_in_todays_order() {
        let (listed, cheap, dear, unknown) =
            (name("listed"), name("cheap"), name("dear"), name("unknown"));
        let picks = vec![
            read(&unknown, 600, None),
            read(&dear, 600, Some(5.0)),
            read(&cheap, 600, Some(0.5)),
            Pick {
                listed: Some(0),
                ..read(&listed, 600, Some(9.0))
            },
        ];
        assert_eq!(ranked(picks, None), ["listed", "cheap", "dear", "unknown"]);
    }
}
