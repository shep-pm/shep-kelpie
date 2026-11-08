//! What kelpie keeps of triaging issues other people open

use serde::{Deserialize, Serialize};

use crate::ports::{Probability, Timestamp};
use crate::ticket::TicketId;

/// Where triaging stands
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageState {
    /// When `triage.pick` went past `off`: only an issue opened after it is
    /// triaged
    pub since: Timestamp,
    /// The issues triaged or being triaged, oldest first, the last [`TRIAGED_KEPT`]
    /// finished ones
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<Triaged>,
    /// The triager calls made on issues that have no record to carry them yet,
    /// oldest issue first, the last [`TRIAGED_KEPT`] issues' worth. A first pass
    /// has no record until its comment is recorded, so its calls are kept here:
    /// a restart does not forget the places they spent.
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unrecorded: Vec<UnrecordedCall>,
    /// The highest issue number a prune dropped the record of, so an issue at or
    /// below it that carries `needs-approval` and has no record is told from one
    /// someone labelled by hand: kelpie triaged it and the record went.
    ///
    /// Left out while zero.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pruned_through: u64,
    /// The splits kelpie is filing or has filed, one per issue. A plan lives here
    /// so a split cut short resumes after a restart and files nothing twice. An
    /// entry stays while its issue is listed open, and it is what keeps the
    /// parent from `ready-for-agent`.
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub splits: Vec<TriageSplit>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// How many finished triage records are kept, the oldest going first
///
/// An issue whose record went is told apart from an untouched one by its
/// `needs-approval` label and, if that is taken off, by the marker in its
/// comments. The label alone does not say who put it on, so
/// [`TriageState::pruned_through`] says which issue numbers kelpie may have
/// triaged. A duplicate-closing record is kept past the count: its issue has
/// no label, and the record keeps a reopened one out of the due list.
pub const TRIAGED_KEPT: usize = 200;

/// How many of one issue's triage call times are kept, which is more than the
/// most one issue can make in a day: its first pass, its author's answers and
/// a body the author rewrote.
pub const TRIAGE_CALLS_KEPT: usize = 8;

/// The last format before a triage kept the highest issue number a prune dropped
const BEFORE_PRUNED_THROUGH: u32 = 75;

impl TriageState {
    /// Drops the oldest finished records past [`TRIAGED_KEPT`], keeping duplicate-closers and records that hold a call
    ///
    /// The daily caps count the calls the records hold, so a record that holds
    /// one is all a cap can see. A limit's hold makes a record that holds none,
    /// and a burst of them would otherwise push the counted records out and free
    /// the cap partway through the burst it exists for. The calls kept for an
    /// issue with no record are bounded the same way.
    pub fn prune(&mut self) {
        // A record whose split waits on the maintainer's word stays: a yes
        // files from it.
        let held: Vec<u64> = (self.splits.iter())
            .filter(|s| s.asked.is_some())
            .map(|s| s.issue)
            .collect();
        let finished = |t: &Triaged| {
            matches!(t.step, TriageStep::Done | TriageStep::Ready)
                && t.duplicate_of.is_none()
                && !held.contains(&t.issue)
        };
        let mut extra = self
            .issues
            .iter()
            .filter(|t| finished(t))
            .count()
            .saturating_sub(TRIAGED_KEPT);
        let mut through = self.pruned_through;
        self.issues.retain(|t| {
            let drop = extra > 0 && finished(t) && t.calls.is_empty();
            extra -= usize::from(drop);
            if drop {
                through = through.max(t.issue);
            }
            !drop
        });
        self.issues.retain(|t| {
            let drop = extra > 0 && finished(t);
            extra -= usize::from(drop);
            if drop {
                through = through.max(t.issue);
            }
            !drop
        });
        self.pruned_through = through;
        if self.unrecorded.len() > TRIAGED_KEPT {
            self.unrecorded
                .drain(..self.unrecorded.len() - TRIAGED_KEPT);
        }
    }

    /// Brings a triage saved at format `found` up to this one
    ///
    /// A file saved before [`pruned_through`](Self::pruned_through) existed, at
    /// version 75 or earlier, does not say what a prune dropped. A project
    /// holding [`TRIAGED_KEPT`] finished records may have pruned some, and a
    /// record a prune dropped may be above the oldest one kept, so the floor is
    /// the highest recorded issue. A project short of the cap has pruned none.
    pub(super) fn migrate(&mut self, found: u32) {
        let finished = (self.issues.iter())
            .filter(|t| matches!(t.step, TriageStep::Done | TriageStep::Ready))
            .count();
        if found <= BEFORE_PRUNED_THROUGH && finished >= TRIAGED_KEPT {
            let highest = self.issues.iter().map(|t| t.issue).max().unwrap_or(0);
            self.pruned_through = self.pruned_through.max(highest);
        }
    }

    /// Records that a triager call on `issue` ended at `at`, for an issue that
    /// has no record to carry it yet
    pub fn record_call(&mut self, issue: u64, author: &str, at: Timestamp) {
        match self.unrecorded.iter_mut().find(|c| c.issue == issue) {
            Some(made) => {
                made.calls.push(at);
                if made.calls.len() > TRIAGE_CALLS_KEPT {
                    made.calls.remove(0);
                }
                if made.author.is_empty() {
                    made.author = author.to_owned();
                }
            }
            None => self.unrecorded.push(UnrecordedCall {
                issue,
                author: author.to_owned(),
                calls: vec![at],
            }),
        }
    }

    /// Takes the calls kept for `issue`, with their author, for a record that
    /// carries them from here on
    pub fn take_calls(&mut self, issue: u64) -> (String, Vec<Timestamp>) {
        match self.unrecorded.iter().position(|c| c.issue == issue) {
            Some(at) => {
                let made = self.unrecorded.remove(at);
                (made.author, made.calls)
            }
            None => (String::new(), Vec::new()),
        }
    }
}

/// A plan to split an issue into sub-issues, and how far the filing has got
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageSplit {
    /// The issue to split, the parent of every draft
    pub issue: u64,
    /// The drafts in the order they are filed
    pub drafts: Vec<SplitDraft>,
    /// The tickets filed so far: draft `n` is `filed[n]`, saved before the next
    /// write, so a restart files no draft twice
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filed: Vec<TicketId>,
    /// The comment holding the filing back for an answer: the issue's author's,
    /// or on someone else's issue the maintainer's yes. While it is set,
    /// nothing is filed.
    ///
    /// Left out unless the filing waits on an answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<TriageQuestions>,
}

/// One sub-issue of a split, before it is filed
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitDraft {
    /// Its title
    pub title: String,
    /// Its body, as the triager wrote it. Kelpie adds the slice line, the
    /// blockers and a marker when it files it.
    pub body: String,
    /// The labels it is filed with, `ready-for-agent` aside, which goes on last
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// The indices of the earlier drafts it is blocked by
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<usize>,
}

/// A triager call that ran on an issue with no record to carry it: the first
/// pass has none until its comment is recorded, and a call whose reply could not
/// be read never makes one at all. Its time is kept here so the caps see it
/// across a restart.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnrecordedCall {
    /// The issue it ran on
    pub issue: u64,
    /// The login of whoever opened it, which the per-author cap counts by,
    /// ignoring case. Empty when the listing gave none.
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// When each call on that issue ended, oldest first, the last
    /// [`TRIAGE_CALLS_KEPT`] of them
    pub calls: Vec<Timestamp>,
}

