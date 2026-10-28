//! Each action a runner answers, read as one typed request

use serde::{Deserialize, Serialize};

use shep_kelpie_core::ticket::TicketId;

/// A request a runner answers: one variant per action it has
///
/// A frame carries it as `{"action": ..., "params": ...}`, where `action` is
/// the name `shep trigger` is given and `params` is what follows it. `pm`
/// covers both of its verbs, so `pm_attach` and `pm_detach` name which one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// `status`: what the runner's loop last posted, or as it stands now
    Status,
    /// `add <ticket>`: open a work item for the issue
    Add(TicketId),
    /// `rework <number>`: send a pull request back to its worker
    Rework(u64),
    /// `adopt <number>`: take over a pull request kelpie did not open
    Adopt(u64),
    /// `rule <id> <answer>`: the maintainer's answer to a ruling
    Rule(TicketId, RuleAnswer),
    /// `gate [ticket]`: send a work item into the gate
    Gate(Option<TicketId>),
    /// `drop [ticket]`: end a work item
    Drop(Option<TicketId>),
    /// `timings [count]`: where the last finished work items' time went
    Timings(usize),
    /// `attach <ticket> <pid> [session pid]`: hold a work item for a terminal
    Attach(TicketId, u32, Option<u32>),
    /// `detach <ticket> <pid>`: let the held work item go
    Detach(TicketId, u32),
    /// `tell <note>`: a note the project manager's next wake carries
    Tell(String),
    /// `pm attach <pid> [session pid]`: hold the project manager for a terminal
    PmAttach(u32, Option<u32>),
    /// `pm detach <pid>`: let the held project manager go
    PmDetach(u32),
    /// `drain [pause]`: start no new call, and end each worker turn at its
    /// next tool call with `pause`
    Drain {
        /// Whether each worker turn in flight ends at its next tool call
        at_tool: bool,
    },
    /// `undrain [keep]`: start calls again, leaving the turns a pause ended
    /// stopped with `keep`
    Undrain {
        /// Whether the turns a pause ended stay stopped
        keep: bool,
    },
    /// `finish`: hold the board back until the open work items end
    Finish,
    /// `start`: take the board back off `finish`
    Start,
    /// `pausing`: say whether the runner is ending each worker turn
    Pausing,
    /// `board [move <issue> <position> | top <issue>]`: the board's order
    Board(Option<(TicketId, u64)>),
    /// `triage <issue>`: ask the triager about an issue other people opened
    Triage(u64),
    /// `watch`: the status now, then again after each change, as [`Event`]s
    ///
    /// Not an action of `shep trigger`, and it wakes no pass.
    ///
    /// [`Event`]: crate::reply::Event
    Watch,
}

// The action names serde reads for `Request`'s variants: each variant name in
// snake_case, which is what a frame's `action` carries
const NAMES: [&str; 21] = [
    "status",
    "add",
    "rework",
    "adopt",
    "rule",
    "gate",
    "drop",
    "timings",
    "attach",
    "detach",
    "tell",
    "pm_attach",
    "pm_detach",
    "drain",
    "undrain",
    "finish",
    "start",
    "pausing",
    "board",
    "triage",
    "watch",
];

impl Request {
    /// The action `shep trigger` names this request, and `watch` for the one it has no action for
    pub fn action(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Add(_) => "add",
            Self::Rework(_) => "rework",
            Self::Adopt(_) => "adopt",
            Self::Rule(..) => "rule",
            Self::Gate(_) => "gate",
            Self::Drop(_) => "drop",
            Self::Timings(_) => "timings",
            Self::Attach(..) => "attach",
            Self::Detach(..) => "detach",
            Self::Tell(_) => "tell",
            Self::PmAttach(..) | Self::PmDetach(_) => "pm",
            Self::Drain { .. } => "drain",
            Self::Undrain { .. } => "undrain",
            Self::Finish => "finish",
            Self::Start => "start",
            Self::Pausing => "pausing",
            Self::Board(_) => "board",
            Self::Triage(_) => "triage",
            Self::Watch => "watch",
        }
    }

    /// Whether a frame naming `name` as its action reads as one of these
    ///
    /// A frame names a variant, so `pm` arrives as `pm_attach` or `pm_detach`,
    /// where [`Request::action`] names the verb `shep trigger` is given.
    pub(crate) fn reads(name: &str) -> bool {
        NAMES.contains(&name)
    }
}

