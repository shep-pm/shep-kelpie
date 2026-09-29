//! The work item in flight, as the state file keeps it

use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::board::WorkerModel;
use crate::ports::{Cost, Finding, Role, SessionId, Timestamp, Usage, Verdict};
use crate::shots::ShotsRecord;

mod follow_ups;
mod spend;

pub use follow_ups::FollowUps;
pub use spend::{QwenTally, RoleSpend, Spend};

/// The work item in flight
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkItem {
    /// The issue it resolves
    pub issue: u64,
    /// The issue's title when it was added
    pub title: String,
    /// Its branch, cut from `origin/main` unless it is a rework
    pub branch: String,
    /// Whether it reworks a pull request kelpie opened before, from its
    /// latest review. Its worktree starts at the branch's head on `origin`.
    #[serde(default)]
    pub rework: bool,
    /// Whether it adopts an open pull request kelpie didn't open. Its
    /// worktree starts at the branch's head on `origin`, and kelpie never
    /// rewrites the branch's commits.
    #[serde(default)]
    pub adopted: bool,
    /// An adopted pull request's head as it arrived, which its qwen-review
    /// loop diffs against instead of `origin/main`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrived: Option<String>,
    /// Whether an adopted pull request still waits for a CodeRabbit review
    /// kelpie summoned. Until one lands, no round is satisfied.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub summon_owed: bool,
    /// Whether kelpie caught the branch up with `main` since CodeRabbit last
    /// answered a summon. CodeRabbit finds nothing new in a caught-up branch,
    /// so the next summon asks it for a full review.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rebased: bool,
    /// Local rounds finished in every pass of its review loop so far, which
    /// `review.local_rounds` caps
    #[serde(default, skip_serializing_if = "is_zero")]
    pub local_rounds: u32,
    /// Its worktree
    pub worktree: PathBuf,
    /// Its worker's build folder
    pub build: PathBuf,
    /// The model and effort its worker runs on
    pub worker: WorkerModel,
    /// The worker's session, chosen before its first turn
    pub session: SessionId,
    /// Where the worker's turn stands
    pub turn: Turn,
    /// The draft pull request its worker opened, once kelpie has seen it
    pub pull_request: Option<u64>,
    /// Where it stands between the worker's turns and the merge
    #[serde(default)]
    pub phase: Phase,
    /// The head whose red CI run last went to the worker
    #[serde(default)]
    pub red_head: Option<String>,
    /// The conflict with `main` that last went to the worker
    #[serde(default)]
    pub conflict: Option<Conflict>,
    /// What phase to force once the turn now running ends, overriding the
    /// ordinary rule that a known pull request goes straight to CI. Set by
    /// a ruling's answer that needs the qwen-review loop to run again, or by
    /// a rework; cleared once applied.
    #[serde(default)]
    pub resume: Option<Phase>,
    /// Whether a review round or judge call is in flight
    #[serde(default)]
    pub review_call: ReviewCallState,
    /// Its CodeRabbit rounds so far
    #[serde(default)]
    pub coderabbit: CodeRabbitTally,
    /// The pull request's labels, ready state and head, as kelpie and its
    /// worker leave them. A mismatch at the gate is a change kelpie did not make.
    #[serde(default)]
    pub known: Known,
    /// The head at which the maintainer accepted a change to `.claude` or
    /// `.mcp.json`. Later heads that leave those files as this one has them
    /// pass the gate, and so does a worktree holding this head's copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_files_accepted: Option<String>,
    /// Its qwen rounds so far
    #[serde(default)]
    pub qwen: QwenTally,
    /// Whether the forge refused a merge under `auto` since the last
    /// ruling on one. A second refusal raises a `merge-refused` ruling.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub merge_refused: bool,
    /// The head of the last merge under `auto` that answered an error. A
    /// pull request later found merged at it is kelpie's merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_tried: Option<String>,
    /// Kelpie's last shots run, for a project with the preview on
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shots: Option<ShotsRecord>,
    /// The pull request's shots comment, once posted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shots_comment: Option<u64>,
    /// Every finding kelpie has sent the worker to fix, across rounds. The
    /// worker can only defer one of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<Finding>,
    /// The findings the worker left unfixed, read once the pull request
    /// merges. None until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_ups: Option<FollowUps>,
    /// Every Claude call made for it, oldest first
    pub calls: Vec<CallRecord>,
}

