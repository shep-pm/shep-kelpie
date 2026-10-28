//! What happened to the work items, kept for the batch retro
//!
//! One JSON object a line in `<project>/journal.jsonl`, appended as the
//! runner reports a step. The state file forgets a work item when it
//! finishes, and the step reports go to the sheep's log, so without this the
//! retro could not tell what the project manager's wakes, the reviews, the
//! CI fixes and the conflicts of a work item were. A file that cannot be
//! read or written costs the retro its digest's detail and nothing else.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ports::{Finding, Severity, Timestamp};
use crate::runner::StepReport;
use crate::ticket::TicketId;

/// One thing that happened
// wire format: changing this is a breaking change to the journal
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "kebab-case")]
pub(in crate::runner) enum Entry {
    /// The project manager answered a wake
    Wake {
        woke_for: Vec<String>,
        acted: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dropped: Vec<String>,
    },
    /// CI failed on a work item's pull request
    CiFailed {
        issue: TicketId,
        checks: Vec<String>,
    },
    /// CI failed on a work item's pull request with a log that named no file
    /// the branch changed, and these are the tests the log shows failing
    CiOutsideDiff { issue: TicketId, tests: Vec<String> },
    /// A work item's branch was rebased onto `main`
    Rebased { issue: TicketId },
    /// A work item's branch conflicted with `main`
    Conflicted { issue: TicketId, files: Vec<String> },
    /// A review round reported findings
    Round {
        issue: TicketId,
        round: u32,
        reviewer: String,
        findings: usize,
    },
    /// A reviewer the review pass went on without, with why
    ReviewerSkipped {
        issue: TicketId,
        round: u32,
        reviewer: String,
        reason: String,
    },
    /// A round's findings went to the worker's fix turn
    FindingsSent {
        issue: TicketId,
        round: u32,
        held: usize,
    },
    /// The findings a round sent to the worker, as the worker was sent them
    Findings {
        issue: TicketId,
        round: u32,
        findings: Vec<Held>,
    },
    /// What each finding a round's fix turn was sent came to, written when
    /// the turn settles
    FindingOutcomes {
        issue: TicketId,
        round: u32,
        reviewer: String,
        outcomes: Vec<FindingOutcome>,
    },
    /// What slowed a work item's worker down, from its pull request's body
    Friction { issue: TicketId, text: String },
    /// A batch retro started over the work items finished until now
    Retro { issues: Vec<TicketId> },
}

/// A finding as the journal keeps it: where it is and what is wrong, no more
// wire format: changing this is a breaking change to the journal
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::runner) struct Held {
    pub(super) severity: Severity,
    pub(super) file: String,
    pub(super) line: u32,
    pub(super) what: String,
    /// A reviewer raised it again after an earlier round sent it
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(in crate::runner) again: bool,
}

/// What became of one finding a fix turn was sent
// wire format: changing this is a breaking change to the journal
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::runner) struct FindingOutcome {
    pub(in crate::runner) severity: Severity,
    pub(in crate::runner) file: String,
    pub(in crate::runner) line: u32,
    pub(in crate::runner) outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::runner) reason: Option<String>,
}

/// What a fix turn did with a finding, as far as kelpie can tell from what
/// the worker said
// wire format: changing this is a breaking change to the journal
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(in crate::runner) enum Outcome {
    /// The turn pushed and the worker neither disputed nor deferred the
    /// finding, which is not proof it was fixed
    Pushed,
    /// The worker showed it wrong, with a reason where it gave one
    Disputed,
    /// The worker left it for a follow-up issue
    Deferred,
    /// A nit, and the turn pushed nothing
    NitDeclined,
    /// A bot's thread the worker said it fixed, in a turn that pushed
    Fixed,
    /// A bot's thread the worker left, with its reason
    Declined,
    /// A bot's thread the worker's reply did not mention, or said it fixed
    /// in a turn that pushed nothing
    NotAddressed,
}

/// The most characters of a finding's `what` the journal keeps
const WHAT_KEPT: usize = 160;

impl Held {
    /// What the journal keeps of `finding`
    pub(in crate::runner) fn of(finding: &Finding) -> Self {
        Self {
            severity: finding.severity,
            file: finding.file.clone(),
            line: finding.line,
            what: finding.what.chars().take(WHAT_KEPT).collect(),
            again: false,
        }
    }

    /// The same finding, marked or not as raised again
    pub(in crate::runner) fn again(self, again: bool) -> Self {
        Self { again, ..self }
    }
}

