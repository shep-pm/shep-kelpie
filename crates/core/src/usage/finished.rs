//! A finished work item's line in the usage ledger: what it spent, how often
//! it went round its loop, and what its worker ran on

use serde::{Deserialize, Serialize};

use super::{RoleLine, is_zero};
use crate::agents::Tier;
use crate::ports::{Severity, Timestamp};
use crate::ticket::TicketId;
use crate::work_item::{CiFailures, QwenTally, WorkItem};

/// A finished work item, merged, closed with no change or dropped, and what
/// it spent
// wire format: changing this is a breaking change to the usage ledger
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinishedLine {
    /// When it finished
    pub at: Timestamp,
    /// The ticket it resolved
    pub issue: TicketId,
    /// The issue's title
    pub title: String,
    /// Its pull request, if it had one
    pub pull_request: Option<u64>,
    /// Whether the pull request merged
    pub merged: bool,
    /// Whether its issue was closed with no change, so it ended with no
    /// pull request
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
    /// Seconds from its creation to `at`
    pub wall: u64,
    /// Every role's units together
    pub units: u64,
    /// Every role's priced calls together, in US dollars
    pub cost_usd: f64,
    /// The worker's turns
    pub worker: RoleLine,
    /// The reviewers' sessions
    pub reviewer: RoleLine,
    /// Its local rounds and their seconds
    pub qwen: QwenTally,
    /// Reviewers' rounds that came back with an answer
    pub review_rounds: u32,
    /// Worker turns sent to fix a round's held findings
    pub fix_turns: u32,
    /// Rulings raised for it
    pub rulings: u32,
    /// The agent its worker ran on. None in a line from before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// That agent's harness as its file stood at the end, None where kelpie
    /// no longer runs it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// That agent's model, the same way
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The kind its issue's label named, as `fix`. None for an issue with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The least tier the project manager's pick said it needed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    /// Worker turns sent to fix a red CI run
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ci_fix_turns: u32,
    /// Its pull request's heads the gate read red, by the tests that failed
    #[serde(default, skip_serializing_if = "CiFailures::is_zero")]
    pub ci_failures: CiFailures,
    /// The findings above a nit its worker was sent, each once across rounds
    #[serde(default, skip_serializing_if = "HeldFindings::is_zero")]
    pub held: HeldFindings,
    /// The findings its worker disputed, each once per round it was sent
    #[serde(default, skip_serializing_if = "is_zero")]
    pub disputed: u32,
    /// The findings its worker deferred, counted the same way
    #[serde(default, skip_serializing_if = "is_zero")]
    pub deferred: u32,
}

/// A finished work item's findings above a nit, by severity
// wire format: changing this is a breaking change to the usage ledger
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldFindings {
    /// A real defect
    #[serde(default, skip_serializing_if = "is_zero")]
    pub high: u32,
    /// Worth fixing before merge
    #[serde(default, skip_serializing_if = "is_zero")]
    pub medium: u32,
}

impl HeldFindings {
    /// Whether none was held
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }
}

