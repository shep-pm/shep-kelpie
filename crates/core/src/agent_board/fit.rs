//! Each agent that does clearly worse at a kind of work than another that takes it
//!
//! A [`Finding`] is what an `agent-fit` ruling is raised from. It changes
//! nothing itself: the `kinds` it suggests are for the maintainer to write
//! into the agent file. [`fit`] reads every pair of agents whose files take a
//! kind and that have [`FIT_ITEMS`] items of it each, and calls one clearly
//! worse on the three measures a [`Record`] keeps.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::pick::takes;
use super::record::Record;
use crate::agents::Fit;

/// Items each of a pair must have of a kind before either is read against the other
pub const FIT_ITEMS: u32 = 10;

/// Items more the worse agent must finish of a kind before the same pair is read again
pub const FIT_AGAIN: u32 = 10;

/// A merge rate this far below another agent's is clearly worse
const MERGE_RATE_GAP: f64 = 0.15;

/// Median hours this many times another agent's are clearly worse
const HOURS_RATIO: f64 = 1.5;

/// A share of items with 3 or more fix turns this far above another agent's is clearly worse
const HEAVY_FIX_GAP: f64 = 0.15;

/// How one agent is clearly worse than another at the same kind of work
// wire format: changing this is a breaking change to the door
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Measure {
    /// Its items merged this much less often than the other agent's
    MergeRate {
        /// The worse agent's rate
        worse: f64,
        /// The other agent's
        better: f64,
    },
    /// Its merged items took this many times as long as the other agent's
    Hours {
        /// The worse agent's median hours
        worse: f64,
        /// The other agent's
        better: f64,
    },
    /// This much more of its items had 3 or more fix turns
    HeavyFix {
        /// The worse agent's share
        worse: f64,
        /// The other agent's
        better: f64,
    },
}

/// One agent clearly worse at a kind than another agent that takes it
// wire format: changing this is a breaking change to the door
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    /// The kind both agents are read at
    pub kind: String,
    /// The agent that does worse
    pub worse: String,
    /// The agent it is read against
    pub better: String,
    /// What the two records say
    pub measure: Measure,
    /// The `kinds` the worse agent's file should name instead: its own without
    /// this kind, or every kind the records name without it when its file
    /// names none. Empty when its file names only this kind.
    pub suggested_kinds: Vec<String>,
}

/// An agent the board reads, with what its file says it takes
#[derive(Debug, Clone, Copy)]
pub struct Taker<'a> {
    /// Its name, as its record is keyed
    pub agent: &'a str,
    /// Its file's kinds and tier
    pub fit: &'a Fit,
}

/// Each agent clearly worse at a kind than another agent that takes it
///
/// `takers` names every agent the board ranks with its file's kinds. Only the
/// agents whose file takes a kind are read at it, and only when each has
/// [`FIT_ITEMS`] or more items of it: one whose file names no `kinds` takes
/// every kind. Findings come out by kind, then by the worse agent, then by
/// the better one, each naming the first measure that says it.
pub fn fit<'a>(
    records: &BTreeMap<(String, Option<String>), Record>,
    takers: impl IntoIterator<Item = Taker<'a>>,
) -> Vec<Finding> {
    let takers: Vec<Taker<'a>> = takers.into_iter().collect();
    let kinds: BTreeSet<&str> = records
        .keys()
        .filter_map(|(_, kind)| kind.as_deref())
        .collect();
    let mut found = Vec::new();
    for kind in &kinds {
        let group = takes_kind(records, &takers, kind);
        for (worse, worse_record) in &group {
            for (better, better_record) in &group {
                if worse.agent == better.agent {
                    continue;
                }
                let Some(measure) = worse_than(worse_record, better_record) else {
                    continue;
                };
                found.push(Finding {
                    kind: (*kind).to_owned(),
                    worse: worse.agent.to_owned(),
                    better: better.agent.to_owned(),
                    measure,
                    suggested_kinds: suggested(worse.fit, kind, &kinds),
                });
            }
        }
    }
    found
}