impl Entry {
    /// What `report` adds to the journal, if anything
    pub(in crate::runner) fn of(report: &StepReport) -> Option<Self> {
        Some(match report {
            StepReport::PmAnswered {
                woke_for,
                acted,
                dropped,
                ..
            } => Self::Wake {
                woke_for: woke_for.clone(),
                acted: acted.clone(),
                dropped: dropped.clone(),
            },
            StepReport::CiFailed { issue, checks, .. } => Self::CiFailed {
                issue: *issue,
                checks: checks.clone(),
            },
            StepReport::Rebased { issue, .. } => Self::Rebased { issue: *issue },
            StepReport::Conflicted { issue, files, .. } => Self::Conflicted {
                issue: *issue,
                files: files.clone(),
            },
            StepReport::ReviewRound {
                issue,
                round,
                reviewer,
                findings,
                ..
            } => Self::Round {
                issue: *issue,
                round: *round,
                reviewer: reviewer.as_str().to_owned(),
                findings: *findings,
            },
            StepReport::BotReviewed {
                issue,
                round,
                reviewer,
                open_threads,
                ..
            } => Self::Round {
                issue: *issue,
                round: *round,
                reviewer: reviewer.as_str().to_owned(),
                findings: *open_threads,
            },
            StepReport::ReviewerSkipped {
                issue,
                round,
                reviewer,
                reason,
                ..
            } => Self::ReviewerSkipped {
                issue: *issue,
                round: *round,
                reviewer: reviewer.as_str().to_owned(),
                reason: reason.clone(),
            },
            StepReport::LocalRoundFailed {
                issue,
                round,
                reviewer,
                unreviewed,
                ..
            } => Self::ReviewerSkipped {
                issue: *issue,
                round: *round,
                reviewer: reviewer.as_str().to_owned(),
                reason: format!("it read no file: {}", unreviewed.join(", ")),
            },
            StepReport::ReviewFindingsSent {
                issue, round, held, ..
            } => Self::FindingsSent {
                issue: *issue,
                round: *round,
                held: *held,
            },
            _ => return None,
        })
    }

    /// Whether the entry is about the work item for `ticket`
    ///
    /// A wake is about the work items it woke for, picked or acted on, never
    /// one its answer only holds back.
    pub(super) fn names(&self, ticket: TicketId) -> bool {
        match self {
            Self::Wake {
                woke_for,
                acted,
                dropped,
                ..
            } => woke_for
                .iter()
                .chain(acted_on(acted))
                .chain(dropped)
                .any(|text| mentions(text, ticket)),
            Self::CiFailed { issue: n, .. }
            | Self::Rebased { issue: n }
            | Self::Conflicted { issue: n, .. }
            | Self::CiOutsideDiff { issue: n, .. }
            | Self::Findings { issue: n, .. }
            | Self::Round { issue: n, .. }
            | Self::ReviewerSkipped { issue: n, .. }
            | Self::FindingsSent { issue: n, .. }
            | Self::FindingOutcomes { issue: n, .. }
            | Self::Friction { issue: n, .. } => *n == ticket,
            Self::Retro { issues } => issues.contains(&ticket),
        }
    }
}

/// What the project manager's answer did, with the line naming only what it
/// holds back left out: holding an issue is not acting on it
fn acted_on(acted: &[String]) -> Vec<&String> {
    acted
        .iter()
        .filter(|line| !line.starts_with(HOLDS))
        .collect()
}

/// The `acted` line that names what the wake holds back
const HOLDS: &str = "holds ";

/// The `acted` line that journals from before a pick of null meant no preference carry
const STARTS_NOTHING: &str = "starts nothing now";

/// Whether the wake whose answer `acted` is changed nothing it was asked about
pub(in crate::runner) fn changed_nothing(acted: &[String]) -> bool {
    acted_on(acted)
        .iter()
        .all(|line| line.as_str() == STARTS_NOTHING)
}

// Whether `text` names `ticket` as a whole id, and not a piece of a longer one.
// A GitHub ticket's `#` is its own start boundary, so only its digits need
// keeping there. A local id is letters and digits at both ends, so `disk12`
// and `k12x` are not `k12`, and neither is `1k12`.
fn mentions(text: &str, ticket: TicketId) -> bool {
    let tag = ticket.shown();
    let local = matches!(ticket, TicketId::Local(_));
    let runs_on = |c: char| c.is_ascii_digit() || (local && c.is_ascii_alphabetic());
    text.match_indices(&tag).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + tag.len()..].chars().next();
        !(local && before.is_some_and(runs_on)) && !after.is_some_and(runs_on)
    })
}

