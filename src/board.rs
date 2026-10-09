//! The board: which ready issue becomes the next work item
//!
//! The board's input is the project's open issues labelled `ready-for-agent`.
//! The one with the highest priority is dispatched next: `priority: P0`, then
//! `P1`, `P2`, `P3`, then an issue with no priority label. Within a priority
//! the oldest goes first, by issue number. An issue that already
//! has an open pull request or an assignee is someone's work in progress, and
//! is skipped. So is one whose `agent:` label names no implementer the
//! project lists, since kelpie would not know which agent to run it on. An
//! issue waits while any issue it is blocked by is open, even one with a pull
//! request. An issue the runner picks but cannot take, because the forge
//! cannot show it or its `agent:` label fails when `add` reads it, is skipped
//! too, so it cannot stall the issues behind it. An issue with sub-issues is
//! never worked itself: its sub-issues are.

use std::fmt;

use serde::Serialize;

use crate::settings::AgentName;

pub mod briefing;

/// The label that puts an issue on the board
pub const READY: &str = "ready-for-agent";

/// The prefix of the label that names the implementer an issue runs on
pub const AGENT_LABEL: &str = "agent:";

/// What ends an `agent:` label that pins its work item to the agent it names
pub const PIN: char = '!';

/// The prefix of the label that named the worker's model before `agent:`
const OLD_WORKER_LABEL: &str = "worker:";

// The priority labels, highest first. An issue with none ranks after them all.
const PRIORITIES: [&str; 4] = [
    "priority: P0",
    "priority: P1",
    "priority: P2",
    "priority: P3",
];

/// An open issue labelled [`READY`], as the forge lists it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyIssue {
    /// Its number
    pub number: u64,
    /// Its title
    pub title: String,
    /// Its body, as written
    pub body: String,
    /// Whether anyone is assigned to it
    pub assigned: bool,
    /// Its labels' names
    pub labels: Vec<String>,
    /// The issues it is blocked by, as far as the forge lists them
    pub blocked_by: Vec<Blocker>,
    /// How many more blockers the forge counted but did not list
    pub unlisted_blockers: u64,
    /// The issue it is a sub-issue of, if any
    pub parent: Option<u64>,
    /// Its own sub-issues, as the forge counts them
    pub sub_issues: SubIssues,
}

/// How many sub-issues an issue has, and how many of them are closed
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubIssues {
    /// All of them
    pub total: u64,
    /// The closed ones
    pub closed: u64,
}

impl SubIssues {
    /// Whether it has sub-issues and every one is closed
    pub fn all_closed(self) -> bool {
        self.total > 0 && self.closed >= self.total
    }
}

/// An issue a ready issue is blocked by
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blocker {
    /// Its number
    pub number: u64,
    /// Whether the forge shows it open
    pub open: bool,
}

/// An open pull request, as the forge lists it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPullRequest {
    /// Its number
    pub number: u64,
    /// The branch it merges from
    pub head: String,
    /// The issues on the same repo that it closes when it merges
    pub closes: Vec<u64>,
    /// Its labels' names
    pub labels: Vec<String>,
}

/// Why the board passed over a ready issue
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum Skip {
    /// An open pull request closes it
    PullRequest {
        /// The issue
        issue: u64,
        /// The pull request
        pull_request: u64,
    },
    /// Kelpie already finished a work item for it
    Finished {
        /// The issue
        issue: u64,
    },
    /// Someone is assigned to it
    Assigned {
        /// The issue
        issue: u64,
    },
    /// It has sub-issues, which are worked in its place
    Split {
        /// The issue
        issue: u64,
        /// Its sub-issues still open
        open: u64,
    },
    /// An issue it is blocked by is still open
    Blocked {
        /// The issue
        issue: u64,
        /// Its open blockers, lowest first
        by: Vec<u64>,
        /// Blockers the forge did not list, each taken as open
        #[serde(skip_serializing_if = "is_zero")]
        unlisted: u64,
    },
    /// Its `agent:` label names no listed implementer
    Label {
        /// The issue
        issue: u64,
        /// Why
        error: LabelError,
    },
    /// The forge could not show it when the runner came to take it
    Failed {
        /// The issue
        issue: u64,
        /// Why, as the refusal reads
        error: String,
    },
    /// Its pull request asked for a rework that could not start this poll
    Rework {
        /// The issue
        issue: u64,
        /// The pull request
        pull_request: u64,
        /// Why, as the refusal reads
        error: String,
    },
    /// Its body names a file the branch of a work item parked on a ruling
    /// changes, so it waits for that item to move
    Overlap {
        /// The issue
        issue: u64,
        /// The parked work item's issue
        with: u64,
        /// The files both touch
        files: Vec<String>,
    },
    /// It waits a pass for the board to read the paths its body names, or,
    /// with `unknown`, the files of that parked work item's branch, before
    /// the overlap with parked branches can be checked
    PathsUnread {
        /// The issue
        issue: u64,
        /// The parked work item whose branch's files are not known yet
        #[serde(skip_serializing_if = "Option::is_none")]
        unknown: Option<u64>,
    },
    /// It has no `agent:` label, and waits for the issue writer to pick
    /// one, or, with `ruling`, for the maintainer to answer that ruling
    Unlabelled {
        /// The issue
        issue: u64,
        /// The ruling it waits on, since the issue writer could not pick
        #[serde(skip_serializing_if = "Option::is_none")]
        ruling: Option<u64>,
    },
    /// An adopted pull request that could not start this poll, and still waits
    Adopt {
        /// The pull request
        pull_request: u64,
        /// Why, as the refusal reads
        error: String,
    },
}

