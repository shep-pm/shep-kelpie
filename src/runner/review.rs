//! The review: one pass down the project's reviewers, each run once, in order
//!
//! A round whose findings are all nits (LOW), or none, goes straight to the
//! next reviewer. One with anything above a nit sends the worker all of its
//! findings, at the reviewer's own severity, for one fix turn, and the next
//! reviewer reads the fix. After the last reviewer the work item goes on to
//! [`super::gate`].

pub(super) mod calls;
mod criteria;
mod deep;
pub(super) mod findings;
mod lineup;
#[cfg(test)]
mod local;
#[cfg(test)]
mod several;

use std::path::Path;

use super::Runner;
use super::report::{Begin, ReviewCall, ReviewResult, Reviewed, Spent, StepReport};
use super::ruling::park;
use crate::pacer::Scope;
use crate::ports::{
    AgentCall, AgentError, AgentReply, Agents, Finding, Reviewer, ReviewerError, RoundStage,
    Severity, Timestamp, read_review,
};
use crate::settings::{LoopReviewer, QWEN, ReviewerName, Runs};
use crate::state::{Fix, RulingKind, StateError};
use crate::work_item::{CallKind, Phase, Review, ReviewCallState, ReviewStage, Turn, WorkItem};
use crate::worktree;

// A reviewer whose call fails this many times in a row is passed over for
// the rest of its pass: a reply it cannot give would otherwise stall the
// review for good.
const ROUND_FAILURES: u32 = 3;

impl Runner {
    pub(super) fn review_step(&mut self) -> Result<Begin, StateError> {
        let item = self.current().expect("review runs on a work item");
        let Phase::Review(review) = item.phase.clone() else {
            unreachable!("review_step only runs in the review phase")
        };
        let number = item
            .pull_request
            .expect("review starts once a pull request is known");
        let (issue, worktree) = (item.issue, item.worktree.clone());
        let base = item.review_base();
        let build = item.build.clone();
        let worker_folder = self.paths.worker.clone();
        // A new pass reviews new code, so no fix of the last one will
        // resolve the bot threads it sent.
        let fresh = review == Review::first();
        if fresh && !item.threads_sent.is_empty() {
            self.update(WorkItem::forget_threads)?;
        }

        match review.stage.clone() {
            ReviewStage::Round => {
                let chosen = match self.choose_reviewer(&review, &worktree, &base) {
                    Ok(Some(chosen)) => chosen,
                    Ok(None) => return self.pass_ended(),
                    Err(reason) => return Ok(self.gate_failed(reason)),
                };
                if matches!(chosen.runs, Runs::Deep) {
                    return self.deep_started(&chosen, review);
                }
                let criteria = match self.criteria(issue) {
                    Ok(criteria) => criteria,
                    Err(reason) => return Ok(self.gate_failed(reason)),
                };
                match chosen.runs.clone() {
                    Runs::Local(local) => {
                        self.round_started(&chosen, CallKind::Local)?;
                        Ok(Begin::Review(ReviewCall::Local {
                            local,
                            worktree,
                            base,
                            out: build.join("qwen-review"),
                            round: review.round,
                            criteria,
                        }))
                    }
                    Runs::Deep => unreachable!("the deep round returned above"),
                    Runs::Claude(session) => {
                        if let Some(held) = self.pace(Scope::Turn, &session.limit)?.holds() {
                            return Ok(held);
                        }
                        let call = calls::reviewer_call(
                            calls::Round {
                                issue,
                                worktree: &worktree,
                                base: &base,
                                worker_folder: &worker_folder,
                                criteria: &criteria,
                            },
                            (&session.model(), &session.limit),
                            &self.skills,
                        );
                        match call.and_then(|call| self.prepared(call)) {
                            Ok(call) => {
                                self.round_started(&chosen, CallKind::Claude)?;
                                Ok(Begin::Review(ReviewCall::ClaudeRound(call)))
                            }
                            Err(reason) => Ok(self.gate_failed(reason)),
                        }
                    }
                }
            }
            ReviewStage::Found { findings } => self.send_findings(review, findings),
            // begin_turn drives the fix turn itself, and comes here once it ends.
            ReviewStage::Fixing { head } => self.fix_ended(number, &build, review, head),
            ReviewStage::Deep(deep) => self.deep_step(deep),
        }
    }

