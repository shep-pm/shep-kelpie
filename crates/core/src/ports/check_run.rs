//! The check runs kelpie's App shows beside CI on a pull request's head

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
            CheckState::Queued | CheckState::InProgress => None,
        }
    }
}