impl Skip {
    /// The issue passed over, or 0 for an adopted pull request, which goes
    /// before every issue
    pub fn issue(&self) -> u64 {
        match self {
            Self::PullRequest { issue, .. }
            | Self::Finished { issue }
            | Self::Assigned { issue }
            | Self::Split { issue, .. }
            | Self::Blocked { issue, .. }
            | Self::Label { issue, .. }
            | Self::Failed { issue, .. }
            | Self::Rework { issue, .. }
            | Self::Overlap { issue, .. }
            | Self::PathsUnread { issue, .. }
            | Self::Unlabelled { issue, .. } => *issue,
            Self::Adopt { .. } => 0,
        }
    }
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// What the board picked, and what it passed over on the way
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// The issue to dispatch, if any is free
    pub issue: Option<u64>,
    /// The ready issues older than it, or all of them when none is free
    pub skipped: Vec<Skip>,
}

/// Picks the ready issue that nobody is working on, highest priority first and
/// oldest first within a priority
///
/// `finished` lists the issues whose work items kelpie already finished. The
/// forge can still list one as open and ready for a while after its pull
/// request merges, so the board never takes one again.
pub fn pick(
    ready: &[ReadyIssue],
    open: &[OpenPullRequest],
    finished: &[u64],
    implementers: &[AgentName],
) -> Pick {
    let mut ready: Vec<&ReadyIssue> = ready.iter().collect();
    ready.sort_by_key(|i| (priority_rank(&i.labels), i.number));
    let mut skipped = Vec::new();
    for issue in ready {
        let number = issue.number;
        let mut by: Vec<u64> = issue
            .blocked_by
            .iter()
            .filter(|b| b.open)
            .map(|b| b.number)
            .collect();
        by.sort_unstable();
        if finished.contains(&number) {
            skipped.push(Skip::Finished { issue: number });
        } else if let Some(pr) = open.iter().find(|pr| pr.closes.contains(&number)) {
            skipped.push(Skip::PullRequest {
                issue: number,
                pull_request: pr.number,
            });
        } else if issue.assigned {
            skipped.push(Skip::Assigned { issue: number });
        } else if issue.sub_issues.total > 0 {
            skipped.push(Skip::Split {
                issue: number,
                open: issue
                    .sub_issues
                    .total
                    .saturating_sub(issue.sub_issues.closed),
            });
        } else if !by.is_empty() || issue.unlisted_blockers > 0 {
            skipped.push(Skip::Blocked {
                issue: number,
                by,
                unlisted: issue.unlisted_blockers,
            });
        } else if let Some(error) = label_error(&issue.labels, implementers) {
            skipped.push(Skip::Label {
                issue: number,
                error,
            });
        } else {
            return Pick {
                issue: Some(number),
                skipped,
            };
        }
    }
    Pick {
        issue: None,
        skipped,
    }
}

/// The priority an issue's labels give it, from `P0` to `P3`, or `None` with none
pub fn priority(labels: &[String]) -> Option<&'static str> {
    let label = PRIORITIES.get(priority_rank(labels))?;
    label.strip_prefix("priority: ")
}

