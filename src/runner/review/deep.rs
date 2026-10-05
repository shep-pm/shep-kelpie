//! The deep review round: two reads, then one fix turn
//!
//! A fresh session on the `deep_reviewer` role reads the whole pull request
//! for defects, and a second fresh one, shown what the first found, reads it
//! for what the first missed. What the two hold goes to the worker the way
//! any reviewer's findings do: one fix turn when anything is above a nit,
//! then the next reviewer in the project's list.

mod prompts;
#[cfg(test)]
mod tests;

use super::calls::{build_call, diff_against, shots_prompt};
use crate::pacer::Scope;
use crate::ports::{Finding, Role, Tools, read_review};
use crate::runner::Runner;
use crate::runner::report::{Begin, ReviewCall, ReviewResult, Spent, StepReport};
use crate::runner::shots::RoundShots;
use crate::settings::LoopReviewer;
use crate::state::StateError;
use crate::work_item::{CallKind, Deep, Phase, Review, ReviewStage};

/// Where a reader's throwaway settings go
const READ_SETTINGS_FILE: &str = "deep-read-settings.json";

impl Runner {
    // The deep round begins: who reviews it and that it has started are kept,
    // so a restart resumes at the step it was on.
    pub(super) fn deep_started(
        &mut self,
        chosen: &LoopReviewer,
        review: Review,
    ) -> Result<Begin, StateError> {
        let review = Review {
            reviewer: Some(chosen.name.clone()),
            stage: ReviewStage::Deep(Deep::Read),
            ..review
        };
        self.update(|item| {
            // It reads every file, so none is left for a local round.
            item.local_unreviewed.clear();
            item.phase = Phase::Review(review);
        })?;
        self.read_call(None)
    }

    // The deep round's next reader.
    pub(super) fn deep_step(&mut self, deep: Deep) -> Result<Begin, StateError> {
        match deep {
            Deep::Read => self.read_call(None),
            Deep::Missed { first } => self.read_call(Some(first)),
        }
    }

    // A reader's call: the defect prompt over the whole diff, with the issue,
    // and for the second reader what the first found.
    fn read_call(&mut self, first: Option<Vec<Finding>>) -> Result<Begin, StateError> {
        let limit = self.agents.limits.deep_reviewer.clone();
        if let Some(held) = self.pace(Scope::Turn, &limit)?.holds() {
            return Ok(held);
        }
        let item = self.current().expect("the deep round is a work item's");
        let (issue, worktree, base) = (item.issue, item.worktree.clone(), item.review_base());
        let criteria = match self.criteria(issue) {
            Ok(criteria) => criteria,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let shots = match self.round_shots()? {
            RoundShots::Take(begin) => return Ok(*begin),
            RoundShots::Ready(shots) => shots,
        };
        let diff = match diff_against(&worktree, &base) {
            Ok(diff) => diff,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let mut prompt = prompts::reader_prompt(&base, &diff, &criteria, first.as_deref());
        if let Some(run) = &shots {
            prompt.push_str(&shots_prompt(run));
        }
        let model = (&self.agents.deep_reviewer, &limit);
        let call = build_call(Role::DeepReviewer, issue, &worktree, model, prompt);
        let call = call.map(|mut call| {
            call.settings = self.paths.worker.join(READ_SETTINGS_FILE);
            // It reads the worktree to check its work, and runs no command.
            call.tools = Tools::Review;
            if shots.is_some() {
                call.reach.read = vec![self.paths.shots(issue)];
            }
            call
        });
        match call.and_then(|call| self.prepared(call)) {
            Ok(call) => {
                self.mark_review_call_running(CallKind::Deep)?;
                Ok(Begin::Review(ReviewCall::Deep(call)))
            }
            Err(reason) => Ok(self.gate_failed(reason)),
        }
    }

    /// Records what a reader of the deep round said, and goes on to the next step
    pub(super) fn end_deep(
        &mut self,
        result: ReviewResult,
        spent: Option<Spent>,
    ) -> Result<Option<StepReport>, StateError> {
        let ReviewResult::Deep(reply) = result else {
            return Ok(None);
        };
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let Some(item) = self.current_in(&mut next) else {
            return Ok(None);
        };
        super::record_spent(item, spent, now);
        // What the call cost and that it ended are kept whatever became of its answer.
        let Phase::Review(review) = item.phase.clone() else {
            self.save(next)?;
            return Ok(None);
        };
        let ReviewStage::Deep(deep) = review.stage.clone() else {
            self.save(next)?;
            return Ok(None);
        };
        let issue = item.issue;
        let number = item
            .pull_request
            .expect("review starts once a pull request is known");
        let round = review.round;
        let failed = |what: &str, reason: String| StepReport::GateFailed {
            issue,
            reason: format!("the deep round's {what} on #{number} failed: {reason}"),
        };
        let read = |findings: usize, reader: u8| StepReport::DeepRead {
            issue,
            pull_request: number,
            round,
            reader,
            findings,
        };
        let report = match (deep, reply.and_then(|text| read_review(&text))) {
            (Deep::Read, Err(reason)) => failed("first reader", reason),
            (Deep::Missed { .. }, Err(reason)) => failed("second reader", reason),
            (Deep::Read, Ok(first)) => {
                let findings = first.len();
                item.phase = Phase::Review(Review {
                    stage: ReviewStage::Deep(Deep::Missed { first }),
                    ..review
                });
                read(findings, 1)
            }
            (Deep::Missed { first }, Ok(more)) => {
                let findings = more.len();
                let mut all = first;
                for f in more {
                    if !all.iter().any(|k| k.is_same_as(&f)) {
                        all.push(f);
                    }
                }
                // The worst first, and a stable sort keeps each reader's order.
                all.sort_by_key(|f| std::cmp::Reverse(f.severity));
                // From here the round's findings go as any reviewer's do.
                item.phase = Phase::Review(Review {
                    stage: ReviewStage::Found { findings: all },
                    ..review
                });
                read(findings, 2)
            }
        };
        self.save(next)?;
        Ok(Some(report))
    }
}
