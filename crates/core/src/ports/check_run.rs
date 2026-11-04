//! The check runs kelpie's App shows beside CI on a pull request's head

/// The check run on a work item's review
pub const REVIEW_CHECK: &str = "kelpie / review";
/// The check run on a ruling a work item waits on
pub const RULING_CHECK: &str = "kelpie / ruling";
/// The check run summing up where a work item stands
pub const SUMMARY_CHECK: &str = "kelpie";

/// Whether `name` is one of the check runs kelpie posts, which are its own
/// gates and never CI
pub fn is_gate_check(name: &str) -> bool {
    [REVIEW_CHECK, RULING_CHECK, SUMMARY_CHECK].contains(&name)
}

/// The forge's number for a check run, which an update names it by
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CheckRunId(pub u64);

/// How a finished check run ended
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conclusion {
    /// The gate passed
    Success,
    /// The gate failed
    Failure,
    /// The gate ended with no verdict: the work item left it
    Skipped,
}

/// Where a check run stands
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    /// Waiting to start
    Queued,
    /// Running
    InProgress,
    /// Over, ending this way
    Completed(Conclusion),
}

/// One check run as kelpie shows it: a gate's name, where it stands and what
/// the maintainer reads on the pull request
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateCheck {
    /// The check's name, such as `kelpie / review`
    pub name: String,
    /// Where it stands
    pub state: CheckState,
    /// The line shown beside its name
    pub title: String,
    /// The text under the title
    pub summary: String,
}

impl GateCheck {
    /// The forge's word for the check's status
    pub fn status(&self) -> &'static str {
        match self.state {
            CheckState::Queued => "queued",
            CheckState::InProgress => "in_progress",
            CheckState::Completed(_) => "completed",
        }
    }

    /// The forge's word for how the check ended, once it has
    pub fn conclusion(&self) -> Option<&'static str> {
        match self.state {
            CheckState::Completed(Conclusion::Success) => Some("success"),
            CheckState::Completed(Conclusion::Failure) => Some("failure"),
            CheckState::Completed(Conclusion::Skipped) => Some("skipped"),
            CheckState::Queued | CheckState::InProgress => None,
        }
    }
}
