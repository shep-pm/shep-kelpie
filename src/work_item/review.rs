//! Where a work item's review stands: the pass, its round and that round's stage

use serde::{Deserialize, Serialize};

use super::is_zero;
use crate::ports::{Finding, Timestamp};
use crate::review_bot::Bot;
use crate::settings::AgentName;

/// Where the review stands
///
/// A pass runs the project's reviewers once each, in order, review bots
/// among them. A round that finds anything, nits (LOW) included, sends the
/// worker all of its findings for one fix turn before the next reviewer
/// runs, but for the notice of a file the script skipped for its size,
/// which is never sent. After the last, the pull request goes to CI.
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
    /// Whether the pass runs the listed review bots alone: an adopted pull
    /// request's owed summon, or a review bot round an older state file
    /// kept, whose other reviewers had already read it
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bots_only: bool,
    /// The head this round's reviewer reads: the pushed head, which the
    /// worktree held, clean, as the round started
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reading: Option<String>,
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
            bots_only: false,
            reading: None,
        }
    }

    /// A round of `reviewer`'s, at `stage`, after its pass has ended, with
    /// every reviewer in `ran` counted as run, so its fix starts no new pass
    pub fn late(reviewer: AgentName, ran: Vec<AgentName>, stage: ReviewStage) -> Self {
        Self {
            round: u32::try_from(ran.len()).map_or(u32::MAX, |n| n.saturating_add(1)),
            stage,
            reviewer: Some(reviewer),
            ran,
            unread: false,
            ..Self::first()
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
            bots_only: self.bots_only,
            reading: None,
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
    /// The round's findings, about to go to the worker, but for a size-skipped
    /// file's notice, which is never sent
    Found {
        /// What the reviewer found
        findings: Vec<Finding>,
        /// The forge's ids of a review bot's open threads, which kelpie
        /// resolves once a fix for them moves the head. None from any other
        /// reviewer.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        threads: Vec<String>,
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
    /// The worktree was not the head on `origin` as this round was about to
    /// run, so the worker has a turn to push or discard. Once it ends, the
    /// round runs on a worktree at the pushed head, or the work item parks.
    Pushing,
    /// A reviewer with a second look has read once, and a fresh session of
    /// it is about to read again, shown what the first found. Both lists then
    /// go on as `Found`.
    SecondLook {
        /// What the first read found
        first: Vec<Finding>,
    },
    /// A review bot's round, waiting for its window and lease to summon a
    /// review of `head`. A draft is marked ready first, since a bot may skip
    /// drafts.
    Summon {
        /// The bot
        bot: Bot,
        /// When the bot's round began, or went back to waiting to ask for a
        /// full review after the bot marked the head done, which bounds its
        /// wait to summon
        started: Timestamp,
        /// The pull request's head, which the review must cover
        head: String,
        /// When kelpie marked the draft ready, until the forge reads it so
        #[serde(default, skip_serializing_if = "Option::is_none")]
        readied: Option<Timestamp>,
        /// Whether the summon asks for a full review whatever the bot read
        /// before, because the last one was answered with nothing new
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        full: bool,
    },
    /// The bot's label went on, or the comment that summons it was posted,
    /// at `at`. The lease goes back once the bot answers.
    Summoned {
        /// The bot
        bot: Bot,
        /// When the round's wait to summon began
        started: Timestamp,
        /// The head the summon is for
        head: String,
        /// When the summon was made
        at: Timestamp,
        /// Whether it asked for a full review
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        full: bool,
        /// Whether the bot gave no sign of it and it went out once more
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        resent: bool,
    },
    /// The bot's review covers the head, and its threads are read again
    /// until two reads agree, since the forge can show a review before
    /// the threads posted with it
    Settling {
        /// The bot
        bot: Bot,
        /// When kelpie first saw the review, which bounds the wait
        since: Timestamp,
        /// When kelpie last read the threads
        read: Timestamp,
        /// The forge's ids of the open threads that read found
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        open: Vec<String>,
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
                threads: Vec::new(),
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
