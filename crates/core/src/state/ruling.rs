//! A decision only the maintainer makes, as the state file keeps it

use serde::{Deserialize, Serialize};

use super::RulingKind;
use crate::ports::Timestamp;
use crate::ticket::TicketId;

/// A decision only the maintainer makes
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ruling {
    /// What the answering trigger names: its pull request's number, or the
    /// id of the ticket it is for before there is one
    pub id: TicketId,
    /// The ticket of the work item it parks. None only in an older file
    /// with no work item open.
    #[serde(default)]
    pub issue: Option<TicketId>,
    /// The question, as the maintainer reads it
    pub question: String,
    /// The pull request it is about, once there is one
    pub pull_request: Option<u64>,
    /// What raised it, which decides what a yes does
    pub kind: RulingKind,
    /// Whether it reached the maintainer's webhook. One saved before
    /// webhooks existed is posted once.
    #[serde(default)]
    pub alerted: bool,
    /// Whether its comment on its thread is held back, for the project
    /// manager's answer, and so is still to be posted. Kept so a runner that
    /// restarts posts it once the hold ends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unposted: bool,
    /// What each option leads to, in the order the card lists them, as kelpie
    /// worded it from what it knew when it raised the ruling. An option with
    /// none, or a ruling with none, shows the phrase its kind always had.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<String>,
    /// When it was raised. None in a file from before, and then an answer
    /// by number is never refused as stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raised: Option<Timestamp>,
    /// Other approaches the project manager gave when the maintainer asked
    /// for options, first first, which `<id> pm <n>` takes
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approaches: Vec<String>,
    /// What the maintainer asked the project manager about it, while the
    /// answer is owed, so a runner that restarts asks again
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<PmAsked>,
}

/// What the agent board says about one agent and one kind of work, as the
/// `agent-fit` ruling keeps it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentFit {
    /// The kind of work both agents are read at. `kind` already names the
    /// ruling's kind in the state file, so this one is `work_kind`
    pub work_kind: String,
    /// The agent that does worse
    pub worse: String,
    /// The agent it is read against
    pub better: String,
    /// What the two records say
    pub measure: AgentMeasure,
    /// The `kinds` the worse agent's file should name instead, empty when the
    /// board reads it as fitting no kind
    pub suggested_kinds: Vec<String>,
}

/// The measure that says one agent does clearly worse, with each agent's figure
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum AgentMeasure {
    /// Each agent's merge rate, in whole percents
    MergeRate {
        /// The worse agent's
        worse: u32,
        /// The other agent's
        better: u32,
    },
    /// Each agent's median hours, in whole minutes
    Hours {
        /// The worse agent's
        worse: u32,
        /// The other agent's
        better: u32,
    },
    /// Each agent's share of its items with 3 or more fix turns, in whole percents
    HeavyFix {
        /// The worse agent's
        worse: u32,
        /// The other agent's
        better: u32,
    },
}

/// What the maintainer asks the project manager about a ruling
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PmAsked {
    /// Its pick and why, posted in the thread, the ruling still theirs
    Recommend,
    /// Other approaches, for when none of its options is right
    Options,
    /// Its answer, as if `rulings.pm_decides` left the ruling to it
    Decide,
}

impl Ruling {
    /// A ruling about no work item or pull request yet, not yet alerted or
    /// posted
    pub fn new(id: TicketId, question: impl Into<String>, kind: RulingKind) -> Self {
        Self {
            id,
            issue: None,
            question: question.into(),
            pull_request: None,
            kind,
            alerted: false,
            unposted: false,
            effects: Vec::new(),
            raised: None,
            approaches: Vec::new(),
            asked: None,
        }
    }

    /// What the maintainer reads and answers it by: its pull request's
    /// number, or the id of the ticket it is for before there is one. A
    /// ruling saved with neither is named by its id.
    pub fn number(&self) -> TicketId {
        self.pull_request
            .and_then(TicketId::from_issue)
            .or(self.issue)
            .unwrap_or(self.id)
    }

    /// Whether an older kelpie left this ruling under its counter id, beside
    /// another that took its ticket's id: the id names that one
    pub fn is_left_over(&self) -> bool {
        self.id != self.number()
    }

    /// Whether `other` is this very ruling, not one raised later under the
    /// same id: a number names every ruling a work item raises in turn.
    pub fn is_raised_with(&self, other: &Ruling) -> bool {
        self.id == other.id && self.raised == other.raised
    }
}
