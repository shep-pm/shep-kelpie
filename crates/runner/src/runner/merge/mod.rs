//! The merge a yes or `auto` allows, and the cleanup after it
//!
//! A merge takes only the head it was asked about, while CI on it is still
//! green and it still has the latest `main`. Otherwise kelpie withdraws it,
//! goes back to CI, and asks or merges again. Under `auto` a refused merge
//! takes that path too, and a second refusal raises a ruling. Every step is
//! saved before the next, so a runner restarted mid-merge picks up where it
//! stopped.

use std::fmt;

use super::Runner;
use super::gate::{settled, short};
use super::report::{Begin, StepReport};
use super::trigger::WhichItem;
use crate::ports::{Checks, PullRequestState};
use crate::settings::RulingName;
use crate::state::{MergeNotice, Notice, RulingKind, StateError, Stuck};
use crate::ticket::TicketId;
use crate::work_item::{Phase, Review, ReviewCallState, ReviewStage, Turn};
use crate::worktree::{self, Base};
use call::Merged;

/// Why `drop` was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropError {
    /// The trigger named no work item, or one not open
    Which(WhichItem),
    /// The worker's turn is running, and the work item stays until it ends
    TurnRunning(TicketId),
    /// A review call is running, and the work item stays
    /// until it ends, since its result runs outside the runner's lock
    ReviewRunning(TicketId),
    /// A yes is being carried out, and the merge is not stopped halfway
    Merging(TicketId),
    /// The maintainer's `attach`, by this process, holds the work item
    Attached(TicketId, u32),
    /// Its worktree, branch or build folder could not be removed
    Cleanup(String),
    /// The change could not be saved
    State(StateError),
}

impl fmt::Display for DropError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Which(e) => e.fmt(f),
            Self::TurnRunning(issue) => {
                write!(f, "the worker's turn on {} is running", issue.shown())
            }
            Self::ReviewRunning(issue) => {
                write!(f, "a review round on {} is running", issue.shown())
            }
            Self::Merging(issue) => {
                write!(f, "the work item for {} is merging", issue.shown())
            }
            Self::Attached(issue, pid) => write!(
                f,
                "the work item for {} is attached, by process {pid}: quit that claude \
                 session first, which lets it go",
                issue.shown()
            ),
            Self::Cleanup(reason) => f.write_str(reason),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for DropError {}

impl Runner {
    /// Ends a work item without merging it, the one `ticket` names or the
    /// only one open
    ///
    /// Its worktree, local branch and build folder go, and its issue is
    /// recorded as finished. Its pull request and branch on the forge stay,
    /// the pull request labelled `ready-for-human`. It works while draining.
    ///
    /// # Errors
    ///
    /// [`DropError`] when nothing is in flight, or a turn, review round or
    /// merge is under way. Or when the cleanup fails. The work item stays then.
    pub fn drop_work_item(&mut self, ticket: Option<TicketId>) -> Result<(), DropError> {
        self.choose(ticket).map_err(DropError::Which)?;
        let item = self.current().expect("the work item chosen");
        if matches!(item.turn, Turn::Running { .. }) {
            return Err(DropError::TurnRunning(item.issue));
        }
        if matches!(item.review_call, ReviewCallState::Running { .. }) {
            return Err(DropError::ReviewRunning(item.issue));
        }
        // Its worktree is the attached session's working folder.
        if let Some(attached) = item.attached.as_ref().filter(|a| super::attach::live(a)) {
            return Err(DropError::Attached(item.issue, attached.by.pid));
        }
        // A merged pull request parked on its follow-up ruling is past its
        // merge: dropping it would hand the merged pull request back as unmerged.
        let follow_up_parked = |id| {
            let ruling = self.state.rulings.iter().find(|r| r.id == id);
            ruling.is_some_and(|r| matches!(r.kind, RulingKind::FollowUp { .. }))
        };
        if matches!(
            item.phase,
            Phase::Merge { .. } | Phase::Done { merged: true, .. }
        ) || matches!(item.phase, Phase::Ruling { id } if follow_up_parked(id))
        {
            return Err(DropError::Merging(item.issue));
        }
        let summoning = |review: &Review| {
            matches!(
                review.stage,
                ReviewStage::Summon { .. } | ReviewStage::Summoned { .. }
            )
        };
        if matches!(&item.phase, Phase::Review(review) if summoning(review)) {
            self.leave_round();
        }
        match self.cleanup(false, false).map_err(DropError::State)? {
            Begin::Report(StepReport::GateFailed { reason, .. }) => Err(DropError::Cleanup(reason)),
            _ => Ok(()),
        }
    }

