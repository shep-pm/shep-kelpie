//! An agent's track record per kind of work, from the ledger's finished lines

use std::collections::BTreeMap;

use super::typical::middle;
use crate::usage::FinishedLine;

/// The fix turns that make an item a heavy one
pub const HEAVY_FIX_TURNS: u32 = 3;

/// How one agent did at one kind of work
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    /// Every finished item
    pub items: u32,
    /// The items whose pull request merged
    pub merged: u32,
    /// The items whose issue was closed with no change
    pub closed: u32,
    /// The median hours from creation to merge, None with no merged item
    pub median_hours: Option<f64>,
    /// Reviewers' rounds that came back with an answer
    pub review_rounds: u32,
    /// Worker turns sent to fix a round's held findings
    pub fix_turns: u32,
    /// The items with [`HEAVY_FIX_TURNS`] or more fix turns
    pub heavy_fix_items: u32,
    /// Rulings raised
    pub rulings: u32,
    /// The median cost in US dollars of the merged items with every call priced
    pub median_cost_usd: Option<f64>,
}

impl Record {
    /// The share of the items not closed with no change that merged, None with no such item
    pub fn merge_rate(&self) -> Option<f64> {
        let decided = self.items - self.closed;
        (decided > 0).then(|| f64::from(self.merged) / f64::from(decided))
    }
}

/// The record of each agent and kind among `lines`, a line with no kind under None
///
/// A line with no agent counts toward no record.
pub fn records<'a>(
    lines: impl IntoIterator<Item = &'a FinishedLine>,
) -> BTreeMap<(String, Option<String>), Record> {
    let mut tallies: BTreeMap<(String, Option<String>), Tally> = BTreeMap::new();
    for line in lines {
        let Some(agent) = &line.agent else { continue };
        let key = (agent.clone(), line.kind.clone());
        tallies.entry(key).or_default().add(line);
    }
    tallies
        .into_iter()
        .map(|(key, tally)| (key, tally.record()))
        .collect()
}

#[derive(Default)]
struct Tally {
    record: Record,
    walls: Vec<u64>,
    costs: Vec<f64>,
}

impl Tally {
    fn add(&mut self, line: &FinishedLine) {
        let record = &mut self.record;
        record.items += 1;
        record.closed += u32::from(line.closed);
        record.review_rounds += line.review_rounds;
        record.fix_turns += line.fix_turns;
        record.heavy_fix_items += u32::from(line.fix_turns >= HEAVY_FIX_TURNS);
        record.rulings += line.rulings;
        if line.merged {
            record.merged += 1;
            self.walls.push(line.wall);
            if line.worker.unpriced_calls == 0 && line.reviewer.unpriced_calls == 0 {
                self.costs.push(line.cost_usd);
            }
        }
    }

    fn record(mut self) -> Record {
        self.walls.sort_unstable();
        self.costs.sort_by(f64::total_cmp);
        Record {
            median_hours: (!self.walls.is_empty())
                .then(|| middle(&self.walls, u64::midpoint) as f64 / 3600.0),
            median_cost_usd: (!self.costs.is_empty()).then(|| middle(&self.costs, f64::midpoint)),
            ..self.record
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // A finished line of `agent` and `kind` that merged in `wall` seconds
    fn line(agent: Option<&str>, kind: Option<&str>, wall: u64) -> FinishedLine {
        let role = json!({
            "calls": 1, "units": 0, "cost_usd": 0.0,
            "tokens": { "input": 0, "cache_write": 0, "cache_read": 0, "output": 0 },
        });
        serde_json::from_value(json!({
            "at": 1, "issue": 1, "title": "t", "pull_request": 1, "merged": true,
            "wall": wall, "units": 0, "cost_usd": 1.0, "worker": role, "reviewer": role,
            "qwen": { "rounds": 0, "seconds": 0 }, "review_rounds": 0, "fix_turns": 0,
            "rulings": 0, "agent": agent, "kind": kind,
        }))
        .unwrap()
    }

    fn key(agent: &str, kind: Option<&str>) -> (String, Option<String>) {
        (agent.to_owned(), kind.map(str::to_owned))
    }

    #[test]
    fn a_record_counts_each_finished_line_of_its_agent_and_kind() {
        let mut merged = line(Some("opus"), Some("fix"), 7200);
        merged.review_rounds = 2;
        merged.fix_turns = 1;
        merged.rulings = 1;
        let mut priced = line(Some("opus"), Some("fix"), 3600);
        priced.cost_usd = 3.0;
        let mut unpriced = line(Some("opus"), Some("fix"), 3600);
        unpriced.worker.unpriced_calls = 1;
        unpriced.cost_usd = 100.0;
        let mut dropped = line(Some("opus"), Some("fix"), 9000);
        dropped.merged = false;
        dropped.review_rounds = 1;
        let other_kind = line(Some("opus"), Some("docs"), 60);
        let other_agent = line(Some("sonnet"), Some("fix"), 60);
        let kindless = line(Some("opus"), None, 1800);

        let lines = [
            merged,
            priced,
            unpriced,
            dropped,
            other_kind,
            other_agent,
            kindless,
        ];
        let records = records(&lines);

        assert_eq!(
            records[&key("opus", Some("fix"))],
            Record {
                items: 4,
                merged: 3,
                closed: 0,
                median_hours: Some(1.0),
                review_rounds: 3,
                fix_turns: 1,
                heavy_fix_items: 0,
                rulings: 1,
                median_cost_usd: Some(2.0),
            }
        );
        assert_eq!(records[&key("opus", Some("fix"))].merge_rate(), Some(0.75));
        assert_eq!(records[&key("opus", Some("docs"))].items, 1);
        assert_eq!(records[&key("sonnet", Some("fix"))].items, 1);
        assert_eq!(records[&key("opus", None)].median_hours, Some(0.5));
        assert_eq!(records.len(), 4);
    }

    #[test]
    fn an_item_closed_with_no_change_counts_toward_neither_side_of_the_merge_rate() {
        let mut closed = line(Some("opus"), Some("fix"), 60);
        closed.merged = false;
        closed.closed = true;
        let mut dropped = line(Some("opus"), Some("fix"), 60);
        dropped.merged = false;
        let lines = [
            line(Some("opus"), Some("fix"), 60),
            dropped,
            closed.clone(),
            closed,
        ];

        let record = &records(&lines)[&key("opus", Some("fix"))];

        assert_eq!((record.items, record.merged, record.closed), (4, 1, 2));
        assert_eq!(record.merge_rate(), Some(0.5));
    }

    #[test]
    fn an_agent_with_only_closed_items_has_no_merge_rate() {
        let mut closed = line(Some("opus"), Some("fix"), 60);
        closed.merged = false;
        closed.closed = true;

        let records = records(&[closed]);

        assert_eq!(records[&key("opus", Some("fix"))].merge_rate(), None);
    }

    #[test]
    fn the_items_with_three_or_more_fix_turns_count_each_item_once() {
        let mut lines = vec![
            line(Some("opus"), Some("fix"), 60),
            line(Some("opus"), Some("fix"), 60),
            line(Some("opus"), Some("fix"), 60),
        ];
        lines[0].fix_turns = 2;
        lines[1].fix_turns = 3;
        lines[2].fix_turns = 7;

        let record = &records(&lines)[&key("opus", Some("fix"))];

        assert_eq!((record.fix_turns, record.heavy_fix_items), (12, 2));
    }

    #[test]
    fn a_line_with_no_agent_counts_toward_no_record() {
        let lines = [line(None, Some("fix"), 60), line(None, None, 60)];

        assert!(records(&lines).is_empty());
    }
}
