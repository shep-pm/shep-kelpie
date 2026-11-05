//! A ready issue the project manager holds back, and what it waits on
//!
//! A hold ends when the open work item it names closes, when the held issue's
//! text changes from what the hold was made on, or when every issue the board
//! could start is held with no work item open to wait for. Kelpie keeps the
//! holds in the state file, so a restart starts nothing the project manager
//! passed over.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::ProjectState;
use crate::board::ReadyIssue;
use crate::board::briefing::ticket_ref;
use crate::ticket::TicketId;

/// A ready issue the project manager holds back, with what it waits on
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PmHeld {
    /// The ready issue
    pub issue: TicketId,
    /// The open work item it waits on, or none for any work item closing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<TicketId>,
    // The held issue's text as the hold was made, which an edit changes
    #[serde(default)]
    text: String,
}

impl PmHeld {
    /// A hold of `issue` waiting on `until`, made when `issue` read as `text`
    pub fn new(issue: TicketId, until: Option<TicketId>, text: String) -> Self {
        Self { issue, until, text }
    }

    /// What the hold waits on, as the board shows it
    pub fn waits(&self) -> String {
        match self.until {
            Some(until) => format!("until {} closes", ticket_ref(until)),
            None => "until an open work item closes".to_owned(),
        }
    }

    // Whether the board reads `issue` as other text than the hold was made on.
    // An issue the board no longer lists has nothing to compare against.
    fn changed(&self, ready: &[ReadyIssue]) -> bool {
        ready
            .iter()
            .find(|issue| issue.ticket == self.issue)
            .is_some_and(|issue| issue.fingerprint() != self.text)
    }
}

impl fmt::Display for PmHeld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", ticket_ref(self.issue))?;
        match self.until {
            Some(until) => write!(f, " until {}", ticket_ref(until)),
            None => Ok(()),
        }
    }
}

impl ProjectState {
    /// Holds `issue` back until `until` closes, or until any open work item
    /// closes when `until` names none, made when `issue` read as `text`. A
    /// hold of `issue` already standing is left as it is.
    pub fn hold(&mut self, issue: TicketId, until: Option<TicketId>, text: String) {
        if !self.pm_holds.iter().any(|held| held.issue == issue) {
            self.pm_holds.push(PmHeld::new(issue, until, text));
        }
    }

    /// Ends every hold whose issue the board now reads as other text than the
    /// hold was made on, since the project manager held what it read
    pub fn end_changed_holds(&mut self, ready: &[ReadyIssue]) {
        self.pm_holds.retain(|held| !held.changed(ready));
    }

    /// Ends every hold waiting on one of `closed`, which are the work items
    /// that just ended. A hold naming no work item waits on any of them.
    pub fn end_holds_on(&mut self, closed: &[TicketId]) {
        self.pm_holds
            .retain(|held| !held.until.is_none_or(|until| closed.contains(&until)));
    }

    /// Whether every issue in `takeable` is held with no work item open, so no
    /// hold has anything left to wait for
    pub fn holds_stalled(&self, takeable: &[TicketId]) -> bool {
        !takeable.is_empty()
            && self.work_items.is_empty()
            && takeable
                .iter()
                .all(|ticket| self.pm_holds.iter().any(|held| held.issue == *ticket))
    }
}
