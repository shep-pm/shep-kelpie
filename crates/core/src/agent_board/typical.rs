//! An agent's typical time and cost, from the ledger's merged items

use crate::agents::Tier;
use crate::usage::FinishedLine;

/// The merged items a median needs before the board trusts it
pub const SAMPLES: usize = 5;

/// The typical seconds of an item or a turn when the ledger has too few to say
pub const DEFAULT_SECS: u64 = 3600;

/// What an agent typically takes to finish a work item
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Typical {
    /// Seconds from its start to its finish
    pub secs: u64,
    /// What it costs in US dollars, or None when the ledger cannot say
    pub cost_usd: Option<f64>,
}

impl Default for Typical {
    fn default() -> Self {
        Self {
            secs: DEFAULT_SECS,
            cost_usd: None,
        }
    }
}

/// The ledger's merged items, each with its agent's tier
#[derive(Debug)]
pub struct History<'a> {
    samples: Vec<Sample<'a>>,
}

#[derive(Debug)]
struct Sample<'a> {
    agent: Option<&'a str>,
    tier: Option<Tier>,
    kind: Option<&'a str>,
    wall: u64,
    cost_usd: Option<f64>,
    turn: Option<u64>,
}

impl<'a> History<'a> {
    /// The merged items among `lines`, with `tier_of` naming each agent's tier
    ///
    /// An item whose agent `tier_of` does not know counts toward no tier.
    pub fn new(
        lines: impl IntoIterator<Item = &'a FinishedLine>,
        tier_of: impl Fn(&str) -> Option<Tier>,
    ) -> Self {
        let samples = lines
            .into_iter()
            .filter(|line| line.merged)
            .map(|line| {
                let agent = line.agent.as_deref();
                Sample {
                    agent,
                    tier: agent.and_then(&tier_of),
                    kind: line.kind.as_deref(),
                    wall: line.wall,
                    cost_usd: (line.worker.unpriced_calls == 0
                        && line.reviewer.unpriced_calls == 0)
                        .then_some(line.cost_usd),
                    turn: (line.worker.calls > 0).then(|| line.wall / u64::from(line.worker.calls)),
                }
            })
            .collect();
        Self { samples }
    }

    /// What `agent`, of `tier`, typically takes for an item of `kind`
    ///
    /// The medians of the first level with [`SAMPLES`] items: its own of the
    /// kind, its tier's of the kind, its tier's of any kind. Else
    /// [`Typical::default`]. The cost is the priced items' median.
    pub fn item(&self, agent: &str, tier: Tier, kind: Option<&str>) -> Typical {
        let Some(samples) = self.level(agent, tier, kind, |_| true) else {
            return Typical::default();
        };
        let mut walls: Vec<u64> = samples.iter().map(|s| s.wall).collect();
        walls.sort_unstable();
        let mut costs: Vec<f64> = samples.iter().filter_map(|s| s.cost_usd).collect();
        costs.sort_by(f64::total_cmp);
        Typical {
            secs: middle(&walls, u64::midpoint),
            cost_usd: (!costs.is_empty()).then(|| middle(&costs, f64::midpoint)),
        }
    }

    /// How long one of `agent`'s worker turns typically takes, by the chain of [`History::item`]
    ///
    /// An item's turn is its wall time over its worker calls, since the
    /// ledger keeps no worker seconds. [`DEFAULT_SECS`] when no level holds enough.
    pub fn turn_secs(&self, agent: &str, tier: Tier, kind: Option<&str>) -> u64 {
        self.level(agent, tier, kind, |s| s.turn.is_some())
            .map_or(DEFAULT_SECS, |samples| {
                let mut turns: Vec<u64> = samples.iter().filter_map(|s| s.turn).collect();
                turns.sort_unstable();
                middle(&turns, u64::midpoint)
            })
    }

