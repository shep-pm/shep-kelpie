//! The board: which ready issue becomes the next work item
//!
//! The board's input is the project's open issues labelled `ready-for-agent`.
//! The one with the highest priority is dispatched next: `priority: P0`, then
//! `P1`, `P2`, `P3`, then an issue with no priority label. Within a priority
//! the oldest goes first, by issue number. An issue that already
//! has an open pull request or an assignee is someone's work in progress, and
//! is skipped. So is one whose `worker:` label cannot be read, since kelpie
//! would not know which model to run it on. An issue waits while any issue it
//! is blocked by is open, even one with a pull request. An issue the runner
//! picks but cannot take, because the forge cannot show it or its `worker:`
//! label fails when `add` reads it, is skipped too, so it cannot stall the
//! issues behind it. An issue with sub-issues is never worked itself: its
//! sub-issues are.

use std::fmt;

use serde::Serialize;

use crate::settings::{Effort, LabelModels, LabelName, RoleAgents, RoleModel};

/// The label that puts an issue on the board
pub const READY: &str = "ready-for-agent";

/// The prefix of the label that overrides the worker's model and effort
pub const WORKER_LABEL: &str = "worker:";

/// What follows [`WORKER_LABEL`] to ask for the project's local worker
const LOCAL: &str = "local";

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
    /// A ruling on splitting it waits on the maintainer
    Planning {
        /// The issue
        issue: u64,
        /// The ruling
        ruling: u64,
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
    /// Its `worker:` label cannot be read
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
            | Self::Planning { issue, .. }
            | Self::Blocked { issue, .. }
            | Self::Label { issue, .. }
            | Self::Failed { issue, .. }
            | Self::Rework { issue, .. } => *issue,
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
pub fn pick(ready: &[ReadyIssue], open: &[OpenPullRequest], finished: &[u64], local: bool) -> Pick {
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
        } else if let Some(error) = label_error(&issue.labels, local) {
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

// Where an issue's priority label puts it: 0 for P0 to 3 for P3, and 4 with
// none. With several, the highest counts.
fn priority_rank(labels: &[String]) -> usize {
    PRIORITIES
        .iter()
        .position(|p| labels.iter().any(|l| l == p))
        .unwrap_or(PRIORITIES.len())
}

// Why the board cannot take an issue with `labels`, given whether the
// project has a local worker, before anything is spent on it.
fn label_error(labels: &[String], local: bool) -> Option<LabelError> {
    match worker_override(labels) {
        Err(error) => Some(error),
        Ok(Some(WorkerLabel::Local)) if !local => Some(LabelError::NoLocal),
        Ok(_) => None,
    }
}

/// What an issue's `worker:` label asks for
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerLabel {
    /// `worker:<model>-<effort>`: a Claude model and effort
    Model(LabelModel),
    /// `worker:local`: the project's local worker agent
    Local,
}

/// What an issue's `worker:` label asks for, if it has one
///
/// # Errors
///
/// [`LabelError`] when a `worker:` label names no known model or effort, or
/// the issue has more than one.
pub fn worker_override(labels: &[String]) -> Result<Option<WorkerLabel>, LabelError> {
    let mut found = labels.iter().filter_map(|l| l.strip_prefix(WORKER_LABEL));
    let Some(value) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err(LabelError::Several);
    }
    parse_worker_value(value).map(Some)
}

/// What a `worker:` label's value asks for, the part after `worker:`
///
/// # Errors
///
/// [`LabelError::Unreadable`] when it is not `local`, nor `<model>-<effort>`
/// with a known model and effort.
pub fn parse_worker_value(value: &str) -> Result<WorkerLabel, LabelError> {
    if value == LOCAL {
        return Ok(WorkerLabel::Local);
    }
    let unreadable = || LabelError::Unreadable(format!("{WORKER_LABEL}{value}"));
    let (name, effort) = value.split_once('-').ok_or_else(unreadable)?;
    let name = LabelName::parse(name).ok_or_else(unreadable)?;
    let effort = Effort::parse(effort).ok_or_else(unreadable)?;
    Ok(WorkerLabel::Model(LabelModel { name, effort }))
}

/// The model names a `worker:<model>-<effort>` label may use
pub fn worker_model_names() -> [&'static str; LabelName::ALL.len()] {
    LabelName::ALL.map(LabelName::as_str)
}

/// The model name and effort a `worker:<model>-<effort>` label asks for
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelModel {
    /// The model's name, which the project's `models.labels` maps to an id
    pub name: LabelName,
    /// Passed to `claude --effort`
    pub effort: Effort,
}

/// The model and effort an issue's worker runs on, from its label and the
/// project's agents
///
/// A local worker agent takes only the issues labelled `worker:local`, which
/// the maintainer chooses. The rest run on `models`' worker, and a model
/// label runs the id `labels` gives its name.
///
/// # Errors
///
/// [`LabelError::NoLocal`] when the label asks for a local worker the
/// project does not name.
pub fn worker_for(
    label: Option<WorkerLabel>,
    agents: &RoleAgents,
    models: &RoleModel,
    labels: &LabelModels,
) -> Result<WorkerModel, LabelError> {
    let local = agents.limits.worker.lease().is_some();
    match (label, local) {
        (Some(WorkerLabel::Model(model)), _) => Ok(WorkerModel {
            model: labels.id(model.name).to_owned(),
            effort: model.effort,
            local: false,
        }),
        (Some(WorkerLabel::Local), true) => Ok(WorkerModel {
            local: true,
            ..WorkerModel::from(&agents.worker)
        }),
        (Some(WorkerLabel::Local), false) => Err(LabelError::NoLocal),
        (None, true) => Ok(WorkerModel::from(models)),
        (None, false) => Ok(WorkerModel::from(&agents.worker)),
    }
}

/// Why a `worker:` label cannot be used
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LabelError {
    /// The issue has more than one `worker:` label
    Several,
    /// This label is not `worker:<model>-<effort>` with a known model and
    /// effort, nor `worker:local`
    Unreadable(String),
    /// The issue is labelled `worker:local`, and the project's worker agent is not local
    NoLocal,
}

impl fmt::Display for LabelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Several => f.write_str("the issue has more than one `worker:` label"),
            Self::Unreadable(label) => {
                let names = worker_model_names();
                write!(
                    f,
                    "label `{label}` is not `worker:local`, nor `worker:<model>-<effort>` \
                     with a model from {}",
                    names.join(", ")
                )
            }
            Self::NoLocal => f.write_str(
                "the issue is labelled `worker:local`, and the project's `agents.worker` \
                 names no local agent",
            ),
        }
    }
}