    /// Whether kelpie merges a head that passed every gate without asking:
    /// `rulings.pm_decides.merge`
    pub(super) fn merges_unasked(&self) -> bool {
        self.settings.rulings.pm_decides(RulingName::Merge)
    }

    // Marking the draft ready can start a fresh CI run on the same head, so
    // that pass ends there. A later pass, once the checks have had time to
    // register, merges only on a green run.
    pub(super) fn merge(&mut self) -> Result<Begin, StateError> {
        let Some(item) = self.current() else {
            return Ok(Begin::Idle);
        };
        let (issue, Some(number)) = (item.issue, item.pull_request) else {
            return Ok(Begin::Idle);
        };
        let Phase::Merge {
            head,
            readied,
            auto,
        } = item.phase.clone()
        else {
            return Ok(Begin::Idle);
        };
        let tried = item.merge_tried.clone();
        // Read with the runner's lock let go, and the step made again with the result
        let pr = match self.gate_view(number) {
            Ok(pr) => pr,
            Err(begin) => return Ok(*begin),
        };
        match pr.state {
            PullRequestState::Open => {}
            // Kelpie's own merge at this head, when a restart or a lost
            // answer hid it, still gets its notice under `auto`.
            PullRequestState::Merged => {
                let notice = (auto && pr.head == head) || tried == Some(pr.head.clone());
                return self.merged(issue, number, pr.head, notice);
            }
            PullRequestState::Closed => {
                self.update(|item| item.merge_queued = None)?;
                return self.raise(number, Stuck::Closed.into());
            }
        }
        // Queued, the pull request is tested on top of the ones ahead of it
        // while `main` moves under it, so none of the gates below apply.
        if let Some(queued) = item.merge_queued {
            return self.queued(number, head, queued);
        }
        // The gate asks again once `merge` is no longer `true`.
        if auto && !self.merges_unasked() {
            let reason = "`merge` is no longer `true`".to_owned();
            return self.withdraw(issue, number, auto, reason);
        }
        let now = self.ports.clock.now();
        let ci = self.settings.ci.block;
        let settling = ci && readied.is_some_and(|at| !settled(at, now));
        if pr.head != head {
            let reason = format!("#{number} moved to {}", short(&pr.head));
            return self.withdraw(issue, number, auto, reason);
        }
        // A run that marking the draft ready started is waited for; before
        // that, anything but green withdraws the yes.
        let unchecked = match pr.checks == Checks::None && ci {
            true => match self.expects_no_checks(&head) {
                Ok(expected) => expected,
                Err(begin) => return Ok(*begin),
            },
            false => false,
        };
        let green = match pr.checks {
            _ if !ci => true,
            Checks::None if unchecked => true,
            Checks::Passed | Checks::Skipped => true,
            Checks::None | Checks::Pending(_) if readied.is_some() => false,
            Checks::None | Checks::Pending(_) | Checks::Failed(_) => {
                let reason = format!("CI on #{number} is no longer green");
                return self.withdraw(issue, number, auto, reason);
            }
        };
        match self.base_standing(&head) {
            Ok(Base::Current) => {}
            Ok(Base::Lagging) => return Ok(Begin::Idle),
            Ok(Base::Behind) => {
                let since = if auto {
                    "the gate passed"
                } else {
                    "the question"
                };
                let reason = format!("main moved since {since}");
                return self.withdraw(issue, number, auto, reason);
            }
            Err(begin) => return Ok(*begin),
        }
        if pr.draft {
            let marked = match self.ready_marked(number) {
                Ok(marked) => marked,
                Err(begin) => return Ok(*begin),
            };
            if let Err(e) = marked {
                return Ok(self.gate_failed(format!("cannot mark #{number} ready: {e}")));
            }
            let readied = Some(now);
            self.update(|item| {
                item.phase = Phase::Merge {
                    head,
                    readied,
                    auto,
                };
                item.known.ready = true;
            })?;
            return Ok(Begin::Report(StepReport::MarkedReady {
                issue,
                pull_request: number,
            }));
        }
        // The forge already shows it ready: either this pass just read that
        // back, or a restart landed between the call above and its update.
        // Either way it is kelpie's own doing, not a foreign change, and the
        // gate must not mistake it for one on its next look.
        if !item.known.ready {
            self.update(|item| item.known.ready = true)?;
        }
        if settling || !green {
            return Ok(Begin::Idle);
        }
        // A queued pull request is not merged yet and a second merge call
        // would refuse it, so a restart that lost the mark finds it here.
        let standing = match self.queue_standing(number) {
            Ok(standing) => standing,
            Err(begin) => return Ok(*begin),
        };
        if standing.queued || standing.armed {
            return self.queue_marked(standing.removals);
        }
        let removals = standing.removals;
        let made = match self.merge_made(number, &head, auto) {
            Ok(made) => made,
            Err(begin) => return Ok(*begin),
        };
        match made {
            Merged::Refused(reason) => Ok(self.gate_failed(reason)),
            Merged::Waiting => self.queue_marked(removals),
            Merged::Declined(reason) => self.refused(issue, number, head, reason),
            Merged::Unread(e) | Merged::Read(Err(e)) => {
                Ok(self.gate_failed(format!("cannot read #{number}: {e}")))
            }
            Merged::Read(Ok(pr)) if pr.state == PullRequestState::Open => {
                self.queue_marked(removals)
            }
            Merged::Read(Ok(_)) => self.merged(issue, number, head, auto),
        }
    }