/// A conflict with `main` that went to the worker as its next turn
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conflict {
    /// The branch's head when it conflicted
    pub head: String,
    /// The `origin/main` commit it conflicted with
    pub main: String,
    /// How many conflict turns this work item has had, this one included
    pub turns: u32,
}

/// A work item's CodeRabbit rounds so far
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeRabbitTally {
    /// Rounds whose review covered the head
    pub rounds: u32,
    /// Whether the maintainer let the rounds past their cap
    pub cap_cleared: bool,
    /// Whether CodeRabbit is satisfied with the code as it stands. A
    /// worker's turn changes the code, so it clears this.
    pub satisfied: bool,
}

/// Where one CodeRabbit round stands
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CodeRabbitStage {
    /// Waiting for the CodeRabbit lease, to summon a review of `head`
    Lease {
        /// The head CI passed on
        head: String,
        /// When kelpie marked the draft ready, until the forge reads it so
        #[serde(default, skip_serializing_if = "Option::is_none")]
        readied: Option<Timestamp>,
        /// Whether the summon asks for a full review whatever CodeRabbit read
        /// before, because the last one was answered with nothing new
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        full: bool,
    },
    /// The label went on, or the comment asking for a full review was
    /// posted, at `at`. The lease goes back once CodeRabbit answers.
    Summoned {
        /// The head the summon is for
        head: String,
        /// When the summon was made
        at: Timestamp,
        /// Whether it asked for a full review
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        full: bool,
        /// Whether CodeRabbit gave no sign of it and it went out once more
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        resent: bool,
    },
    /// The open threads of a review of `head`, judged in order
    Judging {
        /// The head the review covered
        head: String,
        /// Every thread still open, as a finding
        threads: Vec<OpenThread>,
        /// The judge's verdict on each thread judged so far, same order
        verdicts: Vec<Verdict>,
    },
    /// The findings the judge held were sent to the worker; waiting for its fix
    Fixing {
        /// The head the findings are on, which a fix moves
        head: String,
    },
}

/// A CodeRabbit thread still open, as the judge reads it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenThread {
    /// The forge's id, which resolving it takes
    pub id: String,
    /// What it says
    pub finding: Finding,
}

/// The labels, ready state and head kelpie believes a pull request carries
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Known {
    /// Its labels' names
    pub labels: Vec<String>,
    /// Whether it is marked ready for review (not a draft)
    pub ready: bool,
    /// Its head as the worker's turn or kelpie's own push left it, once
    /// kelpie has read one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

/// Whether `labels`, `ready` and `head`, as seen on the forge, differ from
/// `known`, and if so what changed, named plainly enough to answer from a phone
///
/// A `known` with no head makes no claim about it.
pub fn foreign_change(
    known: &Known,
    labels: &[String],
    ready: bool,
    head: &str,
) -> Option<(Known, String)> {
    let added: Vec<&String> = labels
        .iter()
        .filter(|l| !known.labels.contains(l))
        .collect();
    let removed: Vec<&String> = known
        .labels
        .iter()
        .filter(|l| !labels.contains(l))
        .collect();
    let mut parts = Vec::new();
    if !added.is_empty() {
        parts.push(format!(
            "the `{}` {} added",
            joined(&added),
            label_or_labels(added.len())
        ));
    }
    if !removed.is_empty() {
        parts.push(format!(
            "the `{}` {} removed",
            joined(&removed),
            label_or_labels(removed.len())
        ));
    }
    if ready && !known.ready {
        parts.push("it was marked ready for review".to_owned());
    } else if !ready && known.ready {
        parts.push("it was marked a draft again".to_owned());
    }
    if known.head.as_deref().is_some_and(|h| h != head) {
        let short = head.get(..7).unwrap_or(head);
        parts.push(format!(
            "its head moved to {short}, a commit the worker did not push"
        ));
    }
    if parts.is_empty() {
        return None;
    }
    let seen = Known {
        labels: labels.to_vec(),
        ready,
        head: Some(head.to_owned()),
    };
    Some((seen, parts.join("; ")))
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn label_or_labels(n: usize) -> &'static str {
    if n == 1 { "label was" } else { "labels were" }
}

fn joined(labels: &[&String]) -> String {
    labels
        .iter()
        .map(|l| l.as_str())
        .collect::<Vec<_>>()
        .join("`, `")
}

/// Where a work item stands between the worker's turns and the merge
///
/// A work item saved before phases existed reads as [`Phase::Implement`],
/// so a restart never puts its pull request through the gate unasked.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Phase {
    /// The worker's turns. One that ends with a pull request open starts the
    /// qwen-review loop.
    #[default]
    Implement,
    /// The qwen-review loop, between the draft pull request and CI
    Review(Review),
    /// Waiting for CI on the pull request's head
    Ci {
        /// The head kelpie last saw, once it has looked
        head: Option<String>,
        /// When kelpie first saw that head, or entered CI
        since: Timestamp,
    },
    /// A CodeRabbit round, between green CI and the merge ruling
    #[serde(rename = "coderabbit")]
    CodeRabbit(CodeRabbitStage),
    /// Parked on a ruling
    Ruling {
        /// The ruling's id
        id: u64,
    },
    /// The maintainer said yes, or every gate passed under `auto`: merging this head
    Merge {
        /// The head the ruling, or the gate, was about
        head: String,
        /// When kelpie marked the draft ready, which can start a fresh CI run
        #[serde(default)]
        readied: Option<Timestamp>,
        /// Whether the gate started it under `auto`, with no ruling asked
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        auto: bool,
    },
    /// Removing the worktree, branch and build folder
    Done {
        /// Whether the pull request merged, so its branch on the forge goes too
        merged: bool,
    },
}

