//! Turns that end without a reply to act on: one that failed, and one that
//! ran past its ceiling
//!
//! Each parks the work item on a ruling whose yes puts the turn back, and
//! whose no stops the work item.

use std::time::Duration;

use crate::ports::Timestamp;
use crate::runner::Runner;
use crate::runner::report::{Begin, StepReport};
use crate::runner::ruling::park;
use crate::state::{ProjectState, RulingKind, StateError};
use crate::work_item::Turn;

impl Runner {
    pub(super) fn turn_ceiling(&self) -> Duration {
        Duration::from_secs(u64::from(self.settings.worker.turn_timeout.get()) * 60)
    }

    pub(super) fn park_ceiling_passed(&mut self, now: Timestamp) -> Result<Begin, StateError> {
        let mut next = self.state.clone();
        if let Some(item) = next.work_item.as_mut() {
            item.turn = Turn::Ended { at: now };
        }
        let mut report = timed_out(self.project.as_str(), &mut next);
        self.save(next)?;
        self.fill_comment_failed(&mut report);
        Ok(Begin::Report(report))
    }
}

// Parks the work item on a turn-ceiling ruling and builds its report. Shared
// by a call that actually hit `ClaudeError::TimedOut` and by a restart that
// finds a turn already past its ceiling with no call spent. The caller sets
// `item.turn` beforehand: this only raises the ruling. `comment_failed` is
// filled in afterwards, once the ruling has actually been posted.
pub(super) fn timed_out(project: &str, next: &mut ProjectState) -> StepReport {
    let item = next
        .work_item
        .as_mut()
        .expect("a turn ceiling is about a work item");
    let (issue, session, pull_request) = (item.issue, item.session.clone(), item.pull_request);
    let phase = Some(item.phase.clone());
    let (_, id, question) = park(
        project,
        next,
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
pub(super) fn failed(
    project: &str,
    next: &mut ProjectState,
    at: Timestamp,
    reason: String,
) -> StepReport {
    let item = next
        .work_item
        .as_mut()
        .expect("a failed turn is about a work item");
    let failure = Turn::Failed {
        at,
        reason: reason.clone(),
    };
    let retry = std::mem::replace(&mut item.turn, failure);
    let (issue, pull_request) = (item.issue, item.pull_request);
    let kind = RulingKind::TurnFailed {
        reason,
        phase: item.phase.clone(),
        retry,
    };
    let (_, id, question) = park(project, next, pull_request, kind);
    StepReport::Failed {
        issue,
        pull_request,
        id,
        question,
        comment_failed: None,
    }
}
