//! A workflow that is red on `main`

use serde::{Deserialize, Serialize};

use crate::ports::Timestamp;
use crate::ticket::TicketId;

/// A workflow whose latest completed run on `main` failed
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedWorkflow {
    /// The workflow's name
    pub workflow: String,
    /// The forge's id for the first red run kelpie read since it last read the
    /// workflow green, which is not the workflow's first red run when it failed
    /// before kelpie read it
    pub first_run: u64,
    /// The forge's id for its latest red run
    pub run: u64,
    /// The commit the latest red run ran on
    pub head: String,
    /// When `first_run` was created, so how long kelpie has known it red
    pub since: Timestamp,
    /// The run whose failed jobs kelpie last reran, once it has
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reran: Option<u64>,
    /// The head whose failed jobs kelpie reran, or tried to and was refused,
    /// so each red head is rerun once
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reran_head: Option<String>,
    /// Whether `head` is red again after its rerun, or its rerun was refused,
    /// which is when the project manager may open a fix or ask
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub again: bool,
    /// What the project manager did about `head` once it was red again. A fix
    /// work item stays here through the heads that land on top of it, until the
    /// item is closed or merged, so one failure has one fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handled: Option<Handled>,
    /// Whether the maintainer was told it stayed red, so a red stretch is
    /// told once
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub noticed: bool,
}

/// What the project manager did about a head that stayed red after its rerun
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Handled {
    /// It opened this fix work item at the front of the board
    Fix(TicketId),
    /// It asked the maintainer, with the log lines it read
    Asked,
}

impl RedWorkflow {
    /// The fix work item the project manager opened for this workflow, once it has
    pub fn fix(&self) -> Option<TicketId> {
        match self.handled {
            Some(Handled::Fix(ticket)) => Some(ticket),
            _ => None,
        }
    }

    /// Whether `head` has had its rerun, or its refusal. A state file from
    /// before `reran_head` holds only `reran`, the run it reran: when that is
    /// the latest red run, its head was rerun, and a newer run is a head that
    /// never was.
    pub fn rerun_done(&self) -> bool {
        match &self.reran_head {
            Some(head) => *head == self.head,
            None => self.reran == Some(self.run),
        }
    }

    /// What is being done about the workflow's latest red head, in words
    pub fn doing(&self) -> String {
        match (self.handled, self.again, self.rerun_done()) {
            (Some(Handled::Fix(ticket)), ..) => format!(
                "the project manager opened fix work item {} at the front of the board",
                ticket.shown()
            ),
            (Some(Handled::Asked), ..) => {
                "the project manager asked the maintainer, with the log lines it read".to_owned()
            }
            (None, true, _) => {
                "red again after its rerun; the project manager opens a fix or asks".to_owned()
            }
            (None, false, true) => {
                "kelpie reran its failed jobs and waits for the result".to_owned()
            }
            (None, false, false) => "kelpie reruns its failed jobs at the next read".to_owned(),
        }
    }
}