/// Whether a review round or judge call is in flight
///
/// Recorded in state the way a running turn is: `drop` refuses while a
/// call is running, since it runs outside the runner's lock and a dropped
/// work item would leave nothing for the result to land on.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ReviewCallState {
    /// Nothing is running
    #[default]
    Idle,
    /// A round or a judge call is running, started at this time
    Running {
        /// When it started
        since: Timestamp,
    },
}

/// Where the review loop stands
///
/// With a local round, rounds alternate, local first: an odd round is the
/// local one, an even one Claude's, and the loop ends once two rounds in a
/// row hold nothing above a nit (LOW). Without one, every round is Claude's,
/// and one such round ends it. The worker's fix turn for each round is folded
/// in before the next.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    /// The round now running or about to run, 1-indexed
    pub round: u32,
    /// Rounds finished in a row with nothing held above a nit
    pub consecutive_clean: u32,
    /// Whether the maintainer already let the loop past its round guard
    pub guard_cleared: bool,
    /// Where this round stands
    pub stage: ReviewStage,
}

impl Review {
    /// The first round, about to run
    pub fn first() -> Self {
        Self {
            round: 1,
            consecutive_clean: 0,
            guard_cleared: false,
            stage: ReviewStage::Round,
        }
    }

    /// Which reviewer runs this round, given how many local rounds the work
    /// item has left: 0 with the local round off
    pub fn reviewer(&self, local_left: u32) -> ReviewerKind {
        if local_left > 0 && self.round % 2 == 1 {
            ReviewerKind::Local
        } else {
            ReviewerKind::Claude
        }
    }
}

/// Which reviewer a review round runs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewerKind {
    /// The project's local round, named for the first model it ran
    #[serde(rename = "qwen")]
    Local,
    /// A fresh Claude session, never the worker's
    Claude,
}

/// Where one review round stands
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ReviewStage {
    /// About to run this round's reviewer
    Round,
    /// The round's raw findings, judged in order, oldest first
    Judging {
        /// What the reviewer found
        findings: Vec<Finding>,
        /// The judge's verdict on each finding judged so far, same order
        verdicts: Vec<Verdict>,
    },
    /// The findings the judge held were sent to the worker; waiting for its fix
    Fixing {
        /// Whether every held finding was a nit (LOW), so a clean fix keeps
        /// or extends the consecutive-clean streak
        clean: bool,
        /// The pull request's head when the findings were sent, which a fix
        /// moves. None in an older state file, whose fix is not checked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<String>,
    },
}

