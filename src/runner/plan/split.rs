//! Splitting an issue into sub-issues on the forge, and closing it once
//! they are all closed
//!
//! Each piece is opened, then linked with its blockers, saving after each
//! forge change so a restart carries on where it stopped. A split or close
//! the forge keeps refusing waits on a ruling, so the board goes on.

use super::pick::RepoLabels;
use super::{PARENT_CLOSED, TRIES, put_plan};
use crate::board::{READY, ReadyIssue};
use crate::plan::{self, Piece, PieceWorker, Plan, Stage};
use crate::runner::Runner;
use crate::runner::report::{Begin, StepReport};
use crate::state::{ProjectState, RulingKind, StateError};

impl Runner {
    /// Carries on the split under way, if one is and it is not parked
    pub(in crate::runner) fn split_under_way(&mut self) -> Result<Option<Begin>, StateError> {
        let splitting = self.state.plans.iter();
        let Some(issue) = splitting
            .filter(|p| matches!(p.stage, Stage::Splitting { failures, .. } if failures < TRIES))
            .map(|p| p.issue)
            .next()
        else {
            return Ok(None);
        };
        let report = match self.split(issue)? {
            Err(reason) => self.split_refused(issue, reason)?,
            Ok(report) => report,
        };
        Ok(Some(Begin::Report(report)))
    }

    // Counts a refused split step, and parks the split on a ruling once the
    // forge has refused `TRIES` steps in a row, so the board goes on.
    fn split_refused(&mut self, issue: u64, reason: String) -> Result<StepReport, StateError> {
        let mut next = self.state.clone();
        let mut stuck = None;
        if let Some(Plan {
            stage: Stage::Splitting {
                opened, failures, ..
            },
            ..
        }) = next.plans.iter_mut().find(|p| p.issue == issue)
        {
            *failures += 1;
            stuck = (*failures >= TRIES).then(|| opened.clone());
        }
        let ruling = match stuck {
            Some(opened) => {
                let reason = reason.clone();
                let kind = RulingKind::SplitStuck { reason, opened };
                Some(self.raise_on_issue(next, issue, kind)?.0)
            }
            None => {
                self.save(next)?;
                None
            }
        };
        Ok(StepReport::SplitFailed {
            issue,
            reason,
            ruling,
        })
    }

    // Each piece is opened, then linked as a sub-issue with its blockers.
    // What the forge already shows is not asked for again, so a step whose
    // answer was lost is not repeated. Err is the forge's refusal.
    fn split(&mut self, issue: u64) -> Result<Result<StepReport, String>, StateError> {
        let (forge, repo) = (&self.ports.forge, &self.settings.forge);
        let parent = match forge.issue(repo, issue) {
            Ok(found) => found,
            Err(e) => return Ok(Err(format!("cannot read the issue: {e}"))),
        };
        if !parent.open || !parent.labels.iter().any(|l| l == READY) {
            let mut next = self.state.clone();
            next.plans.retain(|p| p.issue != issue);
            self.save(next)?;
            let reason = if parent.open {
                format!("#{issue} is no longer labelled `{READY}`")
            } else {
                format!("#{issue} was closed")
            };
            return Ok(Ok(StepReport::SplitDropped { issue, reason }));
        }
        let labels: Vec<&str> = parent.labels.iter().map(String::as_str).collect();
        let mut known = RepoLabels::default();
        // Why a sub-issue's pick did not land, by its number, for the comment.
        let mut missed = Vec::new();
        loop {
            let Some(Stage::Splitting {
                why,
                pieces,
                opened,
                linked,
                ..
            }) = self.plan_of(issue).cloned()
            else {
                unreachable!("a split under way is splitting");
            };
            let (forge, repo) = (&self.ports.forge, &self.settings.forge);
            let Some(piece) = pieces.get(linked) else {
                return self
                    .finish_split(issue, &why, &pieces, opened, &missed)
                    .map(Ok);
            };
            let Some(&number) = opened.get(linked) else {
                let made = forge.create_issue(repo, &piece.title, &piece.body, &labels);
                let number = match made {
                    Ok(number) => number,
                    Err(e) => return Ok(Err(format!("cannot open piece {}: {e}", linked + 1))),
                };
                self.change_split(issue, |opened, _| opened.push(number))?;
                continue;
            };
            let shown = match forge.issue(repo, number) {
                Ok(shown) => shown,
                Err(e) => return Ok(Err(format!("cannot read #{number}: {e}"))),
            };
            if shown.parent != Some(issue)
                && let Err(e) = forge.add_sub_issue(repo, issue, number)
            {
                return Ok(Err(format!("cannot make #{number} a sub-issue: {e}")));
            }
            let blockers = piece.blocked_by.iter();
            let blockers =
                blockers.filter_map(|&by| by.checked_sub(1).and_then(|at| opened.get(at)));
            for by in blockers.filter(|by| !shown.blocked_by.contains(by)) {
                if let Err(e) = forge.add_blocker(repo, number, *by) {
                    return Ok(Err(format!("cannot mark #{number} blocked by #{by}: {e}")));
                }
            }
            // The copied parent labels may already carry one, which wins
            // over the piece's own pick. A pick that does not land leaves
            // the sub-issue to the project's own worker, and the comment
            // says why.
            if !plan::already_has_worker(&shown.labels)
                && let Some(label) = plan::resolved_label(piece.worker.as_deref())
                && let Err(reason) = self.put_pick(&mut known, number, &label)
            {
                missed.push((number, reason));
            }
            self.change_split(issue, |_, linked| *linked += 1)?;
        }
    }

