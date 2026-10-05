//! Where a work item's review stands: the pass, its round and that round's stage

use serde::{Deserialize, Serialize};

use super::is_zero;
use crate::ports::Finding;
use crate::settings::AgentName;

/// Where the review stands
///
/// A pass runs the project's reviewers once each, in order. A round that
/// finds anything above a nit (LOW) sends the worker all of its findings for
/// one fix turn before the next reviewer runs. After the last, the pull
/// request goes to CI.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    /// The round now running or about to run, 1-indexed from the start of
    /// its pass
    pub round: u32,
    /// Where this round stands
    pub stage: ReviewStage,
    /// Who reviews this round, once it has started. None in an older state
    /// file, whose rounds alternated local first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<AgentName>,
    /// The reviewers this pass has run, oldest first. The next round goes to
    /// the first listed reviewer not among them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ran: Vec<AgentName>,
    /// The calls of this round's reviewer that failed in a row
    #[serde(default, skip_serializing_if = "is_zero")]
    pub failures: u32,
    /// The reviewer before, in a state file saved before `ran`. It is kept
    /// until the next round, which reads it into `ran`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<AgentName>,
    /// Whether no reviewer of this pass has read the pull request yet: none
    /// has come back with findings or clean having reviewed some file. False
    /// in a state file from before it was kept, which is not known to be unread.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
}

impl Review {
    /// The first round of a pass, about to run
    pub fn first() -> Self {
        Self {
            round: 1,
            stage: ReviewStage::Round,
            reviewer: None,
            ran: Vec::new(),
            failures: 0,
            last: None,
            unread: true,
        }
    }

    /// The next round of the same pass, once this one's reviewer is done
    pub fn next_round(self) -> Self {
        let mut ran = self.ran;
        // Each reviewer once, so none runs twice in a pass.
        ran.extend(self.reviewer.filter(|name| !ran.contains(name)));
        Self {
            round: self.round + 1,
            stage: ReviewStage::Round,
            reviewer: None,
            ran,
            failures: 0,
            last: None,
            unread: self.unread,
        }
    }
}

/// Where one review round stands
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ReviewStage {
    /// About to run this round's reviewer
    Round,
    /// The round's findings, about to go to the worker if any is above a nit
    Found {
        /// What the reviewer found
        findings: Vec<Finding>,
    },
    /// The round's findings were sent to the worker; waiting for its fix
    Fixing {
        /// The pull request's head when the findings were sent, which a fix
        /// moves. None in an older state file, whose fix is not checked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<String>,
        /// The findings sent. Empty in an older state file, whose fix that
        /// pushed nothing is not taken to have deferred them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sent: Vec<Finding>,
        /// The deferred findings file's lines as the findings were sent, so
        /// only a deferral written since counts against them
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deferred_before: Vec<Finding>,
    },
    /// A reviewer with a second look has read once, and a fresh session of
    /// it is about to read again, shown what the first found. Both lists then
    /// go on as `Found`.
    SecondLook {
        /// What the first read found
        first: Vec<Finding>,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn every_review_stage_is_pinned() {
        let value = |s: ReviewStage| serde_json::to_value(s).unwrap();
        let finding = Finding {
            severity: crate::ports::Severity::High,
            file: "a.rs".into(),
            line: 3,
            what: "bad".into(),
            why: "breaks".into(),
        };
        assert_eq!(
            value(ReviewStage::SecondLook {
                first: vec![finding.clone()],
            }),
            json!({
                "stage": "second-look",
                "first": [{ "severity": "high", "file": "a.rs", "line": 3, "what": "bad", "why": "breaks" }],
            })
        );
        assert_eq!(
            value(ReviewStage::Found {
                findings: vec![finding],
            }),
            json!({
                "stage": "found",
                "findings": [{
                    "severity": "high",
                    "file": "a.rs",
                    "line": 3,
                    "what": "bad",
                    "why": "breaks",
                }],
            })
        );
        assert_eq!(
            value(ReviewStage::Fixing {
                head: Some("c0ffee".into()),
                sent: Vec::new(),
                deferred_before: Vec::new(),
            }),
            json!({ "stage": "fixing", "head": "c0ffee" })
        );
        let saved_before_the_head: ReviewStage =
            serde_json::from_value(json!({ "stage": "fixing" })).unwrap();
        assert_eq!(
            saved_before_the_head,
            ReviewStage::Fixing {
                head: None,
                sent: Vec::new(),
                deferred_before: Vec::new(),
            }
        );
        assert_eq!(value(saved_before_the_head), json!({ "stage": "fixing" }));
    }

    #[test]
    fn the_next_round_keeps_each_reviewer_the_pass_ran_once() {
        let name = |n: &str| AgentName::try_from(n.to_owned()).unwrap();
        let review = Review {
            reviewer: Some(name("defect-hunter")),
            ran: vec![name("qwen"), name("defect-hunter")],
            ..Review::first()
        };
        let next = review.next_round();
        assert_eq!(next.ran, [name("qwen"), name("defect-hunter")]);
        assert_eq!((next.round, next.reviewer), (2, None));
        assert!(next.unread, "nobody has read it yet");
    }
}
