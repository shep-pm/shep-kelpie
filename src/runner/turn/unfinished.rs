//! Turns that end without a reply to act on: one that failed, one that ran
//! past its ceiling, and a first turn that stopped short of a pull request
//!
//! Each parks the work item on a ruling whose yes puts the turn back, and
//! whose no stops the work item. A turn that stopped short is sent back to
//! the worker once before it parks.

use std::time::Duration;

use crate::ports::Timestamp;
use crate::runner::report::{Begin, StepReport};
use crate::runner::ruling::park;
use crate::runner::{Names, Runner};
use crate::state::{ProjectState, RulingKind, StateError};
use crate::work_item::{Phase, Review, Turn};

/// The prompt that sends back a worker whose turn ended with no pull request
/// and no question
const STOPPED_SHORT: &str = "Your last turn ended with no pull request for this \
                             work item and no question. If a tool failed, try it \
                             again or find another way, and open the draft pull \
                             request once the work is done. If only the maintainer \
                             can unblock you, end your reply with a \
                             <kelpie-question> block.";

/// Why a turn that stopped short twice parks
const STOPPED_TWICE: &str = "it ended twice with no pull request and no question";

impl Runner {
    pub(super) fn turn_ceiling(&self) -> Duration {
        Duration::from_secs(u64::from(self.settings.worker.turn_timeout.get()) * 60)
    }

    // A first turn ended with no pull request kelpie knows of and no
    // question. The forge is asked again, since the turn's end may have
    // missed one; with none open, the worker is sent back once, and the
    // next turn that stops short parks it on a ruling, whose yes sends it
    // back again. A forge that cannot be asked is tried again next step.
    pub(super) fn stopped_short(&mut self) -> Result<Begin, StateError> {
        let item = self.current().expect("a turn is a work item's");
        let (issue, sent_back) = (item.issue, item.sent_back);
        let open = match self.ports.forge.open_pull_requests(&self.settings.forge) {
            Ok(open) => open,
            Err(e) => {
                let reason = format!("cannot list open pull requests: {e}");
                return Ok(Begin::Report(StepReport::GateFailed { issue, reason }));
            }
        };
        let found = open.iter().find(|pr| pr.head == item.branch);
        let mut next = self.state.clone();
        let item = next.item_mut(issue).expect("the work item checked above");
        if let Some(pr) = found {
            item.pull_request = Some(pr.number);
            item.phase = Phase::Review(Review::first());
            self.save(next)?;
            return self.begin_item(false);
        }
        item.turn = Turn::Next {
            prompt: STOPPED_SHORT.to_owned(),
        };
        if !sent_back {
            item.sent_back = true;
            self.save(next)?;
            return self.begin_item(false);
        }
        let now = self.ports.clock.now();
        let mut report = failed(
            self.names(),
            &mut next,
            issue,
            now,
            STOPPED_TWICE.to_owned(),
        );
        self.save(next)?;
        self.fill_comment_failed(&mut report);
        Ok(Begin::Report(report))
    }

    pub(super) fn park_ceiling_passed(&mut self, now: Timestamp) -> Result<Begin, StateError> {
        let mut next = self.state.clone();
        let issue = self.current().expect("a turn is a work item's").issue;
        next.item_mut(issue)
            .expect("the work item checked above")
            .turn = Turn::Ended { at: now };
        let mut report = timed_out(self.names(), &mut next, issue);
        self.save(next)?;
        self.fill_comment_failed(&mut report);
        Ok(Begin::Report(report))
    }
}

// Parks the work item on a turn-ceiling ruling and builds its report. Shared
// by a call that actually hit `AgentError::TimedOut` and by a restart that
// finds a turn already past its ceiling with no call spent. The caller sets
// `item.turn` beforehand: this only raises the ruling. `comment_failed` is
// filled in afterwards, once the ruling has actually been posted.
pub(super) fn timed_out(names: Names<'_>, next: &mut ProjectState, issue: u64) -> StepReport {
    let item = next
        .item_mut(issue)
        .expect("a turn ceiling is about an open work item");
    let (session, pull_request) = (item.session.clone(), item.pull_request);
    let phase = Some(item.phase.clone());
    let (id, question) = park(
        names,
        next,
        issue,
        pull_request,
        RulingKind::TurnTimeout { phase },
    );
    StepReport::TimedOut {
        issue,
        session,
        pull_request,
        id,
        question,
        comment_failed: None,
    }
}

// Marks the turn failed and parks the work item on a ruling carrying why,
// keeping the turn as it stood so a yes can put it back. `comment_failed` is
// filled in afterwards, once the ruling has actually been posted.
pub(in crate::runner) fn failed(
    names: Names<'_>,
    next: &mut ProjectState,
    issue: u64,
    at: Timestamp,
    reason: String,
) -> StepReport {
    let item = next
        .item_mut(issue)
        .expect("a failed turn is about an open work item");
    let failure = Turn::Failed {
        at,
        reason: reason.clone(),
    };
    let retry = std::mem::replace(&mut item.turn, failure);
    let pull_request = item.pull_request;
    let kind = RulingKind::TurnFailed {
        reason,
        phase: item.phase.clone(),
        retry,
    };
    let (id, question) = park(names, next, issue, pull_request, kind);
    StepReport::Failed {
        issue,
        pull_request,
        id,
        question,
        comment_failed: None,
    }
}