/// An issue whose triage comment kelpie has posted, or which a triage limit
/// held with no call at all
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Triaged {
    /// The issue's number
    pub issue: u64,
    /// The id of the comment kelpie posted on it, 0 when no comment was
    /// posted, which is what a limit's hold leaves
    pub comment: u64,
    /// How far the issue has got
    pub step: TriageStep,
    /// When the record was made: the moment its triage comment was recorded,
    /// or the moment a limit held the issue. A record saved before it existed
    /// reads as 0.
    #[serde(default = "no_time")]
    pub at: Timestamp,
    /// The login of whoever opened the issue, which the per-author cap counts
    /// by, ignoring case. Empty for a deleted account, and for a record saved
    /// before the cap existed.
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// The limit that held this issue, which kelpie labelled `needs-approval`
    /// with no call and no comment
    ///
    /// Left out unless a limit held the issue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<TriageHeld>,
    /// When each of this issue's triage calls ran, oldest first, the last
    /// [`TRIAGE_CALLS_KEPT`] of them. The caps count calls, so an issue whose
    /// author's round made several in a day spends that many places of them.
    /// A record with none made no call: a limit held it, or it was saved
    /// before calls were counted.
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<Timestamp>,
    /// The labels the research asked for that are not all on yet, so that a
    /// restart at `commented` puts them on
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// The open issue or pull request this one was closed as a duplicate of,
    /// which the comment named before the close
    ///
    /// Left out unless the issue was closed as a duplicate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<u64>,
    /// The body kelpie is owed to write: the agent-ready body over the
    /// issue's on the ready path, or the issue's body with the settled answers
    /// under its author's text on the answers path. Kept so a restart writes
    /// it and never writes it twice
    ///
    /// Left out unless an edit is owed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_body: Option<String>,
    /// The labels the issue carried when this pass began, kept so a pass that
    /// resumes after a restart still tells a triage label the maintainer put on
    /// from kelpie's own
    ///
    /// Left out while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen: Vec<String>,
    /// The ask a kelpie that asked for a word (`github`, `local` or `none`) saved while triage waited on it
    ///
    /// Kelpie no longer asks for a word: the triage comment carries the
    /// questions, and the record holds `questions` and `local`. A file saved before
    /// that still loads, and the author's comment after this ask is read as the
    /// answers to the triage comment. Left out unless a file saved before the
    /// change holds one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<TriageAsked>,
    /// The triage comment that carries the questions for the author to answer,
    /// while the triage waits for the answers. Any comment the author writes
    /// after it is the answers.
    ///
    /// Left out unless the triage waits on the answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<TriageQuestions>,
    /// The prompt the triage comment holds for the author to run in their own
    /// terminal, while the triage waits for the body their session writes back.
    /// It is set with `questions`, and either ends the wait. No label goes on
    /// meanwhile.
    ///
    /// Left out unless the triage waits on a local session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<TriageLocal>,
    /// The classifier's probability that the issue is spam, on an issue the
    /// check read as spam and gave no research call. It is what `status` says
    /// the `possible-spam` label is for.
    ///
    /// Left out unless the check read the issue as spam.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spam: Option<Probability>,
    /// The hash of the body this pass was made from. Kelpie writes no body
    /// over an issue whose body has changed since.
    ///
    /// Left out for a record saved before it existed, which is written as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub begun: Option<u64>,
}

