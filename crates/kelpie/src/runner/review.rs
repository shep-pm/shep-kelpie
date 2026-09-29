//! The review loop: rounds alternating the local round and Claude, or
//! Claude's alone, each judged finding by finding before the worker sees any
//!
//! A round with no raw findings is clean at once. One with findings waits
//! for the judge, in order; once every finding is judged, the held ones (if
//! any) go to the worker's next turn, and the round's cleanliness is decided
//! by the judge's severities. Two clean rounds in a row, one from each
//! reviewer since they strictly alternate, end the loop for [`super::gate`];
//! with the local round off, one clean Claude round does. Past the round
//! guard the worker parks for a ruling; a yes clears the guard for the rest
//! of this work item.

pub(super) mod calls;
pub(super) mod findings;
#[cfg(test)]
mod local;

use std::path::Path;

use super::Runner;
use super::report::{Begin, ReviewCall, ReviewResult, Reviewed, Spent, StepReport};
use super::shots::RoundShots;
use crate::ports::{
    Claude, ClaudeCall, ClaudeError, ClaudeReply, Finding, Reviewer, ReviewerError, Severity,
    Timestamp, Verdict, read_review,
};
use crate::state::{Fix, RulingKind, StateError};
use crate::work_item::{Phase, Review, ReviewCallState, ReviewStage, ReviewerKind, Turn, WorkItem};
use crate::worktree;

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

        match review.stage.clone() {
            ReviewStage::Round => {
                if !review.guard_cleared && review.round > self.settings.review.loop_guard.get() {
                    return self.raise(number, RulingKind::ReviewGuard { review });
                }
                match review.reviewer(self.local_left()) {
                    ReviewerKind::Local => {
                        self.mark_review_call_running()?;
                        Ok(Begin::Review(ReviewCall::Local {
                            local: self.settings.review.local.clone(),
                            worktree,
                            base,
                            out: build.join("qwen-review"),
                            round: review.round,
                        }))
                    }
                    ReviewerKind::Claude => {
                        let shots = match self.round_shots()? {
                            RoundShots::Take(begin) => return Ok(begin),
                            RoundShots::Ready(shots) => shots,
                        };
                        let model = self.settings.models.reviewer.clone();
                        let dir = self.paths.shots(issue);
                        let shots = shots.as_ref().map(|run| calls::Screens { dir: &dir, run });
                        match calls::reviewer_call(
                            issue,
                            &worktree,
                            &base,
                            &worker_folder,
                            &model,
                            shots,
                            &self.skills,
                        ) {
                            Ok(call) => {
                                self.mark_review_call_running()?;
                                Ok(Begin::Review(ReviewCall::ClaudeRound(call)))
                            }
                            Err(reason) => Ok(self.gate_failed(reason)),
                        }
                    }
                }
            }
            ReviewStage::Judging { findings, verdicts } => {
                if verdicts.len() == findings.len() {
                    return self.finalize_round(review, findings, verdicts);
                }
                let finding = findings[verdicts.len()].clone();
                let model = self.settings.models.judge.clone();
                let shots = self.preview_on().then(|| self.paths.shots(issue));
                match calls::judge_call(
                    issue,
                    &worktree,
                    &base,
                    &worker_folder,
                    &model,
                    &finding,
                    shots.as_deref(),
                ) {
                    Ok(call) => {
                        self.mark_review_call_running()?;
                        Ok(Begin::Review(ReviewCall::Judge(call)))
                    }
                    Err(reason) => Ok(self.gate_failed(reason)),
                }
            }
            // begin_turn drives the fix turn itself, and comes here once it ends.
            ReviewStage::Fixing { clean, head } => {
                self.fix_ended(number, &build, review, clean, head)
            }
        }
    }

    // A fix turn that pushed nothing fixed nothing, whatever it says: the
    // held findings still stand, so the round cannot count.
    fn fix_ended(
        &mut self,
        number: u64,
        build: &Path,
        review: Review,
        clean: bool,
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
        let now = self.ports.clock.now();
        let local = self.local_rounds();
        self.update(|item| {
            item.phase = advance(review, clean, now, local, &mut item.local_rounds)
        })?;
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

    // How many local rounds a work item may run, which sets who reviews a
    // round and how many clean rounds end the loop.
    fn local_rounds(&self) -> u32 {
        let review = &self.settings.review;
        match review.local_rounds {
            _ if !review.local.is_on() => 0,
            Some(rounds) => rounds.get(),
            None => u32::MAX,
        }
    }

    // How many of them this work item has not run yet.
    fn local_left(&self) -> u32 {
        let ran = self.current().map_or(0, |item| item.local_rounds);
        self.local_rounds().saturating_sub(ran)
    }

    // Recorded in state before the runner's lock is released for the call
    // itself, the same as a worker's turn marks `Turn::Running`: `drop`
    // refuses while this is set, so a call in flight always has a work
    // item to land its result on.
    pub(super) fn mark_review_call_running(&mut self) -> Result<(), StateError> {
        let since = self.ports.clock.now();
        self.update(|item| item.review_call = ReviewCallState::Running { since })
    }

    // Every held finding holds the judge's own severity, since the nit rule
    // and the worker's fix both go by what the judge decided, not the
    // reviewer's original claim.
    fn finalize_round(
        &mut self,
        review: Review,
        findings: Vec<Finding>,
        verdicts: Vec<Verdict>,
    ) -> Result<Begin, StateError> {
        let item = self.current().expect("finalize runs on a work item");
        let issue = item.issue;
        let number = item
            .pull_request
            .expect("review runs once a pull request is known");
        let round = review.round;
        let now = self.ports.clock.now();
        let held: Vec<Finding> = findings
            .into_iter()
            .zip(verdicts)
            .filter(|(_, v)| v.holds)
            .map(|(f, v)| Finding {
                severity: v.severity,
                ..f
            })
            .collect();
        let local = self.local_rounds();
        if held.is_empty() {
            self.update(|item| {
                item.phase = advance(review, true, now, local, &mut item.local_rounds);
            })?;
            return Ok(Begin::Report(StepReport::ReviewFindingsSent {
                issue,
                pull_request: number,
                round,
                held: 0,
                clean: true,
            }));
        }
        let clean = held.iter().all(|f| f.severity <= Severity::Low);
        let head = match self.origin_head() {
            Ok(head) => head,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let build = &item.build;
        let path = findings::findings_path(build);
        if let Err(reason) = findings::write_findings_file(build, &path, round, &held) {
            return Ok(self.gate_failed(reason));
        }
        let held_count = held.len();
        let prompt = findings::fix_prompt(number, round, held_count, &path);
        self.update(|item| {
            item.turn = Turn::Next { prompt };
            item.phase = Phase::Review(Review {
                stage: ReviewStage::Fixing {
                    clean,
                    head: Some(head),
                },
                ..review
            });
        })?;
        Ok(Begin::Report(StepReport::ReviewFindingsSent {
            issue,
            pull_request: number,
            round,
            held: held_count,
            clean,
        }))
    }

    pub(super) fn end_review(
        &mut self,
        reviewed: Reviewed,
    ) -> Result<Option<StepReport>, StateError> {
        let Reviewed { result, spent } = reviewed;
        // A call stopped with the runner saves nothing, the same as a turn
        // in `end_turn`: its round or judge call is still due, and runs
        // again once a restart clears `review_call`.
        if matches!(result, ReviewResult::Stopped) {
            return Ok(None);
        }
        let in_review_bot_round = self
            .current()
            .is_some_and(|item| matches!(item.phase, Phase::CodeRabbit(_)));
        if in_review_bot_round {
            return self.review_bot_verdict(result, spent);
        }
        let now = self.ports.clock.now();
        let local = self.local_rounds();
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

        let report = match result {
            ReviewResult::Findings(Err(reason)) => StepReport::GateFailed { issue, reason },
            // Nothing to judge: the round is clean at once.
            ReviewResult::Findings(Ok(findings)) if findings.is_empty() => {
                if !matches!(review.stage, ReviewStage::Round) {
                    unreachable!("a round's findings only arrive while awaiting that round");
                }
                item.phase = advance(review, true, now, local, &mut item.local_rounds);
                StepReport::ReviewFindingsSent {
                    issue,
                    pull_request: number,
                    round,
                    held: 0,
                    clean: true,
                }
            }
            ReviewResult::Findings(Ok(findings)) => {
                if !matches!(review.stage, ReviewStage::Round) {
                    unreachable!("a round's findings only arrive while awaiting that round");
                }
                let reviewer = review.reviewer(local.saturating_sub(item.local_rounds));
                let count = findings.len();
                item.phase = Phase::Review(Review {
                    stage: ReviewStage::Judging {
                        findings,
                        verdicts: Vec::new(),
                    },
                    ..review
                });
                StepReport::ReviewRound {
                    issue,
                    pull_request: number,
                    round,
                    reviewer,
                    findings: count,
                }
            }
            ReviewResult::Verdict(Err(reason)) => StepReport::GateFailed { issue, reason },
            ReviewResult::Stopped => unreachable!("a stopped call returns above"),
            ReviewResult::Verdict(Ok(verdict)) => {
                let ReviewStage::Judging {
                    findings,
                    mut verdicts,
                } = review.stage.clone()
                else {
                    unreachable!("a verdict only arrives while judging");
                };
                let (holds, severity) = (verdict.holds, verdict.severity);
                verdicts.push(verdict);
                item.phase = Phase::Review(Review {
                    stage: ReviewStage::Judging { findings, verdicts },
                    ..review
                });
                StepReport::FindingJudged {
                    issue,
                    round,
                    holds,
                    severity,
                }
            }
        };
        self.save(next)?;
        Ok(Some(report))
    }
}

/// Where the review phase goes after one round finishes, clean or not
///
/// While local rounds are left, two clean rounds in a row end the loop, and
/// the strict alternation means the pair is always one of each. Without
/// one, or once the work item's `local_rounds` are spent, every round is
/// Claude's, so one clean Claude round ends it. `ran` counts the work
/// item's local rounds, across every pass of the loop, this one included.
pub(super) fn advance(
    review: Review,
    clean: bool,
    now: crate::ports::Timestamp,
    local_rounds: u32,
    ran: &mut u32,
) -> Phase {
    let consecutive_clean = if clean {
        review.consecutive_clean + 1
    } else {
        0
    };
    let local = review.reviewer(local_rounds.saturating_sub(*ran)) == ReviewerKind::Local;
    if local {
        *ran = ran.saturating_add(1);
    }
    let alone = !local && *ran >= local_rounds;
    let needed = if alone { 1 } else { 2 };
    if consecutive_clean >= needed {
        Phase::Ci {
            head: None,
            since: now,
        }
    } else {
        Phase::Review(Review {
            round: review.round + 1,
            consecutive_clean,
            guard_cleared: review.guard_cleared,
            stage: ReviewStage::Round,
        })
    }
}

/// Runs `action` outside the runner's lock: the local round, or a fresh
/// Claude call for a review round or the judge
///
/// A call stopped with the runner comes back as [`ReviewResult::Stopped`]
/// rather than an error, so `end_review` can tell it from a failed gate.
pub(super) fn run_review_call(
    claude: &dyn Claude,
    reviewer: &dyn Reviewer,
    action: ReviewCall,
) -> Reviewed {
    match action {
        ReviewCall::Local {
            local,
            worktree,
            base,
            out,
            round,
        } => match reviewer.round(&local, &worktree, &base, &out, round) {
            Err(ReviewerError::Stopped) => stopped(),
            result => Reviewed {
                result: ReviewResult::Findings(result.map_err(|e| e.to_string())),
                spent: Some(Spent::Local),
            },
        },
        ReviewCall::ClaudeRound(call) => {
            let (reply, spent) = run_claude(claude, &call);
            match reply {
                Err(ClaudeError::Stopped) => stopped(),
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
        ReviewCall::Judge(call) => {
            let (reply, spent) = run_claude(claude, &call);
            match reply {
                Err(ClaudeError::Stopped) => stopped(),
                reply => Reviewed {
                    result: ReviewResult::Verdict(reply.map_err(|e| e.to_string()).and_then(
                        |reply| {
                            calls::parse_verdict(&reply.text).ok_or_else(|| {
                                format!("unreadable judge output: {}", reply.text.trim())
                            })
                        },
                    )),
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
    item.review_call = ReviewCallState::Idle;
}

fn stopped() -> Reviewed {
    Reviewed {
        result: ReviewResult::Stopped,
        spent: None,
    }
}

// A reply that came back cost something even if what it said is unusable.
fn run_claude(
    claude: &dyn Claude,
    call: &ClaudeCall,
) -> (Result<ClaudeReply, ClaudeError>, Option<Spent>) {
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
