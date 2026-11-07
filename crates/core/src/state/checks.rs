//! What the issue check last said of each open issue, kept per issue and edit
//!
//! The board checks an issue when it is new or its key changed. An unchanged
//! issue is neither checked nor triaged twice. A result also holds where the
//! triager stands on that edit.

use serde::{Deserialize, Serialize};

/// The last version whose checks kept no `ready_seen`
const BEFORE_READY_SEEN: u32 = 101;

/// What the issue check said of one open issue, at one edit
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueCheck {
    /// The issue's number
    pub issue: u64,
    /// `board::check::key` of the title, body and labels it checked
    pub key: u64,
    /// Each part the issue lacked or held malformed, in the check's words,
    /// empty when it passed
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    /// Where the triager stands on this edit
    #[serde(default, skip_serializing_if = "CheckTriage::is_none")]
    pub triage: CheckTriage,
    /// Whether the issue has carried `ready-for-agent`. One that has and does
    /// not now had it taken off by the maintainer, and the check puts it back
    /// for no edit.
    ///
    /// Left out while false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ready_seen: bool,
}

impl IssueCheck {
    /// Seeds `ready_seen` in a file of version `found` that kept none: a passed
    /// check had its label put on, or left on, by the check
    pub(super) fn migrate(&mut self, found: u32) {
        if found <= BEFORE_READY_SEEN {
            self.ready_seen = self.passed();
        }
    }

    /// Whether the issue passed
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Where the triager stands on an issue's edit
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckTriage {
    /// Nothing is owed: it passed, or its rewrite was checked again
    #[default]
    None,
    /// It failed, and the triager has not run on it since
    Owed,
    /// It failed on an edit made after its triage ended, and is owed a new pass
    Edited,
    /// The triager ran on it, and its rewrite is checked once its pass ends
    Ran,
}

impl CheckTriage {
    /// Whether nothing is owed
    pub fn is_none(&self) -> bool {
        *self == Self::None
    }
}