// The agents `takers` names that take `kind` and have FIT_ITEMS items of it,
// by name.
fn takes_kind<'a, 'b, 'r>(
    records: &'r BTreeMap<(String, Option<String>), Record>,
    takers: &'b [Taker<'a>],
    kind: &str,
) -> Vec<(&'b Taker<'a>, &'r Record)> {
    let mut group: Vec<(&Taker<'a>, &Record)> = takers
        .iter()
        .filter(|taker| takes(taker.fit, Some(kind)))
        .filter_map(|taker| {
            let record = records.get(&(taker.agent.to_owned(), Some(kind.to_owned())))?;
            (record.items >= FIT_ITEMS).then_some((taker, record))
        })
        .collect();
    group.sort_by_key(|(taker, _)| taker.agent);
    group
}

// The `kinds` the worse agent's file should name: its own without this kind,
// or every kind the records name without it when its file names none.
fn suggested(fit: &Fit, kind: &str, kinds: &BTreeSet<&str>) -> Vec<String> {
    match &fit.kinds {
        Some(list) => (list.iter())
            .filter(|taken| *taken != kind)
            .map(str::to_owned)
            .collect(),
        None => kinds
            .iter()
            .copied()
            .filter(|taken| *taken != kind)
            .map(str::to_owned)
            .collect(),
    }
}

// The first measure that says `a` does clearly worse than `b`, or None. A
// measure a record cannot say compares nothing.
fn worse_than(a: &Record, b: &Record) -> Option<Measure> {
    if let (Some(worse), Some(better)) = (a.merge_rate(), b.merge_rate())
        && better - worse >= MERGE_RATE_GAP
    {
        return Some(Measure::MergeRate { worse, better });
    }
    if let (Some(worse), Some(better)) = (a.median_hours, b.median_hours)
        && better > 0.0
        && worse >= better * HOURS_RATIO
    {
        return Some(Measure::Hours { worse, better });
    }
    let (worse, better) = (a.heavy_fix_share(), b.heavy_fix_share());
    (worse - better >= HEAVY_FIX_GAP).then_some(Measure::HeavyFix { worse, better })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Kinds;

    // Each agent and kind of record, keyed as `agent_board::records` keys them
    fn tallies(entries: &[(&str, &str, Record)]) -> BTreeMap<(String, Option<String>), Record> {
        entries
            .iter()
            .map(|(agent, kind, record)| {
                (
                    ((*agent).to_owned(), Some((*kind).to_owned())),
                    record.clone(),
                )
            })
            .collect()
    }

    // An agent named `agent` whose file names `kinds`, every kind when none
    fn file(agent: &str, kinds: &[&str]) -> (String, Fit) {
        let named = (!kinds.is_empty()).then(|| {
            let listed: Vec<String> = kinds.iter().map(|kind| (*kind).to_owned()).collect();
            Kinds::try_from(listed).unwrap()
        });
        (
            agent.to_owned(),
            Fit {
                kinds: named,
                ..Default::default()
            },
        )
    }

    fn at<'a>(files: &'a [(String, Fit)]) -> impl Iterator<Item = Taker<'a>> + 'a {
        files.iter().map(|(agent, fit)| Taker { agent, fit })
    }

    // `items` items, `merged` of them merged, each merged one taking `hours`
    fn record(items: u32, merged: u32, hours: f64, heavy_fix_items: u32) -> Record {
        Record {
            items,
            merged,
            median_hours: Some(hours),
            heavy_fix_items,
            ..Default::default()
        }
    }

    #[test]
    fn an_agent_with_a_lower_merge_rate_at_ten_items_is_found() {
        let records = tallies(&[
            ("weak", "fix", record(10, 5, 1.0, 0)),
            ("strong", "fix", record(10, 10, 1.0, 0)),
        ]);
        let files = [file("weak", &["docs", "fix"]), file("strong", &["fix"])];

        let found = fit(&records, at(&files));

        assert_eq!(
            found,
            [Finding {
                kind: "fix".into(),
                worse: "weak".into(),
                better: "strong".into(),
                measure: Measure::MergeRate {
                    worse: 0.5,
                    better: 1.0,
                },
                suggested_kinds: vec!["docs".into()],
            }]
        );
    }

    #[test]
    fn an_agent_with_fewer_than_ten_items_is_never_found() {
        let records = tallies(&[
            ("weak", "fix", record(9, 0, 9.0, 9)),
            ("strong", "fix", record(10, 10, 0.1, 0)),
            ("both", "fix", record(9, 0, 9.0, 9)),
        ]);
        let files = [file("weak", &["fix"]), file("strong", &["fix"])];

        let found = fit(&records, at(&files));

        assert_eq!(found, [], "nine items of a kind say nothing");
    }

    #[test]
    fn a_file_with_no_kinds_is_suggested_every_kind_but_this_one() {
        let records = tallies(&[
            ("weak", "fix", record(10, 1, 1.0, 0)),
            ("strong", "fix", record(10, 10, 1.0, 0)),
            ("weak", "docs", record(10, 10, 1.0, 0)),
        ]);
        let files = [file("weak", &[]), file("strong", &["fix"])];

        let found = fit(&records, at(&files));

        assert_eq!(
            found,
            [Finding {
                kind: "fix".into(),
                worse: "weak".into(),
                better: "strong".into(),
                measure: Measure::MergeRate {
                    worse: 0.1,
                    better: 1.0,
                },
                suggested_kinds: vec!["docs".into()],
            }],
            "its file names no kinds, so the suggestion writes out every kind the records name"
        );
    }

    #[test]
    fn agents_within_the_thresholds_give_no_finding() {
        let records = tallies(&[
            ("slower", "fix", record(10, 9, 2.0, 2)),
            ("faster", "fix", record(10, 10, 1.5, 1)),
        ]);
        let files = [file("slower", &["fix"]), file("faster", &["fix"])];

        let found = fit(&records, at(&files));

        assert_eq!(
            found,
            [],
            "a tenth off the merge rate, a third more hours and a tenth more heavy fix turns"
        );
    }

    #[test]
    fn a_longer_median_and_a_heavier_fix_share_are_each_found() {
        let hours = tallies(&[
            ("slow", "fix", record(10, 10, 3.0, 0)),
            ("quick", "fix", record(10, 10, 1.0, 0)),
        ]);
        let fixes = tallies(&[
            ("often", "fix", record(10, 10, 1.0, 4)),
            ("seldom", "fix", record(10, 10, 1.0, 1)),
        ]);
        let slow = [file("slow", &["fix"]), file("quick", &["fix"])];
        let often = [file("often", &["fix"]), file("seldom", &["fix"])];

        let found = fit(&hours, at(&slow));
        let fixes_found = fit(&fixes, at(&often));

        assert_eq!(
            found[0].measure,
            Measure::Hours {
                worse: 3.0,
                better: 1.0,
            }
        );
        assert_eq!(
            fixes_found[0].measure,
            Measure::HeavyFix {
                worse: 0.4,
                better: 0.1,
            },
            "the share of its items with 3 or more fix turns"
        );
    }

    #[test]
    fn an_agent_whose_file_does_not_take_the_kind_is_read_against_no_one() {
        let records = tallies(&[
            ("docs_only", "fix", record(20, 0, 9.0, 9)),
            ("weak", "fix", record(10, 5, 1.0, 0)),
            ("strong", "fix", record(10, 10, 1.0, 0)),
        ]);
        let files = [
            file("docs_only", &["docs"]),
            file("weak", &["fix"]),
            file("strong", &["fix"]),
        ];

        let found = fit(&records, at(&files));

        let pairs: Vec<_> = found
            .iter()
            .map(|f| (f.worse.as_str(), f.better.as_str()))
            .collect();
        assert_eq!(pairs, [("weak", "strong")], "its record is read at no kind");
    }
}