    // The last reviewer is done, so the work item goes on to CI in this step.
    fn pass_ended(&mut self) -> Result<Begin, StateError> {
        let since = self.ports.clock.now();
        self.update(|item| item.phase = Phase::Ci { head: None, since })?;
        self.check_ci()
    }

    // A fix turn that pushed nothing fixed nothing, whatever it says: the
    // findings sent still stand, so the next reviewer does not run yet.
    fn fix_ended(
        &mut self,
        number: u64,
        build: &Path,
        review: Review,
        head: Option<String>,
    ) -> Result<Begin, StateError> {
        let issue = self.current().expect("a fix is a work item's").issue;
        let round = review.round;
        let pushed = match head {
            Some(before) => match self.origin_head() {
                Ok(now) if now == before => {
                    let path = findings::findings_path(build);
                    let prompt = findings::again_prompt(number, round, &path);
                    let fix = Fix::Review(review);
                    return self.raise(number, RulingKind::FixNotPushed { fix, prompt });
                }
                Ok(now) => Some(now),
                Err(reason) => return Ok(self.gate_failed(reason)),
            },
            None => None,
        };
        let next = self.after_round(review, self.ports.clock.now());
        self.update(|item| item.phase = next)?;
        Ok(Begin::Report(StepReport::FixPushed {
            issue,
            pull_request: number,
            round,
            head: pushed,
        }))
    }

    // Asks git rather than the forge: the forge's head lags a push by a moment.
    pub(super) fn origin_head(&self) -> Result<String, String> {
        let item = self.current().expect("a head is a work item's");
        worktree::origin_head(&self.settings.repo, &item.branch).map_err(|e| e.to_string())
    }

    // Recorded in state before the runner's lock is released for the call
    // itself, the same as a worker's turn marks `Turn::Running`: `drop`
    // refuses while this is set, so a call in flight always has a work
    // item to land its result on.
    pub(super) fn mark_review_call_running(&mut self, kind: CallKind) -> Result<(), StateError> {
        let since = self.ports.clock.now();
        self.update(|item| item.call_started(kind, since))
    }

    // Marks the call running and keeps who reviews the round, so a round
    // cut short resumes with the same reviewer.
    fn round_started(&mut self, chosen: &LoopReviewer, kind: CallKind) -> Result<(), StateError> {
        let since = self.ports.clock.now();
        let name = chosen.name.clone();
        self.update(|item| {
            item.call_started(kind, since);
            if let Phase::Review(review) = &mut item.phase {
                review.reviewer = Some(name);
            }
        })
    }