/// Sorts `ready` into the board rule's order: priority, then oldest by number
pub fn rule_order(ready: &mut [ReadyIssue]) {
    ready.sort_by_key(|i| (priority_rank(&i.labels), i.number));
}

// Where an issue's priority label puts it: 0 for P0 to 3 for P3, and 4 with
// none. With several, the highest counts.
fn priority_rank(labels: &[String]) -> usize {
    PRIORITIES
        .iter()
        .position(|p| labels.iter().any(|l| l == p))
        .unwrap_or(PRIORITIES.len())
}

// Why the board cannot take an issue with `labels`, given the project's
// implementers, before anything is spent on it.
fn label_error(labels: &[String], implementers: &[AgentName]) -> Option<LabelError> {
    agent_label(labels, implementers).err()
}

/// The implementer an issue's `agent:<name>` label names, if it has one
///
/// The prefix is read in any case, so `Agent:opus-high` is a label too, and
/// a `!` at its end, which pins the work item to it, is not the name's.
///
/// # Errors
///
/// [`LabelError`] when the issue has more than one `agent:` label, or its
/// label names no agent in `implementers`.
pub fn agent_label(
    labels: &[String],
    implementers: &[AgentName],
) -> Result<Option<AgentName>, LabelError> {
    let prefixed = |l: &&String| {
        l.get(..AGENT_LABEL.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(AGENT_LABEL))
    };
    let mut found = labels.iter().filter(prefixed);
    let Some(label) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err(LabelError::Several);
    }
    let value = &label[AGENT_LABEL.len()..];
    let value = value.strip_suffix(PIN).unwrap_or(value);
    match implementers.iter().find(|name| name.as_str() == value) {
        Some(name) => Ok(Some(name.clone())),
        None => Err(LabelError::NotListed {
            label: label.clone(),
            listed: implementers.iter().map(AgentName::to_string).collect(),
        }),
    }
}

/// Whether an issue's `agent:` label ends in `!`, which keeps its work item
/// on that implementer: it never falls back to another
pub fn pinned(labels: &[String]) -> bool {
    labels.iter().any(|l| {
        l.get(..AGENT_LABEL.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(AGENT_LABEL))
            && l.ends_with(PIN)
    })
}

/// An issue's `worker:` label, which kelpie no longer reads, if it has one
pub fn old_worker_label(labels: &[String]) -> Option<&str> {
    labels
        .iter()
        .find(|l| l.starts_with(OLD_WORKER_LABEL))
        .map(String::as_str)
}

/// Why an `agent:` label cannot be used
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LabelError {
    /// The issue has more than one `agent:` label
    Several,
    /// The label names no agent the project lists in `agents.implementers`
    NotListed {
        /// The label
        label: String,
        /// The agents the project lists
        listed: Vec<String>,
    },
}

impl fmt::Display for LabelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Several => f.write_str("the issue has more than one `agent:` label"),
            Self::NotListed { label, listed } => write!(
                f,
                "label `{label}` names no agent the project lists in `agents.implementers`, \
                 which are {}",
                listed.join(", ")
            ),
        }
    }
}