    // Leaves the split's one comment on `issue`, with what each sub-issue's
    // worker came to, and drops its plan: the split is done.
    pub(super) fn finish_split(
        &mut self,
        issue: u64,
        why: &str,
        pieces: &[Piece],
        opened: Vec<u64>,
        missed: &[(u64, String)],
    ) -> Result<StepReport, StateError> {
        let (forge, repo) = (&self.ports.forge, &self.settings.forge);
        let workers = pieces.iter().zip(&opened).map(|(piece, &number)| {
            let shown = match forge.issue(repo, number) {
                Ok(shown) => shown,
                Err(e) => return PieceWorker::Unread(e.to_string()),
            };
            if plan::already_has_worker(&shown.labels) {
                return PieceWorker::Labelled;
            }
            let missed = missed.iter().find(|(n, _)| *n == number);
            PieceWorker::Defaulted(missed.map_or_else(
                || plan::fallback_reason(piece.worker.as_deref()),
                |(_, reason)| reason.clone(),
            ))
        });
        let workers: Vec<PieceWorker> = workers.collect();
        let comment = plan::comment(why, pieces, &opened, &workers);
        // `post_comment` takes an issue as well as a pull request.
        let comment_failed = forge.post_comment(repo, issue, &comment).err();
        let mut next = self.state.clone();
        next.plans.retain(|p| p.issue != issue);
        self.save(next)?;
        Ok(StepReport::Split {
            issue,
            sub_issues: opened,
            comment_failed: comment_failed.map(|e| e.to_string()),
        })
    }

    // A step that lands also clears the count of refusals in a row.
    fn change_split(
        &mut self,
        issue: u64,
        change: impl FnOnce(&mut Vec<u64>, &mut usize),
    ) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let plan = next.plans.iter_mut().find(|p| p.issue == issue);
        if let Some(Plan {
            stage:
                Stage::Splitting {
                    opened,
                    linked,
                    failures,
                    ..
                },
            ..
        }) = plan
        {
            change(opened, linked);
            *failures = 0;
        }
        self.save(next)
    }

    /// Closes a ready issue whose sub-issues are all closed, if the board
    /// lists one the forge has not refused too often
    pub(in crate::runner) fn close_split_done(
        &mut self,
        ready: &[ReadyIssue],
    ) -> Result<Option<Begin>, StateError> {
        let done = ready.iter().find(|i| {
            i.sub_issues.all_closed()
                && !matches!(self.plan_of(i.number), Some(Stage::Closing { failures }) if *failures >= TRIES)
        });
        let Some(issue) = done.map(|i| i.number) else {
            return Ok(None);
        };
        let closed = (self.ports.forge).close_issue(&self.settings.forge, issue, PARENT_CLOSED);
        let mut next = self.state.clone();
        let report = match closed {
            Ok(()) => {
                next.plans.retain(|p| p.issue != issue);
                self.save(next)?;
                StepReport::ParentClosed { issue }
            }
            Err(e) => {
                let reason = e.to_string();
                let failures = match self.plan_of(issue) {
                    Some(Stage::Closing { failures }) => failures + 1,
                    _ => 1,
                };
                put_plan(&mut next, issue, Stage::Closing { failures });
                let ruling = if failures >= TRIES {
                    let kind = RulingKind::CloseStuck {
                        reason: reason.clone(),
                    };
                    Some(self.raise_on_issue(next, issue, kind)?.0)
                } else {
                    self.save(next)?;
                    None
                };
                StepReport::ParentCloseFailed {
                    issue,
                    reason,
                    ruling,
                }
            }
        };
        Ok(Some(Begin::Report(report)))
    }
}

// A yes on a stuck split: it tries again from where it stopped.
pub(super) fn retried(next: &mut ProjectState, issue: u64) {
    let plan = next.plans.iter_mut().find(|p| p.issue == issue);
    if let Some(Plan {
        stage: Stage::Splitting { failures, .. },
        ..
    }) = plan
    {
        *failures = 0;
    }
}
