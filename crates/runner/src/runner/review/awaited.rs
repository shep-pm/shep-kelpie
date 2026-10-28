//! CI on the head a reviewer is about to read
//!
//! A red run is a sure defect that costs no model's read, so no reviewer reads
//! a head until it is pushed and green (ADR 0008). A round about to start a
//! reviewer or summon a bot first waits for the run on that head: two minutes
//! for checks to register, then pending waits, green goes on, and red is the
//! worker's CI fix turn, counted toward `ci.fix_attempts`. A head green stays so
//! for the pass; with `ci.block` off the round starts at once. Checks still none
//! once settled on a head behind `main` are the gate's catch-up, run from here.
//! Both the fix turn and the catch-up come back to this round.

use super::super::Runner;
use super::super::gate::settled;
use super::super::report::Begin;
use crate::ports::{Checks, PullRequestState};
use crate::state::{StateError, Stuck};
use crate::work_item::{Awaited, Phase, Review};
use crate::worktree::Base;

impl Runner {
    /// What the step does instead of `review`'s reviewer reading the pushed
    /// head, when CI has not passed on it: wait, or send the worker its CI
    /// fix
    ///
    /// `None` lets the reviewer go ahead, and keeps in `review` the run it
    /// waited on.
    pub(super) fn before_read(&mut self, review: &mut Review) -> Result<Option<Begin>, StateError> {
        if !self.settings.ci.block {
            return Ok(None);
        }
        let Some(head) = review.reading.clone() else {
            return Ok(None);
        };
        if review
            .ci
            .as_ref()
            .is_some_and(|ci| ci.green && ci.head == head)
        {
            return Ok(None);
        }
        let Some(item) = self.current() else {
            return Ok(Some(Begin::Idle));
        };
        let issue = item.issue;
        let Some(number) = item.pull_request else {
            return Ok(Some(Begin::Idle));
        };
        // Read with the runner's lock let go, and the step made again with the result
        let pr = match self.round_view(number) {
            Ok(pr) => pr,
            Err(begin) => return Ok(Some(*begin)),
        };
        // A pull request merged or closed by hand ends as the gate ends it,
        // and no reviewer reads it.
        match pr.state {
            PullRequestState::Open => {}
            PullRequestState::Merged => {
                return self.merged(issue, number, pr.head, false).map(Some);
            }
            PullRequestState::Closed => {
                return self.raise(number, Stuck::Closed.into()).map(Some);
            }
        }
        // The forge lags a push by a moment, and its checks are of its head.
        if pr.head != head {
            return Ok(Some(Begin::Idle));
        }
        let now = self.ports.clock.now();
        let since = (review.ci.as_ref())
            .filter(|ci| ci.head == head)
            .map_or(now, |ci| ci.since);
        self.note_checks(&pr.checks);
        let mut ci = Awaited {
            head: head.clone(),
            since,
            green: false,
        };
        let mut conclusion = match &pr.checks {
            Checks::Failed(_) => "failed",
            Checks::Skipped => "skipped",
            _ => "passed",
        };
        let failed = match &pr.checks {
            Checks::Failed(checks) => checks.clone(),
            _ => Vec::new(),
        };
        let queued = match &pr.checks {
            Checks::Pending(queued) => queued.clone(),
            _ => Vec::new(),
        };
        let unreported = pr.checks == Checks::None;
        // A bot's own check, skipped, is all a draft that ran no workflow shows,
        // as one that conflicts with `main` does.
        let unrun_draft = pr.draft && pr.checks == Checks::Skipped;
        let mut verdict = match pr.checks {
            _ if !settled(since, now) => None,
            Checks::None | Checks::Pending(_) => None,
            // Nothing ran on a draft whose jobs all skip it, and the real
            // run starts only once it is marked ready, which a bot's round
            // does after this wait: so a draft waits, and doctor says why.
            Checks::Skipped if pr.draft => None,
            Checks::Passed | Checks::Skipped => {
                ci.green = true;
                Some(Ok(()))
            }
            Checks::Failed(checks) => Some(Err(checks)),
        };
        self.keep_ci(review, &ci)?;
        let back = Review {
            reading: None,
            ..review.clone()
        };
        if let Some(begin) = self.check_queued(number, &head, since, &queued, Some(&back))? {
            return Ok(Some(begin));
        }
        if (unreported || unrun_draft) && settled(since, now) {
            // Only a head with no run at all is caught up here: one that does
            // get CI is not rebased mid-review.
            match self.base_standing(&head) {
                Ok(Base::Behind) => return self.rebase(number, &head, Some(back)).map(Some),
                Ok(Base::Lagging) => {}
                Ok(Base::Current) if unrun_draft => {}
                Ok(Base::Current) => {
                    match self.without_checks(number, &head, since, Some(&back))? {
                        Some(begin) => return Ok(Some(begin)),
                        None => {
                            (ci.green, conclusion, verdict) = (true, "skipped", Some(Ok(())));
                            self.keep_ci(review, &ci)?;
                        }
                    }
                }
                Err(begin) => return Ok(Some(*begin)),
            }
        }
        if verdict.is_some() {
            self.ci_settled(issue, number, &head, conclusion, &failed);
        }
        match verdict {
            None => Ok(Some(Begin::Idle)),
            Some(Ok(())) => Ok(None),
            Some(Err(checks)) => self.ci_failed(number, head, checks, Some(back)).map(Some),
        }
    }

    // Keeps `ci`, the wait on the head being read, in the round and the state file.
    fn keep_ci(&mut self, review: &mut Review, ci: &Awaited) -> Result<(), StateError> {
        if review.ci.as_deref() == Some(ci) {
            return Ok(());
        }
        review.ci = Some(Box::new(ci.clone()));
        let ci = ci.clone();
        self.update(|item| {
            if let Phase::Review(review) = &mut item.phase {
                review.ci = Some(Box::new(ci));
            }
        })
    }
}