    // Nits alone are not worth a fix turn. Otherwise every finding goes, at
    // the reviewer's own severity, nits included.
    fn send_findings(
        &mut self,
        review: Review,
        findings: Vec<Finding>,
    ) -> Result<Begin, StateError> {
        let item = self.current().expect("findings are a work item's");
        let issue = item.issue;
        let number = item
            .pull_request
            .expect("review runs once a pull request is known");
        let round = review.round;
        if findings.iter().all(|f| f.severity <= Severity::Low) {
            let next = self.after_round(review, self.ports.clock.now());
            self.update(|item| item.phase = next)?;
            return Ok(Begin::Report(StepReport::ReviewFindingsSent {
                issue,
                pull_request: number,
                round,
                held: 0,
            }));
        }
        let head = match self.origin_head() {
            Ok(head) => head,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let build = &item.build;
        let path = findings::findings_path(build);
        if let Err(reason) = findings::write_findings_file(build, &path, round, &findings) {
            return Ok(self.gate_failed(reason));
        }
        let held = findings.len();
        let prompt = findings::fix_prompt(number, round, held, &path);
        self.update(|item| {
            item.record_held(&findings);
            item.turn = Turn::Next { prompt };
            item.phase = Phase::Review(Review {
                stage: ReviewStage::Fixing { head: Some(head) },
                ..review
            });
        })?;
        Ok(Begin::Report(StepReport::ReviewFindingsSent {
            issue,
            pull_request: number,
            round,
            held,
        }))
    }

    pub(super) fn end_review(
        &mut self,
        reviewed: Reviewed,
    ) -> Result<Option<StepReport>, StateError> {
        let Reviewed { result, spent } = reviewed;
        // A call stopped with the runner saves nothing, the same as a turn
        // in `end_turn`: its round is still due, and runs again once a
        // restart clears `review_call`.
        if matches!(result, ReviewResult::Stopped) {
            return Ok(None);
        }
        let in_deep_round = self.current().is_some_and(
            |item| matches!(&item.phase, Phase::Review(r) if matches!(r.stage, ReviewStage::Deep(_))),
        );
        if in_deep_round {
            return self.end_deep(result, spent);
        }
        let now = self.ports.clock.now();
        let local_round = self.current().is_some_and(
            |item| matches!(&item.phase, Phase::Review(review) if self.is_local_round(review)),
        );
        let mut next = self.state.clone();
        // Tolerated the same way `end_turn` tolerates a turn's result
        // arriving with nothing (or something else) to apply it to: the
        // guard on `drop` keeps this from happening today, but a result
        // for a work item that is no longer this one, or gone outright,
        // is silently discarded rather than panicking the runner.
        let Some(item) = self.current_in(&mut next) else {
            return Ok(None);
        };
        record_spent(item, spent, now);
        let Phase::Review(review) = item.phase.clone() else {
            return Ok(None);
        };
        let issue = item.issue;
        let number = item
            .pull_request
            .expect("review runs once a pull request is known");
        let round = review.round;

        // Not a failed gate to wait out: the maintainer moves the model, so
        // the round is parked on a ruling, which alerts.
        if let ReviewResult::Spilled(reason) = &result {
            let reason = reason.clone();
            let kind = RulingKind::LocalModelSpilled { review, reason };
            let (id, question) = park(self.names(), &mut next, issue, Some(number), kind);
            self.save(next)?;
            let comment_failed = self.post_ruling(Some(number), id);
            return Ok(Some(StepReport::Ruling {
                issue,
                pull_request: number,
                id,
                question,
                comment_failed,
            }));
        }

        // The script writes a line for a file it could not review, and still
        // finishes the round: those lines are not findings.
        let (result, unreviewed) = match result {
            ReviewResult::Findings(Ok(findings)) if local_round => {
                let (unreviewed, findings) = findings.into_iter().partition(Finding::is_unreviewed);
                (ReviewResult::Findings(Ok(findings)), unreviewed)
            }
            result => (result, Vec::new()),
        };
        // An older state file names no reviewer for its local round.
        let local_name = review.reviewer.clone().unwrap_or_else(|| {
            ReviewerName::try_from(QWEN.to_owned()).expect("the local round's name is valid")
        });
        let unreviewed: Vec<String> = unreviewed.into_iter().map(|f: Finding| f.file).collect();

        let report = match result {
            // The round stays due; one that keeps failing goes no further.
            ReviewResult::Findings(Err(reason)) if review.failures + 1 < ROUND_FAILURES => {
                let failures = review.failures + 1;
                item.phase = Phase::Review(Review { failures, ..review });
                StepReport::GateFailed { issue, reason }
            }
            ReviewResult::Findings(Err(reason)) => {
                let reviewer = review.reviewer.clone().unwrap_or_else(ReviewerName::claude);
                if !item.reviewers_skipped.contains(&reviewer) {
                    item.reviewers_skipped.push(reviewer.clone());
                }
                item.phase = self.after_round(review, now);
                StepReport::ReviewerSkipped {
                    issue,
                    pull_request: number,
                    round,
                    reviewer,
                    reason,
                }
            }
            // Every file went unreviewed, so the round looked at nothing, and
            // the pass goes on to the next reviewer.
            ReviewResult::Findings(Ok(findings))
                if findings.is_empty() && !unreviewed.is_empty() =>
            {
                if !matches!(review.stage, ReviewStage::Round) {
                    unreachable!("a round's findings only arrive while awaiting that round");
                }
                *item.local_failures.entry(local_name.clone()).or_default() += 1;
                // The next round leaving any of these unreviewed again counts too.
                item.local_unreviewed.clone_from(&unreviewed);
                item.local_unreviewed_by = Some(local_name.clone());
                item.phase = self.after_round(review, now);
                StepReport::LocalRoundFailed {
                    issue,
                    pull_request: number,
                    round,
                    reviewer: local_name,
                    unreviewed,
                }
            }
            // Nothing found: the pass goes on to the next reviewer at once.
            ReviewResult::Findings(Ok(findings)) if findings.is_empty() => {
                if !matches!(review.stage, ReviewStage::Round) {
                    unreachable!("a round's findings only arrive while awaiting that round");
                }
                if local_round {
                    item.note_local_round(&local_name, &unreviewed);
                }
                item.phase = self.after_round(review, now);
                StepReport::ReviewFindingsSent {
                    issue,
                    pull_request: number,
                    round,
                    held: 0,
                }
            }
            ReviewResult::Findings(Ok(findings)) => {
                if !matches!(review.stage, ReviewStage::Round) {
                    unreachable!("a round's findings only arrive while awaiting that round");
                }
                // The same name `note_local_round` keeps the failures under.
                let reviewer = if local_round {
                    local_name.clone()
                } else {
                    review.reviewer.clone().unwrap_or_else(ReviewerName::claude)
                };
                if local_round {
                    item.note_local_round(&local_name, &unreviewed);
                }
                let count = findings.len();
                item.phase = Phase::Review(Review {
                    stage: ReviewStage::Found { findings },
                    ..review
                });
                StepReport::ReviewRound {
                    issue,
                    pull_request: number,
                    round,
                    reviewer,
                    findings: count,
                    unreviewed,
                }
            }
            ReviewResult::Deep(_) => unreachable!("a deep session's reply comes in the deep round"),
            ReviewResult::Stopped => unreachable!("a stopped call returns above"),
            ReviewResult::Spilled(_) => unreachable!("a spilled model returns above"),
        };
        self.save(next)?;
        Ok(Some(report))
    }
}

/// Runs `action` outside the runner's lock: the local round, or a fresh
/// Claude call for a review round or a deep round's session
///
/// A call stopped with the runner comes back as [`ReviewResult::Stopped`]
/// rather than an error, so `end_review` can tell it from a failed gate.
pub(super) fn run_review_call(
    claude: &dyn Agents,
    reviewer: &dyn Reviewer,
    action: ReviewCall,
    watch: &(dyn Fn(RoundStage) + Sync),
) -> Reviewed {
    match action {
        ReviewCall::Local {
            local,
            worktree,
            base,
            out,
            round,
            criteria,
        } => {
            match reviewer.round_watched(&local, &worktree, &base, &out, round, &criteria, watch) {
                Err(ReviewerError::Stopped) => stopped(),
                Err(ReviewerError::Spilled(reason)) => Reviewed {
                    result: ReviewResult::Spilled(reason),
                    spent: None,
                },
                result => Reviewed {
                    result: ReviewResult::Findings(result.map_err(|e| e.to_string())),
                    spent: Some(Spent::Local),
                },
            }
        }
        ReviewCall::ClaudeRound(call) => {
            let (reply, spent) = run_claude(claude, &call);
            match reply {
                Err(AgentError::Stopped) => stopped(),
                reply => Reviewed {
                    result: ReviewResult::Findings(reply.map_err(|e| e.to_string()).and_then(
                        |reply| {
                            read_review(&reply.text).map_err(|text| {
                                format!(
                                    "the Claude round's reply is neither findings nor CLEAN: {text}"
                                )
                            })
                        },
                    )),
                    spent,
                },
            }
        }
        ReviewCall::Deep(call) => {
            let (reply, spent) = run_claude(claude, &call);
            match reply {
                Err(AgentError::Stopped) => stopped(),
                reply => Reviewed {
                    result: ReviewResult::Deep(
                        reply.map(|reply| reply.text).map_err(|e| e.to_string()),
                    ),
                    spent,
                },
            }
        }
    }
}

/// Adds what a review call spent to the work item's record, and marks no
/// call in flight
///
/// A local round's time runs from when the call was marked running.
pub(super) fn record_spent(item: &mut WorkItem, spent: Option<Spent>, now: Timestamp) {
    match spent {
        Some(Spent::Claude {
            role,
            session,
            usage,
            session_cost,
        }) => {
            item.record_call(role, now, session, usage, session_cost);
        }
        Some(Spent::Local) => {
            item.qwen.rounds += 1;
            if let ReviewCallState::Running { since } = item.review_call {
                item.qwen.seconds += now.0.saturating_sub(since.0);
            }
        }
        None => {}
    }
    item.call_ended();
}

fn stopped() -> Reviewed {
    Reviewed {
        result: ReviewResult::Stopped,
        spent: None,
    }
}

// A reply that came back cost something even if what it said is unusable.
fn run_claude(
    claude: &dyn Agents,
    call: &AgentCall,
) -> (Result<AgentReply, AgentError>, Option<Spent>) {
    let reply = claude.run(call);
    let spent = reply.as_ref().ok().map(|reply| Spent::Claude {
        role: call.role,
        session: call.session.id().clone(),
        usage: reply.usage,
        session_cost: reply.session_cost,
    });
    (reply, spent)
}

#[cfg(test)]
mod tests;