impl WorkItem {
    /// Remembers findings sent to the worker, once each
    pub fn record_held(&mut self, held: &[Finding]) {
        let same =
            |a: &Finding, b: &Finding| (&a.file, a.line, &a.what) == (&b.file, b.line, &b.what);
        for finding in held {
            if !self.held.iter().any(|known| same(known, finding)) {
                self.held.push(finding.clone());
            }
        }
    }

    /// Records the head kelpie's own catch-up with `main` pushed, which
    /// CodeRabbit has not read
    pub fn caught_up(&mut self, head: Option<String>) {
        self.known.head = head;
        self.rebased = true;
    }

    /// The commit its qwen-review loop diffs against: `origin/main`, or the
    /// head an adopted pull request arrived with, so none of that is reviewed
    pub fn review_base(&self) -> String {
        self.arrived
            .clone()
            .unwrap_or_else(|| format!("origin/{}", crate::worktree::BASE))
    }

    /// What its calls have cost so far
    pub fn cost(&self) -> Cost {
        Cost(self.calls.iter().map(|c| c.cost.0).sum())
    }

    /// What `session` had cost as of its last recorded call
    pub fn session_cost(&self, session: &SessionId) -> Cost {
        let last = self.calls.iter().rev().find(|c| &c.session == session);
        last.map_or(Cost(0), |c| c.session_cost)
    }
}

/// A random version 4 UUID, which `claude --session-id` takes
pub fn new_session_id() -> io::Result<SessionId> {
    let mut b = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let (a, rest) = hex.split_at(8);
    let (b, rest) = rest.split_at(4);
    let (c, rest) = rest.split_at(4);
    let (d, e) = rest.split_at(4);
    Ok(SessionId(format!("{a}-{b}-{c}-{d}-{e}")))
}

/// Where the worker's turn stands
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase", deny_unknown_fields)]
pub enum Turn {
    /// The first turn waits for the project to run
    Due,
    /// A turn started and has not ended; after a restart, it is resumed
    Running {
        /// When it started
        since: Timestamp,
    },
    /// A later turn waits to resume the session with this prompt
    Next {
        /// What the turn tells the worker
        prompt: String,
    },
    /// The turn ended and the worker waits for kelpie
    Ended {
        /// When it ended
        at: Timestamp,
    },
    /// The turn could not run, and waits for the maintainer
    Failed {
        /// When it failed
        at: Timestamp,
        /// Why
        reason: String,
    },
}