    // Nobody is asked before a merge under `auto`, so a head the gates never
    // saw is adopted into the worktree and goes back through every gate.
    pub(super) fn regate(&mut self, from: &str, tip: &str) -> Result<(), String> {
        let item = self.current().ok_or("the work item is gone")?;
        let (repo, branch) = (&self.settings.git.checkout, &item.branch);
        worktree::adopt(repo, &item.worktree, branch, from, tip, &item.remote)
            .map_err(|e| format!("cannot bring the worktree to {}: {e}", short(tip)))?;
        let tip = Some(tip.to_owned());
        self.update(|item| {
            item.known.head = tip;
            item.phase = Phase::Review(Review::first());
        })
        .map_err(|e| e.to_string())
    }

    // The notice is saved with the merge, so it goes out exactly once.
    pub(super) fn merged(
        &mut self,
        issue: TicketId,
        number: u64,
        head: String,
        notice: bool,
    ) -> Result<Begin, StateError> {
        let mut next = self.state.clone();
        let Some(item) = self.current_in(&mut next) else {
            return Ok(Begin::Idle);
        };
        item.phase = Phase::Done {
            merged: true,
            closed: false,
        };
        let notice = notice.then_some(MergeNotice {
            issue,
            pull_request: number,
            head,
        });
        next.notices.extend(notice.clone().map(Notice::Merged));
        self.save(next)?;
        // A comment that fails is told once and not retried: the webhook's
        // retry is the notice's, and nothing here waits on the comment.
        if let Some(notice) = &notice {
            self.notice_on(number, &super::voice::merged_notice(notice));
        }
        self.finish(true)
    }

    fn withdraw(
        &mut self,
        issue: TicketId,
        number: u64,
        auto: bool,
        reason: String,
    ) -> Result<Begin, StateError> {
        let since = self.ports.clock.now();
        self.update(|item| item.phase = Phase::Ci { head: None, since })?;
        Ok(Begin::Report(if auto {
            StepReport::MergeWithdrawn {
                issue,
                pull_request: number,
                reason,
            }
        } else {
            StepReport::YesWithdrawn {
                issue,
                pull_request: number,
                reason,
            }
        }))
    }

    // The first refusal under `auto` goes back to CI, which catches the
    // branch up with `main`. A second one, on any head, asks.
    fn refused(
        &mut self,
        issue: TicketId,
        number: u64,
        head: String,
        reason: String,
    ) -> Result<Begin, StateError> {
        let Some(item) = self.current() else {
            return Ok(Begin::Idle);
        };
        let refused_before = item.merge_refused;
        let tried = Some(head.clone());
        self.update(|item| {
            item.merge_refused = true;
            item.merge_tried = tried;
        })?;
        if refused_before {
            return self.raise(number, Stuck::MergeRefused { head, why: reason }.into());
        }
        self.withdraw(issue, number, true, reason)
    }

    // Ends a work item that finished.
    pub(super) fn finish(&mut self, merged: bool) -> Result<Begin, StateError> {
        self.cleanup(merged, true)
    }
}

pub(in crate::runner) mod call;
mod cleanup;
mod queue;

#[cfg(test)]
mod auto;
#[cfg(test)]
mod parent;

#[cfg(test)]
mod close;

#[cfg(test)]
mod tests;
