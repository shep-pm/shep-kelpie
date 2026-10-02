//! Where a work item's deep review round stands
//!
//! The round is one pass instead of a loop: two readers, a confirmation of
//! each HIGH they hold, one fix turn and a re-check of the fix. Each step is
//! kept in state, so a restart resumes at the step it was on.

use serde::{Deserialize, Serialize};

use crate::ports::{Finding, Severity};

/// A step of the deep round
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Deep {
    /// The first reader is about to read the whole pull request
    Read,
    /// A second fresh session is about to read it, shown what the first found
    Missed {
        /// The first reader's findings
        first: Vec<Finding>,
    },
    /// Each HIGH the readers hold gets a failing test, in order
    Confirming {
        /// Every finding both readers held, HIGHs first
        held: Vec<Held>,
        /// The worktree's files that differ from its head, as the session
        /// now confirming found them, so what it adds can be told
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        before: Vec<Found>,
    },
    /// What the worker is to be sent is about to be written and sent
    Sending {
        /// The findings
        held: Vec<Held>,
        /// Whether this is the second trip, after the re-check found a
        /// finding unfixed
        again: bool,
    },
    /// The worker's fix turn is next or running
    Fixing {
        /// What the worker was sent
        held: Vec<Held>,
        /// The pull request's head when it was sent, which the fix must move
        head: String,
        /// Whether this is the second trip, after the re-check found a
        /// finding unfixed
        again: bool,
    },
    /// The fix is about to be re-checked against what the worker was sent
    Rechecking {
        /// What the worker was sent
        held: Vec<Held>,
        /// The head the fix started from, so its commits are the ones after
        head: String,
        /// Whether this re-check follows the second trip
        again: bool,
    },
}

/// A finding the round holds, and what backs it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Held {
    /// What a reader found
    pub finding: Finding,
    /// What a session that ran commands made of it
    #[serde(default, skip_serializing_if = "Backing::is_pending")]
    pub backing: Backing,
    /// What a re-check found still wrong after the worker's fix, when it did
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub still: Option<String>,
}

impl Held {
    /// A finding no session has tried to confirm
    pub fn new(finding: Finding) -> Self {
        Self {
            finding,
            backing: Backing::Pending,
            still: None,
        }
    }

    /// Whether it is a HIGH still to confirm
    pub fn to_confirm(&self) -> bool {
        self.finding.severity == Severity::High && self.backing == Backing::Pending
    }
}

/// What came of trying to confirm a finding
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backing", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Backing {
    /// Not tried: a HIGH not yet confirmed, or a finding below HIGH, which
    /// goes to the fix turn as the reader wrote it
    #[default]
    Pending,
    /// A session wrote a failing test for it, and the fix is re-checked by
    /// running that test
    Test {
        /// The test file, from the repo's root
        file: String,
        /// The command that runs it
        command: String,
        /// What the session added to the worktree, which the pushed head
        /// must still hold: the test, and any other file it wrote to
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        written: Vec<Written>,
    },
    /// A session could not make a test fail for it
    Unconfirmed {
        /// Why, as the session said
        why: String,
    },
}

/// The lines a confirming session added to one file, in order
///
/// What it added and not the whole file, since two sessions' tests may go in
/// the same file and a fix may change the file elsewhere.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Written {
    /// The file, from the repo's root
    pub path: String,
    /// The lines the session added to it, trimmed, with no blank one
    pub added: Vec<String>,
}

/// A file as a confirming session found it, when it differed from the head
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Found {
    /// The file, from the repo's root
    pub path: String,
    /// Its git blob id
    pub blob: String,
}

impl Backing {
    fn is_pending(&self) -> bool {
        *self == Self::Pending
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn finding(severity: Severity) -> Finding {
        Finding {
            severity,
            file: "src/lib.rs".into(),
            line: 3,
            what: "w".into(),
            why: "y".into(),
        }
    }

    #[test]
    fn every_step_is_pinned() {
        let held = vec![
            Held::new(finding(Severity::Low)),
            Held {
                finding: finding(Severity::High),
                backing: Backing::Test {
                    file: "tests/a.rs".into(),
                    command: "cargo test a".into(),
                    written: vec![Written {
                        path: "tests/a.rs".into(),
                        added: vec!["fn a() {}".into()],
                    }],
                },
                still: None,
            },
            Held {
                finding: finding(Severity::High),
                backing: Backing::Unconfirmed { why: "x".into() },
                still: Some("the guard is gone".into()),
            },
        ];
        let value = |d: Deep| serde_json::to_value(d).unwrap();
        assert_eq!(value(Deep::Read), json!({ "step": "read" }));
        let rechecking = Deep::Rechecking {
            held: held.clone(),
            head: "abc".into(),
            again: true,
        };
        let saved = value(rechecking.clone());
        assert_eq!(
            saved,
            json!({
                "step": "rechecking",
                "held": [
                    { "finding": { "severity": "low", "file": "src/lib.rs", "line": 3, "what": "w", "why": "y" } },
                    {
                        "finding": { "severity": "high", "file": "src/lib.rs", "line": 3, "what": "w", "why": "y" },
                        "backing": {
                            "backing": "test", "file": "tests/a.rs", "command": "cargo test a",
                            "written": [{ "path": "tests/a.rs", "added": ["fn a() {}"] }],
                        },
                    },
                    {
                        "finding": { "severity": "high", "file": "src/lib.rs", "line": 3, "what": "w", "why": "y" },
                        "backing": { "backing": "unconfirmed", "why": "x" },
                        "still": "the guard is gone",
                    },
                ],
                "head": "abc",
                "again": true,
            })
        );
        assert_eq!(serde_json::from_value::<Deep>(saved).unwrap(), rechecking);
    }

    #[test]
    fn only_a_high_nobody_tried_is_still_to_confirm() {
        assert!(Held::new(finding(Severity::High)).to_confirm());
        assert!(!Held::new(finding(Severity::Medium)).to_confirm());
        let tried = Held {
            finding: finding(Severity::High),
            backing: Backing::Unconfirmed { why: "x".into() },
            still: None,
        };
        assert!(!tried.to_confirm());
    }
}