/// One Claude call made for a work item
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallRecord {
    /// The role it was made for
    pub role: Role,
    /// When it ended
    pub at: Timestamp,
    /// The session it ran in
    pub session: SessionId,
    /// What it used
    pub usage: Usage,
    /// What it cost: the change in its session's cost
    pub cost: Cost,
    /// What its session had cost when it ended
    pub session_cost: Cost,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test::a_work_item;

    #[test]
    fn the_format_is_pinned() {
        assert_eq!(
            serde_json::to_value(a_work_item()).unwrap(),
            json!({
                "issue": 42,
                "title": "Add a thing",
                "branch": "kelpie/42",
                "rework": false,
                "adopted": false,
                "worktree": "/k/wt/shep/42",
                "build": "/k/targets/shep/42",
                "worker": { "model": "claude-opus-5-5", "effort": "medium" },
                "session": "5e55",
                "turn": { "state": "running", "since": 9 },
                "pull_request": 51,
                "phase": { "state": "ci", "head": "c0ffee", "since": 11 },
                "red_head": "bad",
                "conflict": { "head": "c0ffee", "main": "a11ce", "turns": 1 },
                "resume": null,
                "review_call": { "state": "idle" },
                "coderabbit": { "rounds": 0, "cap_cleared": false, "satisfied": false },
                "known": { "labels": ["review please"], "ready": false },
                "qwen": { "rounds": 0, "seconds": 0 },
                "calls": [{
                    "role": "worker",
                    "at": 10,
                    "session": "5e55",
                    "usage": { "input": 1, "cache_write": 2, "cache_read": 3, "output": 4 },
                    "cost": 5,
                    "session_cost": 6,
                }],
            })
        );
    }

    #[test]
    fn every_turn_state_is_pinned() {
        let value = |t: Turn| serde_json::to_value(t).unwrap();
        assert_eq!(value(Turn::Due), json!({ "state": "due" }));
        assert_eq!(
            value(Turn::Ended { at: Timestamp(3) }),
            json!({ "state": "ended", "at": 3 })
        );
        assert_eq!(
            value(Turn::Failed {
                at: Timestamp(4),
                reason: "no worktree".into()
            }),
            json!({ "state": "failed", "at": 4, "reason": "no worktree" })
        );
    }

    #[test]
    fn every_phase_is_pinned() {
        let value = |p: Phase| serde_json::to_value(p).unwrap();
        assert_eq!(value(Phase::Implement), json!({ "state": "implement" }));
        assert_eq!(
            value(Phase::Review(Review {
                round: 2,
                consecutive_clean: 1,
                guard_cleared: false,
                stage: ReviewStage::Round,
            })),
            json!({
                "state": "review",
                "round": 2,
                "consecutive_clean": 1,
                "guard_cleared": false,
                "stage": { "stage": "round" },
            })
        );
        assert_eq!(
            value(Phase::CodeRabbit(CodeRabbitStage::Summoned {
                head: "c0ffee".into(),
                at: Timestamp(12),
                full: false,
                resent: false,
            })),
            json!({ "state": "coderabbit", "stage": "summoned", "head": "c0ffee", "at": 12 })
        );
        let again = json!({ "state": "coderabbit", "stage": "summoned", "head": "c0ffee", "at": 12, "resent": true });
        let sent: Phase = serde_json::from_value(again.clone()).unwrap();
        assert_eq!(value(sent), again);
        let full = json!({ "state": "coderabbit", "stage": "summoned", "head": "c0ffee", "at": 12, "full": true });
        let asked: Phase = serde_json::from_value(full.clone()).unwrap();
        assert_eq!(value(asked), full);
        assert_eq!(
            value(Phase::CodeRabbit(CodeRabbitStage::Lease {
                head: "c0ffee".into(),
                readied: None,
                full: true,
            })),
            json!({ "state": "coderabbit", "stage": "lease", "head": "c0ffee", "full": true })
        );
        let judging = Phase::CodeRabbit(CodeRabbitStage::Judging {
            head: "c0ffee".into(),
            threads: vec![OpenThread {
                id: "PRRT_1".into(),
                finding: Finding {
                    severity: crate::ports::Severity::Medium,
                    file: "a.rs".into(),
                    line: 0,
                    what: "w".into(),
                    why: "y".into(),
                },
            }],
            verdicts: vec![],
        });
        let pinned = value(judging.clone());
        assert_eq!(pinned["threads"][0]["id"], "PRRT_1");
        assert_eq!(serde_json::from_value::<Phase>(pinned).unwrap(), judging);
        assert_eq!(
            value(Phase::Ruling { id: 3 }),
            json!({ "state": "ruling", "id": 3 })
        );
        assert_eq!(
            value(Phase::Merge {
                head: "c0ffee".into(),
                readied: Some(Timestamp(12)),
                auto: false,
            }),
            json!({ "state": "merge", "head": "c0ffee", "readied": 12 })
        );
        assert_eq!(
            value(Phase::Merge {
                head: "c0ffee".into(),
                readied: None,
                auto: true,
            }),
            json!({ "state": "merge", "head": "c0ffee", "readied": null, "auto": true })
        );
        assert_eq!(
            value(Phase::Done { merged: true }),
            json!({ "state": "done", "merged": true })
        );
        assert_eq!(
            serde_json::to_value(Turn::Next {
                prompt: "fix it".into()
            })
            .unwrap(),
            json!({ "state": "next", "prompt": "fix it" })
        );
    }

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
        let verdict = Verdict {
            holds: true,
            severity: crate::ports::Severity::Medium,
            reason: "regraded".into(),
        };
        assert_eq!(
            value(ReviewStage::Judging {
                findings: vec![finding.clone()],
                verdicts: vec![verdict.clone()],
            }),
            json!({
                "stage": "judging",
                "findings": [{
                    "severity": "high",
                    "file": "a.rs",
                    "line": 3,
                    "what": "bad",
                    "why": "breaks",
                }],
                "verdicts": [{ "holds": true, "severity": "medium", "reason": "regraded" }],
            })
        );
        assert_eq!(
            value(ReviewStage::Fixing {
                clean: true,
                head: Some("c0ffee".into())
            }),
            json!({ "stage": "fixing", "clean": true, "head": "c0ffee" })
        );
        let saved_before_the_head: ReviewStage =
            serde_json::from_value(json!({ "stage": "fixing", "clean": true })).unwrap();
        assert_eq!(
            saved_before_the_head,
            ReviewStage::Fixing {
                clean: true,
                head: None
            }
        );
        assert_eq!(
            value(saved_before_the_head),
            json!({ "stage": "fixing", "clean": true })
        );
    }

    #[test]
    fn rounds_alternate_local_first_and_are_all_claudes_without_one() {
        let review = Review::first();
        assert_eq!(review.reviewer(u32::MAX), ReviewerKind::Local);
        assert_eq!(
            Review { round: 2, ..review }.reviewer(u32::MAX),
            ReviewerKind::Claude
        );
        for round in 1..=3 {
            let review = Review {
                round,
                ..Review::first()
            };
            assert_eq!(review.reviewer(0), ReviewerKind::Claude, "round {round}");
        }
    }

    #[test]
    fn with_no_local_round_left_every_round_is_claudes() {
        let round = |round| Review {
            round,
            ..Review::first()
        };
        assert_eq!(round(3).reviewer(1), ReviewerKind::Local);
        assert_eq!(round(3).reviewer(0), ReviewerKind::Claude);
        assert_eq!(round(4).reviewer(1), ReviewerKind::Claude);
    }

    #[test]
    fn the_local_reviewer_keeps_its_wire_name() {
        assert_eq!(
            serde_json::to_value(ReviewerKind::Local).unwrap(),
            json!("qwen")
        );
    }

    #[test]
    fn review_call_state_is_pinned() {
        assert_eq!(
            serde_json::to_value(ReviewCallState::Idle).unwrap(),
            json!({ "state": "idle" })
        );
        assert_eq!(
            serde_json::to_value(ReviewCallState::Running {
                since: Timestamp(9)
            })
            .unwrap(),
            json!({ "state": "running", "since": 9 })
        );
    }

    #[test]
    fn a_work_item_saved_before_phases_reads_as_implementing() {
        let mut value = serde_json::to_value(a_work_item()).unwrap();
        let fields = value.as_object_mut().unwrap();
        fields.remove("phase");
        fields.remove("red_head");
        let item: WorkItem = serde_json::from_value(value).unwrap();
        assert_eq!((item.phase, item.red_head), (Phase::Implement, None));
    }

    #[test]
    fn the_review_loop_diffs_from_main_unless_a_pull_request_arrived_at_a_head() {
        let mut item = a_work_item();
        assert_eq!(item.review_base(), "origin/main");
        item.arrived = Some("c0ffee".into());
        assert_eq!(item.review_base(), "c0ffee");
    }

    #[test]
    fn a_work_item_saved_before_reworks_and_adoptions_is_neither() {
        let mut value = serde_json::to_value(a_work_item()).unwrap();
        let fields = value.as_object_mut().unwrap();
        fields.remove("rework");
        fields.remove("adopted");
        let item: WorkItem = serde_json::from_value(value).unwrap();
        assert!(!item.rework && !item.adopted);
    }

    #[test]
    fn a_refused_merge_is_saved_only_while_it_stands() {
        let mut item = a_work_item();
        let value = serde_json::to_value(&item).unwrap();
        assert!(value.get("merge_refused").is_none(), "{value}");
        assert!(
            !serde_json::from_value::<WorkItem>(value)
                .unwrap()
                .merge_refused
        );
        item.merge_refused = true;
        let value = serde_json::to_value(&item).unwrap();
        assert_eq!(value["merge_refused"], true);
        assert!(
            serde_json::from_value::<WorkItem>(value)
                .unwrap()
                .merge_refused
        );
    }

    #[test]
    fn session_ids_are_distinct_version_4_uuids() {
        let a = new_session_id().unwrap().0;
        let b = new_session_id().unwrap().0;
        assert_ne!(a, b);
        let groups: Vec<_> = a.split('-').map(str::len).collect();
        assert_eq!(groups, [8, 4, 4, 4, 12]);
        assert_eq!(&a[14..15], "4");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"), "{a}");
    }

    #[test]
    fn nothing_changed_is_no_foreign_change() {
        let known = Known {
            labels: vec!["bug".into()],
            ready: true,
            head: None,
        };
        assert_eq!(
            foreign_change(&known, &["bug".to_owned()], true, "c0ffee"),
            None
        );
    }

    #[test]
    fn one_label_added_is_named_in_the_singular() {
        let known = Known::default();
        let (seen, text) = foreign_change(&known, &["bug".to_owned()], false, "c0ffee").unwrap();
        assert_eq!(text, "the `bug` label was added");
        assert_eq!(
            seen,
            Known {
                labels: vec!["bug".into()],
                ready: false,
                head: Some("c0ffee".into()),
            }
        );
    }

    #[test]
    fn two_labels_added_are_named_in_the_plural() {
        let known = Known::default();
        let labels = ["urgent".to_owned(), "bug".to_owned()];
        let (_, text) = foreign_change(&known, &labels, false, "c0ffee").unwrap();
        assert_eq!(text, "the `urgent`, `bug` labels were added");
    }

    #[test]
    fn labels_removed_are_named_with_the_same_singular_and_plural_rule() {
        let known = Known {
            labels: vec!["bug".into()],
            ready: false,
            head: None,
        };
        let (_, text) = foreign_change(&known, &[], false, "c0ffee").unwrap();
        assert_eq!(text, "the `bug` label was removed");

        let known = Known {
            labels: vec!["urgent".into(), "bug".into()],
            ready: false,
            head: None,
        };
        let (_, text) = foreign_change(&known, &[], false, "c0ffee").unwrap();
        assert_eq!(text, "the `urgent`, `bug` labels were removed");
    }

    #[test]
    fn marking_ready_or_a_draft_again_is_named() {
        let known = Known::default();
        let (seen, text) = foreign_change(&known, &[], true, "c0ffee").unwrap();
        assert_eq!(text, "it was marked ready for review");
        assert!(seen.ready);

        let known = Known {
            labels: vec![],
            ready: true,
            head: None,
        };
        let (seen, text) = foreign_change(&known, &[], false, "c0ffee").unwrap();
        assert_eq!(text, "it was marked a draft again");
        assert!(!seen.ready);
    }

    #[test]
    fn every_kind_of_change_at_once_is_joined_with_semicolons() {
        let known = Known {
            labels: vec!["bug".into()],
            ready: false,
            head: Some("a11ce".into()),
        };
        let (_, text) = foreign_change(&known, &["urgent".to_owned()], true, "c0ffee").unwrap();
        assert_eq!(
            text,
            "the `urgent` label was added; the `bug` label was removed; \
             it was marked ready for review; \
             its head moved to c0ffee, a commit the worker did not push"
        );
    }

    #[test]
    fn a_head_the_worker_did_not_push_is_named_by_its_short_hash() {
        let known = Known {
            head: Some("a11ce".into()),
            ..Known::default()
        };
        let head = "4887ecf0123456789";
        let (seen, text) = foreign_change(&known, &[], false, head).unwrap();
        assert_eq!(
            text,
            "its head moved to 4887ecf, a commit the worker did not push"
        );
        assert_eq!(seen.head.as_deref(), Some(head));
        assert_eq!(foreign_change(&seen, &[], false, head), None);
    }

    #[test]
    fn a_known_with_no_head_makes_no_claim_about_it() {
        assert_eq!(
            foreign_change(&Known::default(), &[], false, "c0ffee"),
            None
        );
    }

    #[test]
    fn a_calls_cost_is_measured_from_its_own_sessions_last_call() {
        let mut item = a_work_item();
        let other = SessionId("0th3r".into());
        let mut call = item.calls[0].clone();
        call.session = other.clone();
        call.session_cost = Cost(40);
        call.cost = Cost(40);
        item.calls.push(call);
        assert_eq!(item.session_cost(&item.session), Cost(6));
        assert_eq!(item.session_cost(&other), Cost(40));
        assert_eq!(item.session_cost(&SessionId("new".into())), Cost(0));
        assert_eq!(item.cost(), Cost(45));
    }
}