    // The first of the chain's levels that holds `SAMPLES` of the items `keep` takes
    fn level(
        &self,
        agent: &str,
        tier: Tier,
        kind: Option<&str>,
        keep: impl Fn(&Sample<'_>) -> bool,
    ) -> Option<Vec<&Sample<'a>>> {
        let own = |s: &Sample<'_>| kind.is_some() && s.kind == kind && s.agent == Some(agent);
        let tiers_kind = |s: &Sample<'_>| kind.is_some() && s.kind == kind && s.tier == Some(tier);
        let tiers = |s: &Sample<'_>| s.tier == Some(tier);
        let levels: [&dyn Fn(&Sample<'_>) -> bool; 3] = [&own, &tiers_kind, &tiers];
        levels
            .into_iter()
            .map(|at| {
                self.samples
                    .iter()
                    .filter(|s| keep(s) && at(s))
                    .collect::<Vec<_>>()
            })
            .find(|samples| samples.len() >= SAMPLES)
    }
}

// The middle of `sorted`, which is not empty, or `mean` of the middle two
pub(super) fn middle<T: Copy>(sorted: &[T], mean: impl Fn(T, T) -> T) -> T {
    let at = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        mean(sorted[at - 1], sorted[at])
    } else {
        sorted[at]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_board::{Candidate, Item, pick};
    use crate::agents::Fit;
    use crate::ports::{Timestamp, Usage};
    use crate::settings::AgentName;
    use crate::test::ticket;
    use crate::usage::{HeldFindings, RoleLine};
    use crate::work_item::{CiFailures, QwenTally};

    fn line(agent: &str, kind: Option<&str>, wall: u64, cost_usd: f64) -> FinishedLine {
        let role = |calls| RoleLine {
            calls,
            tokens: Usage::default(),
            units: 0,
            cost_usd: 0.0,
            unpriced_calls: 0,
        };
        FinishedLine {
            at: Timestamp(1),
            issue: ticket(1),
            title: "t".into(),
            pull_request: Some(1),
            merged: true,
            closed: false,
            wall,
            units: 0,
            cost_usd,
            worker: role(4),
            reviewer: role(1),
            qwen: QwenTally::default(),
            review_rounds: 0,
            fix_turns: 0,
            rulings: 0,
            agent: Some(agent.into()),
            harness: None,
            model: None,
            kind: kind.map(str::to_owned),
            tier: None,
            ci_fix_turns: 0,
            ci_failures: CiFailures::default(),
            held: HeldFindings::default(),
            disputed: 0,
            deferred: 0,
        }
    }

    fn tier_of(agent: &str) -> Option<Tier> {
        match agent {
            "local" | "small" => Some(Tier::Light),
            "sonnet" | "opus" => Some(Tier::Standard),
            _ => None,
        }
    }

    // `n` merged items of `agent` and `kind`, the i-th taking 100 * (i + 1) seconds and i dollars
    fn items(agent: &str, kind: Option<&str>, n: u64) -> Vec<FinishedLine> {
        (0..n)
            .map(|i| line(agent, kind, 100 * (i + 1), i as f64))
            .collect()
    }

    fn typical(secs: u64, cost_usd: f64) -> Typical {
        Typical {
            secs,
            cost_usd: Some(cost_usd),
        }
    }

    #[test]
    fn an_agent_with_five_items_of_a_kind_takes_their_median() {
        let mut lines = items("local", Some("fix"), 5);
        lines.extend(items("small", Some("fix"), 5).into_iter().map(|mut l| {
            l.wall *= 10;
            l
        }));
        let history = History::new(&lines, tier_of);
        assert_eq!(
            history.item("local", Tier::Light, Some("fix")),
            typical(300, 2.0)
        );
    }

    #[test]
    fn an_agent_with_four_items_of_a_kind_takes_its_tiers_median_of_that_kind() {
        let mut lines = items("local", Some("fix"), 4);
        lines.push(line("small", Some("fix"), 5000, 9.0));
        lines.extend(items("small", Some("docs"), 5));
        let history = History::new(&lines, tier_of);
        // local's 100..400 and small's 5000: the median of five is the third
        assert_eq!(
            history.item("local", Tier::Light, Some("fix")),
            typical(300, 2.0)
        );
    }

    #[test]
    fn a_tier_with_four_items_of_a_kind_takes_its_median_of_every_kind() {
        let mut lines = items("local", Some("fix"), 2);
        lines.extend(items("small", Some("fix"), 2));
        lines.push(line("small", Some("docs"), 900, 7.0));
        lines.extend(items("sonnet", Some("fix"), 5));
        let history = History::new(&lines, tier_of);
        // 100, 100, 200, 200, 900: another tier's items never count
        assert_eq!(
            history.item("local", Tier::Light, Some("fix")),
            typical(200, 1.0)
        );
    }

    #[test]
    fn a_tier_with_four_items_of_any_kind_takes_the_default() {
        let mut lines = items("local", Some("fix"), 4);
        lines.extend(items("sonnet", Some("fix"), 5));
        lines.extend(items("stranger", Some("fix"), 5));
        let history = History::new(&lines, tier_of);
        assert_eq!(
            history.item("local", Tier::Light, Some("fix")),
            Typical {
                secs: DEFAULT_SECS,
                cost_usd: None
            }
        );
    }

    #[test]
    fn an_even_count_takes_the_mean_of_the_middle_two() {
        let lines = items("opus", Some("feat"), 6);
        let history = History::new(&lines, tier_of);
        assert_eq!(
            history.item("opus", Tier::Standard, Some("feat")),
            typical(350, 2.5)
        );
    }

    #[test]
    fn a_kindless_item_counts_only_toward_its_tiers_every_kind() {
        let mut lines = items("sonnet", None, 5);
        lines.extend(items("sonnet", Some("fix"), 1).into_iter().map(|mut l| {
            l.wall = 7000;
            l
        }));
        let history = History::new(&lines, tier_of);
        // six of the tier's items: 100..500 and 7000
        assert_eq!(
            history.item("sonnet", Tier::Standard, Some("fix")),
            typical(350, 1.5)
        );
        assert_eq!(
            history.item("sonnet", Tier::Standard, None),
            typical(350, 1.5),
            "an item with no kind starts at its tier's every kind"
        );
    }

    #[test]
    fn an_unmerged_item_never_counts() {
        let mut lines = items("opus", Some("fix"), 5);
        lines[4].merged = false;
        lines.extend(items("sonnet", Some("docs"), 5).into_iter().map(|mut l| {
            l.wall *= 10;
            l
        }));
        let history = History::new(&lines, tier_of);
        // opus's four merged and sonnet's five: 100..400 and 1000..5000
        assert_eq!(
            history.item("opus", Tier::Standard, Some("fix")),
            typical(1000, 2.0)
        );
    }

    #[test]
    fn the_cost_is_the_median_of_the_priced_items_alone() {
        let mut lines = items("opus", Some("fix"), 5);
        lines[0].worker.unpriced_calls = 1;
        lines[1].reviewer.unpriced_calls = 2;
        let history = History::new(&lines, tier_of);
        // the priced items cost 2, 3 and 4
        assert_eq!(
            history.item("opus", Tier::Standard, Some("fix")),
            typical(300, 3.0)
        );
    }

    #[test]
    fn an_unpriced_agent_loses_the_pick_to_a_priced_one() {
        let mut lines = items("local", Some("fix"), 5);
        for line in &mut lines {
            line.worker.unpriced_calls = 1;
            line.cost_usd = 0.0;
        }
        lines.extend(items("opus", Some("fix"), 5));
        let history = History::new(&lines, tier_of);
        let unpriced = history.item("local", Tier::Light, Some("fix"));
        assert_eq!(unpriced.cost_usd, None);
        let (local, opus) = (
            AgentName::try_from("local".to_owned()).unwrap(),
            AgentName::try_from("opus".to_owned()).unwrap(),
        );
        let any = Fit::default();
        let candidates = [
            Candidate {
                agent: &local,
                fit: &any,
                wait: Some(0),
                typical: unpriced,
                local: false,
            },
            Candidate {
                agent: &opus,
                fit: &any,
                wait: Some(0),
                typical: history.item("opus", Tier::Standard, Some("fix")),
                local: false,
            },
        ];
        let order: Vec<&str> = pick(&Item::default(), &candidates)
            .iter()
            .map(|c| c.agent.as_str())
            .collect();
        assert_eq!(order, ["opus", "local"]);
    }

    #[test]
    fn a_turn_falls_back_to_its_tiers_kind_and_then_its_tiers_every_kind() {
        let mut lines = items("local", Some("fix"), 2);
        lines.extend(items("small", Some("fix"), 3));
        lines.extend(items("small", Some("docs"), 5).into_iter().map(|mut l| {
            l.wall *= 10;
            l
        }));
        let history = History::new(&lines, tier_of);
        // light fix: 100, 200, 100, 200, 300 over four calls each
        assert_eq!(history.turn_secs("local", Tier::Light, Some("fix")), 50);
        // light any kind adds 1000..5000: the middle two turns are 75 and 250
        assert_eq!(history.turn_secs("local", Tier::Light, Some("test")), 162);
        assert_eq!(history.turn_secs("local", Tier::Light, None), 162);
    }

    #[test]
    fn a_turn_is_an_items_wall_time_over_its_worker_calls() {
        let mut lines = items("opus", Some("fix"), 5);
        lines.push(line("opus", Some("fix"), 9000, 0.0));
        lines[5].worker.calls = 0;
        let history = History::new(&lines, tier_of);
        // 100..500 over four calls each; the item with no calls counts for no turn
        assert_eq!(history.turn_secs("opus", Tier::Standard, Some("fix")), 75);
        assert_eq!(
            history.turn_secs("opus", Tier::Heavy, Some("docs")),
            DEFAULT_SECS
        );
    }
}