impl core::error::Error for LabelError {}

/// The model and effort one work item's worker runs on
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerModel {
    /// Passed to `--model` as written
    pub model: String,
    /// Passed to `--effort`
    pub effort: Effort,
    /// Whether the maintainer gave the issue to the project's local worker
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
}

impl From<&RoleModel> for WorkerModel {
    fn from(role: &RoleModel) -> Self {
        Self {
            model: role.model.as_str().to_owned(),
            effort: role.effort,
            local: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(number: u64) -> ReadyIssue {
        ReadyIssue {
            number,
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

    #[test]
    fn the_oldest_ready_issue_is_picked_whatever_order_they_are_listed_in() {
        let pick = pick(&[ready(16), ready(12), ready(14)], &[], &[], true);
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
        let pick = pick(&[ready(3), prioritised(9, "priority: P0")], &[], &[], true);
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
        while let Some(next) = pick(&issues, &[], &[], true).issue {
            order.push(next);
            issues.retain(|i| i.number != next);
        }
        assert_eq!(order, [5, 8, 3, 7, 6, 2, 1, 4]);
    }

    #[test]
    fn an_issue_with_several_priority_labels_ranks_by_the_highest() {
        let mut both = prioritised(9, "priority: P3");
        both.labels.push("priority: P1".into());
        let pick = pick(&[prioritised(2, "priority: P2"), both], &[], &[], true);
        assert_eq!(pick.issue, Some(9));
    }

    #[test]
    fn a_blocked_p0_issue_waits_and_the_next_priority_is_picked() {
        let mut p0 = blocked(9, &[(4, true)]);
        p0.labels.push("priority: P0".into());
        let pick = pick(&[p0, ready(1)], &[], &[], true);
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
        let pick = pick(&[parent, piece], &[], &[], true);
        assert_eq!(pick.issue, Some(5));
        assert_eq!(pick.skipped, [Skip::Split { issue: 4, open: 2 }]);
    }

    #[test]
    fn nothing_ready_picks_nothing() {
        assert_eq!(
            pick(&[], &[], &[], true),
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
        let pick = pick(&[ready(2), taken, ready(5)], &open, &[], true);
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
        let pick = pick(&[ready(22), ready(23)], &[], &[22], true);
        assert_eq!(pick.issue, Some(23));
        assert_eq!(pick.skipped, [Skip::Finished { issue: 22 }]);
    }

    #[test]
    fn a_board_where_every_issue_is_taken_picks_nothing_and_says_why() {
        let mut taken = ready(3);
        taken.assigned = true;
        assert_eq!(
            pick(&[taken], &[], &[], true),
            Pick {
                issue: None,
                skipped: vec![Skip::Assigned { issue: 3 }]
            }
        );
    }

    #[test]
    fn an_issue_with_an_unreadable_worker_label_is_passed_over() {
        let mut odd = ready(1);
        odd.labels.push("worker:gpt-high".into());
        let pick = pick(&[odd, ready(2)], &[], &[], true);
        assert_eq!(pick.issue, Some(2));
        assert_eq!(
            pick.skipped,
            [Skip::Label {
                issue: 1,
                error: LabelError::Unreadable("worker:gpt-high".into())
            }]
        );
    }

    #[test]
    fn an_issue_for_a_local_worker_the_project_lacks_is_passed_over() {
        let mut local = ready(1);
        local.labels.push("worker:local".into());
        let without = pick(&[local.clone(), ready(2)], &[], &[], false);
        assert_eq!(without.issue, Some(2));
        assert_eq!(
            without.skipped,
            [Skip::Label {
                issue: 1,
                error: LabelError::NoLocal
            }]
        );
        assert_eq!(pick(&[local, ready(2)], &[], &[], true).issue, Some(1));
    }

    #[test]
    fn an_issue_with_an_open_blocker_is_passed_over_for_the_next_oldest() {
        let pick = pick(&[blocked(8, &[(32, true)]), ready(9)], &[], &[], true);
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
        let pick = pick(&[blocked(8, &[(32, false), (33, false)])], &[], &[], true);
        assert_eq!(pick.issue, Some(8));
        assert_eq!(pick.skipped, []);
    }

    #[test]
    fn a_skip_for_blockers_names_only_the_open_ones_lowest_first() {
        let mixed = blocked(27, &[(40, true), (24, false), (19, true), (37, true)]);
        assert_eq!(
            pick(&[mixed], &[], &[], true),
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
        let pick = pick(&[long, ready(9)], &[], &[], true);
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
    fn a_worker_label_names_the_model_and_effort() {
        assert_eq!(
            worker_override(&labels(&[READY, "worker:opus-medium"])),
            Ok(Some(WorkerLabel::Model(LabelModel {
                name: LabelName::Opus,
                effort: Effort::Medium,
            })))
        );
        assert_eq!(
            worker_override(&labels(&["worker:haiku-max"])),
            Ok(Some(WorkerLabel::Model(LabelModel {
                name: LabelName::Haiku,
                effort: Effort::Max,
            })))
        );
        assert_eq!(
            worker_override(&labels(&[READY, "worker:local"])),
            Ok(Some(WorkerLabel::Local))
        );
        assert_eq!(worker_override(&labels(&[READY, "bug"])), Ok(None));
    }

    #[test]
    fn a_local_worker_takes_only_the_issues_labelled_for_it() {
        use crate::settings::{LeaseName, Limit, NonBlank, RoleLimits};
        let model = |name: &str| RoleModel {
            model: NonBlank::try_from(name.to_owned()).unwrap(),
            effort: Effort::Low,
            harness: Default::default(),
        };
        let mut agents = RoleAgents {
            worker: model("qwen3.8:27b"),
            reviewer: model("claude-sonnet-5"),
            judge: model("claude-opus-5-5"),
            planner: model("claude-opus-5-5"),
            auditor: model("claude-opus-5-5"),
            deep_reviewer: model("claude-opus-5-5"),
            limits: RoleLimits {
                worker: Limit::Lease(LeaseName::gpu()),
                ..RoleLimits::default()
            },
        };
        let models = model("claude-sonnet-5");
        let ids = LabelModels::default();
        let worker = |label, agents: &RoleAgents| worker_for(label, agents, &models, &ids);
        let ran = |m: Result<WorkerModel, LabelError>| m.map(|m| m.model);
        assert_eq!(ran(worker(None, &agents)), Ok("claude-sonnet-5".into()));
        let local = Some(WorkerLabel::Local);
        assert_eq!(
            ran(worker(local.clone(), &agents)),
            Ok("qwen3.8:27b".into())
        );
        let opus = worker_override(&labels(&["worker:opus-low"])).unwrap();
        assert_eq!(ran(worker(opus, &agents)), Ok("claude-opus-5-5".into()));
        let sonnet = worker_override(&labels(&["worker:sonnet-high"])).unwrap();
        assert_eq!(
            ran(worker(sonnet.clone(), &agents)),
            Ok("claude-sonnet-5-5".into())
        );
        let mapped: LabelModels = toml::from_str("sonnet = \"claude-sonnet-6\"").unwrap();
        assert_eq!(
            ran(worker_for(sonnet, &agents, &models, &mapped)),
            Ok("claude-sonnet-6".into())
        );

        agents.limits.worker = Limit::default();
        assert_eq!(ran(worker(None, &agents)), Ok("qwen3.8:27b".into()));
        assert_eq!(worker(local, &agents), Err(LabelError::NoLocal));
    }

    #[test]
    fn a_worker_label_that_cannot_be_read_is_an_error() {
        for bad in [
            "worker:opus",
            "worker:opus-",
            "worker:-low",
            "worker:gpt-low",
            "worker:opus-huge",
            "worker:Opus-low",
            "worker:Local",
            "worker:local-low",
        ] {
            assert_eq!(
                worker_override(&labels(&[bad])),
                Err(LabelError::Unreadable(bad.into())),
                "{bad}"
            );
        }
        assert_eq!(
            worker_override(&labels(&["worker:opus-low", "worker:sonnet-high"])),
            Err(LabelError::Several)
        );
    }
}