impl FinishedLine {
    /// The line for `item`, which finished as `record` says. Its harness and
    /// model are the caller's to fill, from the agent's file.
    pub fn of(record: &crate::state::Finished, item: &WorkItem, ticket: TicketId) -> Self {
        let tally = item.tally();
        let mut held = HeldFindings::default();
        for finding in &item.held {
            let severity = match finding.severity {
                Severity::High => &mut held.high,
                Severity::Medium => &mut held.medium,
                Severity::Low => continue,
            };
            *severity = severity.saturating_add(1);
        }
        Self {
            at: record.at,
            issue: ticket,
            title: record.title.clone(),
            pull_request: record.pull_request,
            merged: record.merged,
            closed: record.closed,
            wall: record.wall,
            units: tally.units(),
            cost_usd: tally.cost().usd(),
            worker: tally.worker.into(),
            reviewer: tally.reviewer.into(),
            qwen: tally.qwen,
            review_rounds: tally.counts.review_rounds,
            fix_turns: tally.counts.fix_turns,
            rulings: tally.counts.rulings,
            agent: Some(item.agent.as_str().to_owned()),
            harness: None,
            model: None,
            kind: item.kind.clone(),
            tier: item.tier,
            ci_fix_turns: tally.counts.ci_fix_turns,
            ci_failures: tally.counts.ci_failures,
            held,
            disputed: tally.counts.disputed,
            deferred: tally.counts.deferred,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ports::Finding;
    use crate::state::Finished;
    use crate::test::a_work_item;
    use crate::work_item::Seconds;

    // The harness and model are left to the runner, which reads the agent's file.
    #[test]
    fn a_finished_line_counts_held_findings_by_severity_and_its_red_heads() {
        let mut item = a_work_item();
        let finding = |severity| Finding {
            severity,
            file: "src/a.rs".into(),
            line: 1,
            what: "w".into(),
            why: "y".into(),
            test: None,
        };
        item.held = vec![
            finding(Severity::High),
            finding(Severity::Medium),
            finding(Severity::Medium),
        ];
        item.counts.ci_fix_turns = 1;
        item.counts.ci_failures.runnable = 2;
        let record = Finished {
            issue: item.issue,
            title: item.title.clone(),
            pull_request: Some(71),
            merged: true,
            closed: false,
            at: Timestamp(9),
            wall: 9,
            seconds: Seconds::default(),
            spend: None,
        };
        let line = serde_json::to_value(FinishedLine::of(&record, &item, item.issue)).unwrap();
        assert_eq!(line["agent"], json!(item.agent.as_str()));
        assert_eq!(
            (&line["held"], &line["ci_failures"], &line["ci_fix_turns"]),
            (
                &json!({ "high": 1, "medium": 2 }),
                &json!({ "runnable": 2 }),
                &json!(1)
            )
        );
        assert_eq!((line.get("harness"), line.get("model")), (None, None));
    }

    #[test]
    fn a_finished_line_shows_disputed_and_deferred_and_neither_at_zero() {
        let mut item = a_work_item();
        let record = Finished {
            issue: item.issue,
            title: item.title.clone(),
            pull_request: Some(71),
            merged: true,
            closed: false,
            at: Timestamp(9),
            wall: 9,
            seconds: Seconds::default(),
            spend: None,
        };
        let line = serde_json::to_value(FinishedLine::of(&record, &item, item.issue)).unwrap();
        assert_eq!((line.get("disputed"), line.get("deferred")), (None, None));

        item.counts.disputed = 1;
        item.counts.deferred = 2;
        let line = serde_json::to_value(FinishedLine::of(&record, &item, item.issue)).unwrap();
        assert_eq!(
            (&line["disputed"], &line["deferred"]),
            (&json!(1), &json!(2))
        );
    }

    #[test]
    fn a_finished_line_names_its_kind_and_tier_and_a_line_from_before_reads_without_them() {
        let mut item = a_work_item();
        let record = Finished {
            issue: item.issue,
            title: item.title.clone(),
            pull_request: None,
            merged: false,
            closed: true,
            at: Timestamp(9),
            wall: 9,
            seconds: Seconds::default(),
            spend: None,
        };
        let line = serde_json::to_value(FinishedLine::of(&record, &item, item.issue)).unwrap();
        assert_eq!((line.get("kind"), line.get("tier")), (None, None));

        item.kind = Some("docs".into());
        item.tier = Some(Tier::Light);
        let line = serde_json::to_value(FinishedLine::of(&record, &item, item.issue)).unwrap();
        assert_eq!(
            (&line["kind"], &line["tier"]),
            (&json!("docs"), &json!("light"))
        );
        let back: FinishedLine = serde_json::from_value(line).unwrap();
        assert_eq!(
            (back.kind.as_deref(), back.tier),
            (Some("docs"), Some(Tier::Light))
        );

        let before: FinishedLine = serde_json::from_str(
            r#"{"at":200,"issue":7,"title":"t","pull_request":71,"merged":true,"wall":90,
            "units":30,"cost_usd":0.5,
            "worker":{"calls":2,"tokens":{"input":30,"cache_write":0,"cache_read":0,"output":0},"units":30,"cost_usd":0.5},
            "reviewer":{"calls":0,"tokens":{"input":0,"cache_write":0,"cache_read":0,"output":0},"units":0,"cost_usd":0.0},
            "qwen":{"rounds":0,"seconds":0},"review_rounds":2,"fix_turns":1,"rulings":0}"#,
        )
        .unwrap();
        assert_eq!((before.kind, before.tier), (None, None));
    }
}