impl core::error::Error for LabelError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(number: u64) -> ReadyIssue {
        ReadyIssue {
            number,
            title: String::new(),
            body: String::new(),
            assigned: false,
            labels: vec![READY.into()],
            blocked_by: vec![],
            unlisted_blockers: 0,
            parent: None,
            sub_issues: SubIssues::default(),
        }
    }

    fn blocked(number: u64, by: &[(u64, bool)]) -> ReadyIssue {
        let blocked_by = by.iter().map(|&(number, open)| Blocker { number, open });
        ReadyIssue {
            blocked_by: blocked_by.collect(),
            ..ready(number)
        }
    }

    fn labels(names: &[&str]) -> Vec<String> {
        names.iter().map(|&n| n.to_owned()).collect()
    }

    // A project listing `sonnet-high` then `opus-high`.
    fn listed() -> Vec<AgentName> {
        ["sonnet-high", "opus-high"]
            .map(|n| AgentName::try_from(n.to_owned()).unwrap())
            .to_vec()
    }

    #[test]
    fn the_oldest_ready_issue_is_picked_whatever_order_they_are_listed_in() {
        let pick = pick(&[ready(16), ready(12), ready(14)], &[], &[], &listed());
        assert_eq!(pick.issue, Some(12));
        assert_eq!(pick.skipped, []);
    }

    fn prioritised(number: u64, priority: &str) -> ReadyIssue {
        let mut issue = ready(number);
        issue.labels.push(priority.into());
        issue
    }

    #[test]
    fn a_p0_issue_is_picked_before_an_older_unlabelled_one() {
        let pick = pick(
            &[ready(3), prioritised(9, "priority: P0")],
            &[],
            &[],
            &listed(),
        );
        assert_eq!(pick.issue, Some(9));
        assert_eq!(pick.skipped, []);
    }

    #[test]
    fn priorities_order_the_board_p0_to_p3_then_unlabelled_oldest_first_within_each() {
        let mut order = Vec::new();
        let mut issues = vec![
            ready(1),
            prioritised(2, "priority: P3"),
            prioritised(3, "priority: P1"),
            ready(4),
            prioritised(5, "priority: P0"),
            prioritised(6, "priority: P2"),
            prioritised(7, "priority: P1"),
            prioritised(8, "priority: P0"),
        ];
        while let Some(next) = pick(&issues, &[], &[], &listed()).issue {
            order.push(next);
            issues.retain(|i| i.number != next);
        }
        assert_eq!(order, [5, 8, 3, 7, 6, 2, 1, 4]);
    }

    #[test]
    fn an_issue_with_several_priority_labels_ranks_by_the_highest() {
        let mut both = prioritised(9, "priority: P3");
        both.labels.push("priority: P1".into());
        let pick = pick(&[prioritised(2, "priority: P2"), both], &[], &[], &listed());
        assert_eq!(pick.issue, Some(9));
    }

    #[test]
    fn a_blocked_p0_issue_waits_and_the_next_priority_is_picked() {
        let mut p0 = blocked(9, &[(4, true)]);
        p0.labels.push("priority: P0".into());
        let pick = pick(&[p0, ready(1)], &[], &[], &listed());
        assert_eq!(pick.issue, Some(1));
        assert_eq!(pick.skipped.len(), 1);
    }

    #[test]
    fn an_issue_with_sub_issues_is_never_picked_while_its_sub_issues_are() {
        let mut parent = ready(4);
        parent.sub_issues = SubIssues {
            total: 3,
            closed: 1,
        };
        let mut piece = ready(5);
        piece.parent = Some(4);
        let pick = pick(&[parent, piece], &[], &[], &listed());
        assert_eq!(pick.issue, Some(5));
        assert_eq!(pick.skipped, [Skip::Split { issue: 4, open: 2 }]);
    }

    #[test]
    fn nothing_ready_picks_nothing() {
        assert_eq!(
            pick(&[], &[], &[], &listed()),
            Pick {
                issue: None,
                skipped: vec![]
            }
        );
    }

    #[test]
    fn an_issue_with_an_open_pull_request_or_an_assignee_is_passed_over() {
        let mut taken = ready(3);
        taken.assigned = true;
        let open = [OpenPullRequest {
            number: 40,
            head: "feat/2".into(),
            closes: vec![2],
            labels: vec![],
        }];
        let pick = pick(&[ready(2), taken, ready(5)], &open, &[], &listed());
        assert_eq!(pick.issue, Some(5));
        assert_eq!(
            pick.skipped,
            [
                Skip::PullRequest {
                    issue: 2,
                    pull_request: 40
                },
                Skip::Assigned { issue: 3 },
            ]
        );
    }

    #[test]
    fn an_issue_kelpie_finished_is_passed_over_while_the_forge_still_lists_it() {
        let pick = pick(&[ready(22), ready(23)], &[], &[22], &listed());
        assert_eq!(pick.issue, Some(23));
        assert_eq!(pick.skipped, [Skip::Finished { issue: 22 }]);
    }

    #[test]
    fn a_board_where_every_issue_is_taken_picks_nothing_and_says_why() {
        let mut taken = ready(3);
        taken.assigned = true;
        assert_eq!(
            pick(&[taken], &[], &[], &listed()),
            Pick {
                issue: None,
                skipped: vec![Skip::Assigned { issue: 3 }]
            }
        );
    }

    #[test]
    fn an_issue_labelled_for_an_agent_the_project_does_not_list_is_passed_over() {
        let mut odd = ready(1);
        odd.labels.push("agent:haiku-low".into());
        let pick = pick(&[odd, ready(2)], &[], &[], &listed());
        assert_eq!(pick.issue, Some(2));
        assert_eq!(
            pick.skipped,
            [Skip::Label {
                issue: 1,
                error: LabelError::NotListed {
                    label: "agent:haiku-low".into(),
                    listed: vec!["sonnet-high".into(), "opus-high".into()],
                }
            }]
        );
        let Skip::Label { error, .. } = &pick.skipped[0] else {
            panic!("{:?}", pick.skipped);
        };
        assert_eq!(
            error.to_string(),
            "label `agent:haiku-low` names no agent the project lists in \
             `agents.implementers`, which are sonnet-high, opus-high"
        );
    }

    #[test]
    fn an_old_worker_label_is_no_label_error() {
        let mut old = ready(1);
        old.labels.push("worker:gpt-high".into());
        assert_eq!(pick(&[old, ready(2)], &[], &[], &listed()).issue, Some(1));
    }

    #[test]
    fn an_issue_with_an_open_blocker_is_passed_over_for_the_next_oldest() {
        let pick = pick(&[blocked(8, &[(32, true)]), ready(9)], &[], &[], &listed());
        assert_eq!(pick.issue, Some(9));
        assert_eq!(
            pick.skipped,
            [Skip::Blocked {
                issue: 8,
                by: vec![32],
                unlisted: 0
            }]
        );
    }

    #[test]
    fn an_issue_whose_blockers_are_all_closed_is_picked() {
        let pick = pick(
            &[blocked(8, &[(32, false), (33, false)])],
            &[],
            &[],
            &listed(),
        );
        assert_eq!(pick.issue, Some(8));
        assert_eq!(pick.skipped, []);
    }

    #[test]
    fn a_skip_for_blockers_names_only_the_open_ones_lowest_first() {
        let mixed = blocked(27, &[(40, true), (24, false), (19, true), (37, true)]);
        assert_eq!(
            pick(&[mixed], &[], &[], &listed()),
            Pick {
                issue: None,
                skipped: vec![Skip::Blocked {
                    issue: 27,
                    by: vec![19, 37, 40],
                    unlisted: 0
                }]
            }
        );
    }

    #[test]
    fn blockers_the_forge_did_not_list_count_as_open() {
        let mut long = blocked(8, &[(12, false)]);
        long.unlisted_blockers = 3;
        let pick = pick(&[long, ready(9)], &[], &[], &listed());
        assert_eq!(pick.issue, Some(9));
        assert_eq!(
            pick.skipped,
            [Skip::Blocked {
                issue: 8,
                by: vec![],
                unlisted: 3
            }]
        );
        let json = serde_json::to_value(&pick.skipped[0]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "reason": "blocked", "issue": 8, "by": [], "unlisted": 3 })
        );
    }

    #[test]
    fn an_agent_label_names_a_listed_implementer() {
        let opus = AgentName::try_from("opus-high".to_owned()).unwrap();
        assert_eq!(
            agent_label(&labels(&[READY, "agent:opus-high"]), &listed()),
            Ok(Some(opus))
        );
        assert_eq!(agent_label(&labels(&[READY, "bug"]), &listed()), Ok(None));
        assert_eq!(
            agent_label(&labels(&["worker:opus-high"]), &listed()),
            Ok(None)
        );
        let pin = labels(&[READY, "Agent:opus-high!"]);
        assert_eq!(
            agent_label(&pin, &listed()).unwrap().unwrap().as_str(),
            "opus-high"
        );
        assert!(pinned(&pin));
        assert!(!pinned(&labels(&[READY, "agent:opus-high", "bug!"])));
        for bad in [
            "agent:",
            "agent:Opus-high",
            "agent:opus-low",
            "agent:!",
            "agent:opus-high!!",
        ] {
            assert!(
                matches!(
                    agent_label(&labels(&[bad]), &listed()),
                    Err(LabelError::NotListed { label, .. }) if label == bad
                ),
                "{bad}"
            );
        }
        assert_eq!(
            agent_label(
                &labels(&["agent:opus-high", "agent:sonnet-high"]),
                &listed()
            ),
            Err(LabelError::Several)
        );
    }

    #[test]
    fn an_old_worker_label_is_found_to_be_told_about() {
        let found = labels(&[READY, "worker:opus-medium"]);
        assert_eq!(old_worker_label(&found), Some("worker:opus-medium"));
        assert_eq!(old_worker_label(&labels(&[READY, "agent:x"])), None);
    }
}