/// An entry and when it was made
// wire format: changing this is a breaking change to the journal
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::runner) struct Stamped {
    pub(super) at: Timestamp,
    pub(in crate::runner) entry: Entry,
}

/// Adds `entry`, made at `at`, to the journal at `file`
///
/// # Errors
///
/// What went wrong, for the log.
pub(in crate::runner) fn append(file: &Path, at: Timestamp, entry: Entry) -> Result<(), String> {
    let line = serde_json::to_string(&Stamped { at, entry }).map_err(|e| e.to_string())?;
    let opened = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .and_then(|mut f| writeln!(f, "{line}"));
    opened.map_err(|e| format!("cannot add to {}: {}", file.display(), e.kind()))
}

/// Every entry in the journal at `file`, oldest first; a line that does not
/// read is skipped
pub(in crate::runner) fn read(file: &Path) -> Vec<Stamped> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    (text.lines())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AgentName;
    use crate::test::ticket;

    fn wake(woke_for: &str) -> Entry {
        Entry::Wake {
            woke_for: vec![woke_for.to_owned()],
            acted: Vec::new(),
            dropped: Vec::new(),
        }
    }

    #[test]
    fn a_wake_names_a_work_item_only_by_its_whole_ticket_id() {
        let entry = wake("#12 conflicts with main");
        assert!(entry.names(ticket(12)));
        assert!(!entry.names(ticket(1)));
        assert!(!entry.names(ticket(123)));
        let entry = wake("k12 conflicts with main");
        assert!(entry.names("k12".parse().unwrap()));
        assert!(!entry.names("k123".parse().unwrap()));
        // A local id is letters and digits, so a word that ends in it is not
        // the id, and neither is an id that starts inside a longer one.
        let entry = wake("disk12 usage is high");
        assert!(!entry.names("k12".parse().unwrap()), "{entry:?}");
        let entry = wake("1k12 usage is high");
        assert!(!entry.names("k12".parse().unwrap()), "{entry:?}");
        let entry = wake("k12x stalled and 1k12 too");
        assert!(!entry.names("k12".parse().unwrap()), "{entry:?}");
        let entry = wake("the k12 retro, and k12 too");
        assert!(entry.names("k12".parse().unwrap()), "{entry:?}");
    }

    #[test]
    fn a_reviewer_the_pass_went_without_reaches_the_journal_with_its_reason() {
        let report = StepReport::ReviewerSkipped {
            issue: ticket(3),
            pull_request: 71,
            round: 1,
            reviewer: AgentName::try_from("codex".to_owned()).unwrap(),
            reason: "its window opens more than an hour on".to_owned(),
        };
        assert_eq!(
            Entry::of(&report),
            Some(Entry::ReviewerSkipped {
                issue: ticket(3),
                round: 1,
                reviewer: "codex".to_owned(),
                reason: "its window opens more than an hour on".to_owned(),
            })
        );
        assert!(
            Entry::ReviewerSkipped {
                issue: ticket(3),
                round: 1,
                reviewer: "codex".to_owned(),
                reason: "its calls failed three times".to_owned(),
            }
            .names(ticket(3))
        );
    }

    #[test]
    fn a_local_round_that_read_no_file_is_a_reviewer_the_pass_went_without() {
        let report = StepReport::LocalRoundFailed {
            issue: ticket(3),
            pull_request: 71,
            round: 2,
            reviewer: AgentName::try_from("qwen".to_owned()).unwrap(),
            unreviewed: vec!["src/a.rs".into(), "src/b.rs".into()],
        };
        assert_eq!(
            Entry::of(&report),
            Some(Entry::ReviewerSkipped {
                issue: ticket(3),
                round: 2,
                reviewer: "qwen".to_owned(),
                reason: "it read no file: src/a.rs, src/b.rs".to_owned(),
            })
        );
    }

    #[test]
    fn a_wake_that_only_holds_an_issue_back_names_none_of_it() {
        let entry = Entry::Wake {
            woke_for: vec!["#406 CI failed".to_owned()],
            acted: vec!["holds #407, #408".to_owned()],
            dropped: Vec::new(),
        };
        assert!(entry.names(ticket(406)));
        assert!(!entry.names(ticket(407)), "{entry:?}");
        assert!(!entry.names(ticket(408)), "{entry:?}");
        // A wake that picked it, or acted on it, is its own
        let entry = Entry::Wake {
            woke_for: Vec::new(),
            acted: vec!["picked #407".to_owned(), "holds #408".to_owned()],
            dropped: Vec::new(),
        };
        assert!(entry.names(ticket(407)), "{entry:?}");
        assert!(!entry.names(ticket(408)), "{entry:?}");
    }

    #[test]
    fn a_pick_of_none_is_a_wake_that_changed_nothing() {
        assert!(changed_nothing(&[]));
        assert!(changed_nothing(&["starts nothing now".to_owned()]));
        assert!(changed_nothing(&[
            "starts nothing now".to_owned(),
            "holds #407".to_owned()
        ]));
        assert!(!changed_nothing(&["picked #407".to_owned()]));
        assert!(!changed_nothing(&[
            "starts nothing now".to_owned(),
            "unstick #407".to_owned()
        ]));
    }

    #[test]
    fn entries_read_back_oldest_first_and_a_bad_line_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("journal.jsonl");
        let at = |n| Timestamp(n);
        append(&file, at(1), Entry::Rebased { issue: ticket(3) }).unwrap();
        std::fs::write(
            &file,
            format!("{}not json\n", std::fs::read_to_string(&file).unwrap()),
        )
        .unwrap();
        append(&file, at(2), wake("#3 stuck")).unwrap();

        let read = read(&file);
        assert_eq!(
            read,
            [
                Stamped {
                    at: at(1),
                    entry: Entry::Rebased { issue: ticket(3) }
                },
                Stamped {
                    at: at(2),
                    entry: wake("#3 stuck")
                },
            ]
        );
    }

    // The journal a kelpie that keyed its entries by issue number wrote.
    #[test]
    fn a_journal_line_that_kept_an_issue_number_reads_as_that_github_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("journal.jsonl");
        std::fs::write(
            &file,
            "{\"at\":1,\"entry\":{\"entry\":\"rebased\",\"issue\":3}}\n",
        )
        .unwrap();
        assert_eq!(
            read(&file),
            [Stamped {
                at: Timestamp(1),
                entry: Entry::Rebased { issue: ticket(3) }
            }]
        );
    }

    #[test]
    fn finding_outcomes_read_back_and_a_line_of_an_unknown_entry_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("journal.jsonl");
        let entry = Entry::FindingOutcomes {
            issue: ticket(3),
            round: 2,
            reviewer: "qwen".to_owned(),
            outcomes: vec![
                FindingOutcome {
                    severity: Severity::Medium,
                    file: "src/a.rs".to_owned(),
                    line: 4,
                    outcome: Outcome::NitDeclined,
                    reason: None,
                },
                FindingOutcome {
                    severity: Severity::High,
                    file: "src/b.rs".to_owned(),
                    line: 9,
                    outcome: Outcome::Disputed,
                    reason: Some("it is wrong".to_owned()),
                },
            ],
        };
        append(&file, Timestamp(1), entry.clone()).unwrap();
        let before = std::fs::read_to_string(&file).unwrap();
        std::fs::write(
            &file,
            format!(
                "{before}{{\"at\":2,\"entry\":{{\"entry\":\"from-a-later-build\",\"issue\":3}}}}\n"
            ),
        )
        .unwrap();

        assert!(before.contains("\"nit-declined\""), "{before}");
        assert_eq!(
            read(&file),
            [Stamped {
                at: Timestamp(1),
                entry
            }]
        );
    }

    #[test]
    fn a_finding_line_without_again_reads_as_false_and_a_false_value_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("journal.jsonl");
        std::fs::write(
            &file,
            "{\"at\":1,\"entry\":{\"entry\":\"findings\",\"issue\":3,\"round\":1,\
             \"findings\":[{\"severity\":\"medium\",\"file\":\"src/a.rs\",\"line\":4,\
             \"what\":\"old\"}]}}\n",
        )
        .unwrap();
        let Entry::Findings { findings, .. } = read(&file).remove(0).entry else {
            panic!("a findings entry");
        };
        assert!(!findings[0].again, "{findings:?}");

        let finding = Finding::new(Severity::Medium, "src/a.rs", 4, "old", "why");
        let written = |held: Held| {
            let entry = Entry::Findings {
                issue: ticket(3),
                round: 2,
                findings: vec![held],
            };
            serde_json::to_string(&entry).unwrap()
        };
        assert!(!written(Held::of(&finding)).contains("again"));
        assert!(written(Held::of(&finding).again(true)).contains("\"again\":true"));
    }

    #[test]
    fn a_missing_journal_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(&dir.path().join("none.jsonl")), []);
    }
}
