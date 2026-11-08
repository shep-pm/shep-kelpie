//! Confirmed findings a merged pull request left unfixed

use serde::{Deserialize, Serialize};

use crate::ports::{Finding, Timestamp};

/// Confirmed findings a merged pull request left unfixed, waiting to be filed
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowUps {
    /// The findings still to file
    pub findings: Vec<Finding>,
    /// Whether a yes to a `follow-up` ruling allows filing
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ruled: bool,
    /// Whether the findings must be filed unread as `needs-approval`
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
    /// When the forge first refused to take them, since it last took one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_refused: Option<Timestamp>,
}

impl FollowUps {
    /// Takes a yes to the `follow-up` ruling: `triaged` and `unread` as the
    /// answer gave them, and the findings at the places in `dropped`, each
    /// from 1, left out of what is filed
    pub fn say_yes(&mut self, triaged: bool, unread: bool, dropped: &[usize]) {
        // A retry without prior triage keeps the findings unread.
        self.unread |= unread || (!self.ruled && !triaged);
        self.ruled = true;
        self.first_refused = None;
        let mut at = 0;
        self.findings.retain(|_| {
            at += 1;
            !dropped.contains(&at)
        });
    }
}