impl Triaged {
    /// Records that a triage call ran at `at`, keeping the times oldest first
    /// and the last [`TRIAGE_CALLS_KEPT`] of them
    pub fn add_call(&mut self, at: Timestamp) {
        self.calls.push(at);
        if self.calls.len() > TRIAGE_CALLS_KEPT {
            self.calls.remove(0);
        }
    }
}

/// The ask a kelpie that asked for a word was waiting an author's answer to
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageAsked {
    /// When the ask went up: the author's comments after it are the answers,
    /// and silence this long past it ends the wait
    pub at: Timestamp,
    /// How many times it asked, 1 or 2. Kelpie no longer reads it.
    ///
    /// Left out while it has asked once.
    #[serde(default = "asked_once", skip_serializing_if = "is_once")]
    pub asks: u32,
}

fn asked_once() -> u32 {
    1
}

// A record saved before `at` existed has no time, which is past any cap's
// 24 hours, so no cap counts it.
fn no_time() -> Timestamp {
    Timestamp(0)
}

fn is_once(asks: &u32) -> bool {
    *asks <= 1
}

/// The triage comment whose questions kelpie waits an author's answers to
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageQuestions {
    /// The id of the comment holding them
    pub comment: u64,
    /// When it went up: the answers are the author's comments after it, and
    /// silence this long past it ends the round
    pub at: Timestamp,
}

/// The prompt in the triage comment, which kelpie waits an author's own
/// session to answer
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageLocal {
    /// When its prompt went up: silence this long past it, with no body edit,
    /// ends the round as `none`
    pub at: Timestamp,
    /// A hash of the issue body as kelpie last saw it. A body whose hash
    /// differs is one the author's session rewrote, and kelpie triages it
    /// again. Kelpie's own body edits set it, so its own write is never read
    /// as the author's
    pub body: u64,
}

/// How far an issue's triage has got, saved before the next write to the forge
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriageStep {
    /// The issue is the maintainer's, was labelled `ready-for-agent` by hand,
    /// and kelpie took it back for triage: its `ready-for-agent` is swapped for
    /// `needs-approval` or about to be, and its first pass is owed. The comment
    /// that pass posts replaces this record.
    Taken,
    /// The comment is posted and the labels are not all on. On the ready path
    /// the `ready_body` edit is owed too
    Commented,
    /// The comment naming the other item is posted, and the close as a
    /// duplicate of `duplicate_of` is not
    Closed,
    /// The agent-ready body is written over the issue's and the type and
    /// priority labels are not all on
    BodyWritten,
    /// The answers round's comment is posted, and its settled answers are
    /// owed under the issue's author's text, in `ready_body`
    Answers,
    /// The labels the reply gave are on and `needs-approval` is not
    Labelled,
    /// `needs-approval` is on, or the maintainer's own triage label stood
    Done,
    /// `ready-for-agent` is on
    Ready,
}

/// Which triage limit held an issue, so it got `needs-approval` with no call and
/// no comment. The hold is final for that issue: the pass does not come back to
/// it, and `shep kelpie triage` can still be asked of it.
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriageHeld {
    /// The author already had `triage.per_author_per_day` issues triaged in
    /// the 24 hours before it
    AuthorCap,
    /// The author's account is younger than `triage.min_account_days` and the
    /// forge counts them as having no history in the repo
    NewAccount,
}

impl TriageHeld {
    /// What `status` says of the issue's limit
    pub fn said(&self) -> &'static str {
        match self {
            Self::AuthorCap => "its author already had the day's triage calls",
            Self::NewAccount => "its author's account is new and has no history in the repo",
        }
    }
}
