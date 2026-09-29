//! The work item in flight, as the state file keeps it

use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::board::WorkerModel;
use crate::ports::{Cost, Finding, Role, SessionId, Timestamp, Usage, Verdict};
use crate::shots::ShotsRecord;

mod spend;

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
    /// Kelpie's last shots run, for a worktree with a launch file
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shots: Option<ShotsRecord>,
    /// The pull request's shots comment, once posted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shots_comment: Option<u64>,
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
        /// What is running. None in a state file saved before it was kept.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ReviewCallKind>,
    },
}

/// Which call a running [`ReviewCallState`] is
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewCallKind {
    /// A local round
    Local,
    /// A Claude review round
    Claude,
    /// A judge call
    Judge,
    /// A shots run
    Shots,
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

    /// Which reviewer runs this round, given whether the project has a local round
    pub fn reviewer(&self, local: bool) -> ReviewerKind {
        if local && self.round % 2 == 1 {
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
mod tests;
