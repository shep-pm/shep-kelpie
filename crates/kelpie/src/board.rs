//! The board: which ready issue becomes the next work item
//!
//! The board's input is the project's open issues labelled `ready-for-agent`.
//! The oldest one, by issue number, is dispatched next. An issue that already
//! has an open pull request or an assignee is someone's work in progress, and
//! is skipped. So is one whose `worker:` label cannot be read, since kelpie
//! would not know which model to run it on.

use std::fmt;

use serde::Serialize;

use crate::settings::{Effort, RoleModel};

/// The label that puts an issue on the board
pub const READY: &str = "ready-for-agent";

/// The prefix of the label that overrides the worker's model and effort
const WORKER_LABEL: &str = "worker:";

// The names a `worker:` label may use, and the model each one runs.
const MODELS: [(&str, &str); 4] = [
    ("opus", "claude-opus-5-5"),
    ("sonnet", "claude-sonnet-5"),
    ("haiku", "claude-haiku-4-5-20251001"),
    ("fable", "claude-fable-5-1"),
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
    /// Its `worker:` label cannot be read
    Label {
        /// The issue
        issue: u64,
        /// Why
        error: LabelError,
    },
}

/// What the board picked, and what it passed over on the way
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// The issue to dispatch, if any is free
    pub issue: Option<u64>,
    /// The ready issues older than it, or all of them when none is free
    pub skipped: Vec<Skip>,
}

/// Picks the oldest ready issue that nobody is working on
///
/// `finished` lists the issues whose work items kelpie already finished. The
/// forge can still list one as open and ready for a while after its pull
/// request merges, so the board never takes one again.
pub fn pick(ready: &[ReadyIssue], open: &[OpenPullRequest], finished: &[u64]) -> Pick {
    let mut ready: Vec<&ReadyIssue> = ready.iter().collect();
    ready.sort_by_key(|i| i.number);
    let mut skipped = Vec::new();
    for issue in ready {
        let number = issue.number;
        if finished.contains(&number) {
            skipped.push(Skip::Finished { issue: number });
        } else if let Some(pr) = open.iter().find(|pr| pr.closes.contains(&number)) {
            skipped.push(Skip::PullRequest {
                issue: number,
                pull_request: pr.number,
            });
        } else if issue.assigned {
            skipped.push(Skip::Assigned { issue: number });
        } else if let Err(error) = worker_override(&issue.labels) {
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

/// The worker's model and effort from a `worker:<model>-<effort>` label, if the issue has one
///
/// # Errors
///
/// [`LabelError`] when a `worker:` label names no known model or effort, or
/// the issue has more than one.
pub fn worker_override(labels: &[String]) -> Result<Option<WorkerModel>, LabelError> {
    let mut found = labels.iter().filter_map(|l| l.strip_prefix(WORKER_LABEL));
    let Some(value) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err(LabelError::Several);
    }
    let unreadable = || LabelError::Unreadable(format!("{WORKER_LABEL}{value}"));
    let (name, effort) = value.split_once('-').ok_or_else(unreadable)?;
    let model = MODELS
        .iter()
        .find(|(n, _)| *n == name)
        .ok_or_else(unreadable)?
        .1;
    let effort = Effort::parse(effort).ok_or_else(unreadable)?;
    Ok(Some(WorkerModel {
        model: model.to_owned(),
        effort,
    }))
}

/// Why a `worker:` label cannot be used
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LabelError {
    /// The issue has more than one `worker:` label
    Several,
    /// This label is not `worker:<model>-<effort>` with a known model and effort
    Unreadable(String),
}

impl fmt::Display for LabelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Several => f.write_str("the issue has more than one `worker:` label"),
            Self::Unreadable(label) => {
                let names: Vec<&str> = MODELS.iter().map(|(name, _)| *name).collect();
                write!(
                    f,
                    "label `{label}` is not `worker:<model>-<effort>` with a model from {}",
                    names.join(", ")
                )
            }
        }
    }
}

impl std::error::Error for LabelError {}

/// The model and effort one work item's worker runs on
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerModel {
    /// Passed to `--model` as written
    pub model: String,
    /// Passed to `--effort`
    pub effort: Effort,
}

impl From<&RoleModel> for WorkerModel {
    fn from(role: &RoleModel) -> Self {
        Self {
            model: role.model.as_str().to_owned(),
            effort: role.effort,
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
        }
    }

    fn labels(names: &[&str]) -> Vec<String> {
        names.iter().map(|&n| n.to_owned()).collect()
    }

    #[test]
    fn the_oldest_ready_issue_is_picked_whatever_order_they_are_listed_in() {
        let pick = pick(&[ready(16), ready(12), ready(14)], &[], &[]);
        assert_eq!(pick.issue, Some(12));
        assert_eq!(pick.skipped, []);
    }

    #[test]
    fn nothing_ready_picks_nothing() {
        assert_eq!(
            pick(&[], &[], &[]),
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
        }];
        let pick = pick(&[ready(2), taken, ready(5)], &open, &[]);
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
        let pick = pick(&[ready(22), ready(23)], &[], &[22]);
        assert_eq!(pick.issue, Some(23));
        assert_eq!(pick.skipped, [Skip::Finished { issue: 22 }]);
    }

    #[test]
    fn a_board_where_every_issue_is_taken_picks_nothing_and_says_why() {
        let mut taken = ready(3);
        taken.assigned = true;
        assert_eq!(
            pick(&[taken], &[], &[]),
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
        let pick = pick(&[odd, ready(2)], &[], &[]);
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
    fn a_worker_label_names_the_model_and_effort() {
        assert_eq!(
            worker_override(&labels(&[READY, "worker:opus-medium"])),
            Ok(Some(WorkerModel {
                model: "claude-opus-5-5".into(),
                effort: Effort::Medium
            }))
        );
        assert_eq!(
            worker_override(&labels(&["worker:haiku-max"])),
            Ok(Some(WorkerModel {
                model: "claude-haiku-4-5-20251001".into(),
                effort: Effort::Max
            }))
        );
        assert_eq!(worker_override(&labels(&[READY, "bug"])), Ok(None));
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
