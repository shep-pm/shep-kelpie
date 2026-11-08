//! A forge call the test holds open

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{PoisonError, mpsc};
use std::time::Duration;

use super::FakeForge;

// Which forge call a test waits on
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    // The next call the runner makes, whichever it is
    Any,
    // The read of `main`'s workflow runs, which a pass makes for every work item
    MainRuns,
    // The read of one issue, which a review round makes for its criteria
    Issue,
    // The listing of the open issues, which triage's poll makes
    Listing,
    // The listing of the ready issues, which the board's poll starts with
    Board,
    // The read of one pull request, which a parked work item's check makes
    PullRequest,
    // The listing of the open pull requests, which following the bumps makes
    OpenPullRequests,
    // The read of a review bot's activity on one pull request, which a bot's round makes
    Activity,
    // The read of the comments and threads on one pull request, which a talk step makes
    Talk,
    // The read of the end of the failed runs' logs, which a red run makes
    FailedLogs,
    // The read of where a pull request stands in the merge queue, which a merge makes
    MergeQueue,
    // The read of the end of one run's failed log, which a wake makes for each workflow red on `main`
    RunLogTail,
    // The read of one run's log, which a wake makes for a failed run and for a log the project manager asked for
    RunLog,
    // The read of an issue's comments, which a worker's first turn makes
    IssueComments,
    // The read of the login's rate limits, which a pass makes once a minute
    RateLimit,
    // The rerun of a workflow's failed jobs, which a workflow red on `main` is given
    Rerun,
    // The comment on an issue or a pull request, which a ruling released from its hold is posted as
    Comment,
    // The merge of a pull request, which a green bump is given
    Merge,
    // The marking of a draft pull request ready
    MarkReady,
}

// Who reached the forge and who lets it go
#[derive(Debug)]
pub struct Gate {
    entered: Sender<()>,
    release: Receiver<()>,
    at: At,
}

/// A forge that holds its next call open until this is dropped
///
/// A pass asks the forge with the runner's lock held, and each call waits its
/// turn behind the login's pace, so a call held here is a pass that holds the
/// lock. A test takes one to ask a trigger of a runner in the middle of a pass.
#[derive(Debug)]
pub struct HeldCall {
    entered: Receiver<()>,
    // Dropping it is what lets the call through
    _release: Sender<()>,
}

impl Gate {
    /// A gate for `at`, and the call it holds until the test drops it
    pub(crate) fn open(at: At) -> (Self, HeldCall) {
        let (entered, heard) = mpsc::channel();
        let (release, waited) = mpsc::channel();
        let gate = Self {
            entered,
            release: waited,
            at,
        };
        let held = HeldCall {
            entered: heard,
            _release: release,
        };
        (gate, held)
    }

    /// Says the call reached the gate, and waits there until the test lets it go
    pub(crate) fn pass(self) {
        let _ = self.entered.send(());
        let _ = self.release.recv();
    }
}

impl HeldCall {
    /// Whether a call reached the forge and waits here, within `within`
    pub fn waiting(&self, within: Duration) -> bool {
        self.entered.recv_timeout(within).is_ok()
    }
}

impl FakeForge {
    /// Holds the forge's next call open, so a test can ask a trigger of a
    /// runner whose pass is in the middle of it
    pub fn hold_next_call(&self) -> HeldCall {
        self.hold_open(At::Any)
    }

    /// Holds the next read of `main`'s workflow runs open, and lets every
    /// other call through
    pub fn hold_next_main_runs_read(&self) -> HeldCall {
        self.hold_open(At::MainRuns)
    }

    /// Holds the next read of one issue open, and lets every other call through
    pub fn hold_next_issue_read(&self) -> HeldCall {
        self.hold_open(At::Issue)
    }

    /// Holds the next listing of the open issues open, and lets every other
    /// call through
    pub fn hold_next_listing_read(&self) -> HeldCall {
        self.hold_open(At::Listing)
    }

    /// Holds the next listing of the ready issues open, and lets every other call through
    pub fn hold_next_board_read(&self) -> HeldCall {
        self.hold_open(At::Board)
    }

    /// Holds the next read of one pull request open, and lets every other call through
    pub fn hold_next_pull_request_read(&self) -> HeldCall {
        self.hold_open(At::PullRequest)
    }

    /// Holds the next listing of the open pull requests open, and lets every other call through
    pub fn hold_next_open_pull_requests_read(&self) -> HeldCall {
        self.hold_open(At::OpenPullRequests)
    }

    /// Holds the next read of a review bot's activity open, and lets every other call through
    pub fn hold_next_activity_read(&self) -> HeldCall {
        self.hold_open(At::Activity)
    }

    /// Holds the next read of a pull request's comments open, and lets every other call through
    pub fn hold_next_talk_read(&self) -> HeldCall {
        self.hold_open(At::Talk)
    }

    /// Holds the next read of the failed runs' logs open, and lets every other call through
    pub fn hold_next_failed_logs_read(&self) -> HeldCall {
        self.hold_open(At::FailedLogs)
    }

    /// Holds the next read of a pull request's merge queue standing open, and lets every other
    /// call through
    pub fn hold_next_merge_queue_read(&self) -> HeldCall {
        self.hold_open(At::MergeQueue)
    }

    /// Holds the next read of a workflow run's log tail open, and lets every other call through
    pub fn hold_next_run_log_tail_read(&self) -> HeldCall {
        self.hold_open(At::RunLogTail)
    }

    /// Holds the next read of one run's log open, and lets every other call through
    pub fn hold_next_run_log_read(&self) -> HeldCall {
        self.hold_open(At::RunLog)
    }

    /// Holds the next read of an issue's comments open, and lets every other call through
    pub fn hold_next_issue_comments_read(&self) -> HeldCall {
        self.hold_open(At::IssueComments)
    }

    /// Holds the next read of the login's rate limits open, and lets every other call through
    pub fn hold_next_rate_limit_read(&self) -> HeldCall {
        self.hold_open(At::RateLimit)
    }

    /// Holds the next rerun of a workflow's failed jobs open, and lets every other call through
    pub fn hold_next_rerun(&self) -> HeldCall {
        self.hold_open(At::Rerun)
    }

    /// Holds the next comment open, and lets every other call through
    pub fn hold_next_comment(&self) -> HeldCall {
        self.hold_open(At::Comment)
    }

    /// Holds the next merge open, and lets every other call through
    pub fn hold_next_merge(&self) -> HeldCall {
        self.hold_open(At::Merge)
    }

    /// Holds the next marking of a pull request ready open, and lets every other call through
    pub fn hold_next_mark_ready(&self) -> HeldCall {
        self.hold_open(At::MarkReady)
    }

    fn hold_open(&self, at: At) -> HeldCall {
        let (gate, held) = Gate::open(at);
        *self.hold.lock().unwrap_or_else(PoisonError::into_inner) = Some(gate);
        held
    }

    /// Holds one call here until the test drops the [`HeldCall`] it took.
    /// A gate named for one call waits for that call and lets the rest through, and one for
    /// [`At::Any`] waits at the first call that asks the forge.
    pub fn wait_if_held(&self, at: At) {
        let gate = {
            let mut hold = self.hold.lock().unwrap_or_else(PoisonError::into_inner);
            match hold.as_ref() {
                Some(gate) if gate.at == at => hold.take().expect("checked"),
                _ => return,
            }
        };
        gate.pass();
    }
}