/// The maintainer's answer to a ruling
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "lowercase")]
pub enum RuleAnswer {
    /// Go ahead: what that means depends on the ruling
    Yes,
    /// Do not, and send the worker this note
    No {
        /// The note the worker gets
        note: String,
    },
    /// On a merge ruling only: send the worker this note, and its change
    /// through a new pass of the review
    Rework {
        /// The note the worker gets
        note: String,
    },
    /// The answer to the worker's question
    Text {
        /// What the worker is told
        text: String,
    },
    /// On a contribution's ruling only: a yes that names the head it trusts
    Trust {
        /// As many characters of the head's hash as the maintainer read
        head: String,
    },
    /// An option that names an action kelpie carries out
    Do {
        /// Which one
        action: RuleAction,
    },
}

/// What kelpie does, in code, when an option that names it is chosen
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    /// Rerun the failed jobs of the head
    Rerun,
    /// Restore the uncommitted change to an agent file in the worktree
    Restore,
}

#[cfg(test)]
mod tests {
    use super::*;

    // The ticket of GitHub issue `number`
    fn ticket(number: u64) -> TicketId {
        TicketId::from_issue(number).expect("a test's issue is not zero")
    }

    fn every_request() -> Vec<Request> {
        vec![
            Request::Status,
            Request::Add(ticket(7)),
            Request::Rework(71),
            Request::Adopt(72),
            Request::Rule(ticket(71), RuleAnswer::Yes),
            Request::Gate(Some(ticket(7))),
            Request::Drop(Some(ticket(7))),
            Request::Timings(10),
            Request::Attach(ticket(7), 401, Some(402)),
            Request::Detach(ticket(7), 401),
            Request::Tell("hold #4".to_owned()),
            Request::PmAttach(401, Some(402)),
            Request::PmDetach(401),
            Request::Drain { at_tool: true },
            Request::Undrain { keep: true },
            Request::Finish,
            Request::Start,
            Request::Pausing,
            Request::Board(Some((ticket(282), 2))),
            Request::Triage(9),
            Request::Watch,
        ]
    }

    #[test]
    fn every_request_round_trips() {
        for request in every_request() {
            let shown = serde_json::to_string(&request).unwrap();
            assert_eq!(
                serde_json::from_str::<Request>(&shown).unwrap(),
                request,
                "{shown}"
            );
        }
    }

    #[test]
    fn a_request_carries_the_action_the_trigger_names() {
        let named: Vec<&str> = every_request().iter().map(Request::action).collect();
        assert_eq!(
            named,
            [
                "status", "add", "rework", "adopt", "rule", "gate", "drop", "timings", "attach",
                "detach", "tell", "pm", "pm", "drain", "undrain", "finish", "start", "pausing",
                "board", "triage", "watch",
            ]
        );
    }

    #[test]
    fn every_variant_reads_from_the_name_its_frame_carries() {
        for request in every_request() {
            let frame = serde_json::to_value(&request).unwrap();
            let name = frame["action"].as_str().expect("a frame names its action");
            assert!(Request::reads(name), "{name} reads as nothing");
        }
        assert!(!Request::reads("pm"), "`pm` names both pm verbs");
    }

    #[test]
    fn an_answer_names_what_it_gives() {
        for answer in [
            RuleAnswer::Yes,
            RuleAnswer::No {
                note: "not yet".to_owned(),
            },
            RuleAnswer::Rework {
                note: "one more".to_owned(),
            },
            RuleAnswer::Text {
                text: "the third one".to_owned(),
            },
            RuleAnswer::Trust {
                head: "567efe1".to_owned(),
            },
            RuleAnswer::Do {
                action: RuleAction::Rerun,
            },
        ] {
            let shown = serde_json::to_string(&answer).unwrap();
            assert_eq!(
                serde_json::from_str::<RuleAnswer>(&shown).unwrap(),
                answer,
                "{shown}"
            );
        }
    }
}
