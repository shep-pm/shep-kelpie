//! The review loop: rounds down the project's reviewers in order, each
//! judged finding by finding before the worker sees any
//!
//! A round with no raw findings is clean at once. One with findings waits
//! for the judge, in order; once every finding is judged, the held ones (if
//! any) go to the worker's next turn, and the round's cleanliness is decided
//! by the judge's severities. Two clean rounds in a row from two different
//! reviewers end the loop for [`super::gate`]; where only one reviewer can
//! run, one clean round does. Past the round guard the worker parks for a
//! ruling; a yes clears the guard for the rest of this work item.

pub(super) mod calls;
pub(super) mod criteria;
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
use super::shots::RoundShots;
use crate::pacer::Scope;
use crate::ports::{
    AgentCall, AgentError, AgentReply, Agents, Finding, Reviewer, ReviewerError, RoundStage,
    Severity, Timestamp, Verdict, read_review,
};
use crate::settings::{QWEN, ReviewerName, Runs};
use crate::state::{Fix, RulingKind, StateError};
use crate::work_item::{CallKind, Phase, Review, ReviewCallState, ReviewStage, Turn, WorkItem};
use crate::worktree;
use lineup::Chosen;

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
                let chosen = match self.choose_reviewer(&review, &worktree, &base) {
                    Ok(chosen) => chosen,
                    Err(reason) => return Ok(self.gate_failed(reason)),
                };
                if matches!(chosen.reviewer.runs, Runs::Deep) {
                    return self.deep_started(&chosen, review);
                }
                let criteria = match self.criteria(issue) {
                    Ok(criteria) => criteria,
                    Err(reason) => return Ok(self.gate_failed(reason)),
                };
                match chosen.reviewer.runs.clone() {
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
                        let shots = match self.round_shots()? {
                            RoundShots::Take(begin) => return Ok(*begin),
                            RoundShots::Ready(shots) => shots,
                        };
                        let dir = self.paths.shots(issue);
                        let shots = shots.as_ref().map(|run| calls::Screens { dir: &dir, run });
                        let call = calls::reviewer_call(
                            calls::Round {
                                issue,
                                worktree: &worktree,
                                base: &base,
                                worker_folder: &worker_folder,
                                criteria: &criteria,
                            },
                            (&session.model(), &session.limit),
                            shots,
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
            ReviewStage::Judging { findings, verdicts } => {
                if verdicts.len() == findings.len() {
                    return self.finalize_round(review, findings, verdicts);
                }
                let finding = findings[verdicts.len()].clone();
                let model = self.agents.judge.clone();
                let shots = self.preview_on().then(|| self.paths.shots(issue));
                match calls::judge_call(
                    issue,
                    &worktree,
                    &base,
                    &worker_folder,
                    (&model, &self.agents.limits.judge),
                    &finding,
                    shots.as_deref(),
                )
                .and_then(|call| self.prepared(call))
                {
                    Ok(call) => {
                        self.mark_review_call_running(CallKind::Judge)?;
                        Ok(Begin::Review(ReviewCall::Judge(call)))
                    }
                    Err(reason) => Ok(self.gate_failed(reason)),
                }
            }
            // begin_turn drives the fix turn itself, and comes here once it ends.
            ReviewStage::Fixing { clean, head } => {
                self.fix_ended(number, &build, review, clean, head)
            }
            ReviewStage::Deep(deep) => self.deep_step(review, deep),
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
        let local = self.counts_local(&review);
        self.update(|item| {
            // Files a local round left unreviewed keep the round from counting.
            let clean = item.counts_as_clean(clean);
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
    fn round_started(&mut self, chosen: &Chosen, kind: CallKind) -> Result<(), StateError> {
        let since = self.ports.clock.now();
        let name = chosen.reviewer.name.clone();
        let alone = chosen.alone;
        let local_possible = chosen.local_possible;
        self.update(|item| {
            item.call_started(kind, since);
            if !local_possible {
                // No local reviewer is left to review them, so the loop goes
                // on without.
                item.local_unreviewed.clear();
            }
            if let Phase::Review(review) = &mut item.phase {
                review.reviewer = Some(name);
                review.alone = alone;
            }
        })
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
        let local = self.counts_local(&review);
        // Files a local round left unreviewed keep the round from counting,
        // however the judge ruled on the rest.
        if held.is_empty() {
            let clean = item.counts_as_clean(true);
            self.update(|item| {
                item.phase = advance(review, clean, now, local, &mut item.local_rounds);
            })?;
            return Ok(Begin::Report(StepReport::ReviewFindingsSent {
                issue,
                pull_request: number,
                round,
                held: 0,
                clean,
            }));
        }
        let clean = item.counts_as_clean(held.iter().all(|f| f.severity <= Severity::Low));
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
            item.record_held(&held);
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
        let in_deep_round = self.current().is_some_and(
            |item| matches!(&item.phase, Phase::Review(r) if matches!(r.stage, ReviewStage::Deep(_))),
        );
        if in_deep_round {
            return self.end_deep(result, spent);
        }
        let now = self.ports.clock.now();
        let local = self
            .current()
            .and_then(|item| match &item.phase {
                Phase::Review(review) => Some(self.counts_local(review)),
                _ => None,
            })
            .unwrap_or(false);
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
            ReviewResult::Findings(Err(reason)) => StepReport::GateFailed { issue, reason },
            // Every file went unreviewed, so the round looked at nothing: it
            // is neither clean nor counted, and its reviewer gets one more try.
            ReviewResult::Findings(Ok(findings))
                if findings.is_empty() && !unreviewed.is_empty() =>
            {
                if !matches!(review.stage, ReviewStage::Round) {
                    unreachable!("a round's findings only arrive while awaiting that round");
                }
                *item.local_failures.entry(local_name.clone()).or_default() += 1;
                // The retry leaving any of these unreviewed again counts too.
                item.local_unreviewed.clone_from(&unreviewed);
                item.local_unreviewed_by = Some(local_name.clone());
                let retrying = !item.local_reviewer_down(&local_name);
                if !retrying && let Phase::Review(kept) = &mut item.phase {
                    // The next round chooses its reviewer afresh.
                    kept.reviewer = None;
                    kept.alone = false;
                }
                StepReport::LocalRoundFailed {
                    issue,
                    pull_request: number,
                    round,
                    reviewer: local_name,
                    unreviewed,
                    retrying,
                }
            }
            // Nothing to judge: the round is clean at once.
            ReviewResult::Findings(Ok(findings)) if findings.is_empty() => {
                if !matches!(review.stage, ReviewStage::Round) {
                    unreachable!("a round's findings only arrive while awaiting that round");
                }
                if local_round {
                    item.note_local_round(&local_name, &unreviewed);
                }
                // A Claude round after a local one that left files unreviewed
                // gets no credit either.
                let clean = item.counts_as_clean(true);
                item.phase = advance(review, clean, now, local, &mut item.local_rounds);
                StepReport::ReviewFindingsSent {
                    issue,
                    pull_request: number,
                    round,
                    held: 0,
                    clean,
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
                let count = findings.len();
                if local_round {
                    // Kept through judging and fixing: until a later local
                    // round reviews these files, nothing counts as clean.
                    item.note_local_round(&local_name, &unreviewed);
                }
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
                    unreviewed,
                }
            }
            ReviewResult::Deep(_) => unreachable!("a deep session's reply comes in the deep round"),
            ReviewResult::Verdict(Err(reason)) => StepReport::GateFailed { issue, reason },
            ReviewResult::Stopped => unreachable!("a stopped call returns above"),
            ReviewResult::Spilled(_) => unreachable!("a spilled model returns above"),
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
/// A clean round ends the loop when its reviewer was the only one that
/// could run, or when the round before was clean and someone else's. An
/// older state file names no reviewers, and its rounds strictly alternated.
/// `ran` counts the work item's local rounds, this one included when
/// `local` says it counts.
pub(super) fn advance(
    review: Review,
    clean: bool,
    now: crate::ports::Timestamp,
    local: bool,
    ran: &mut u32,
) -> Phase {
    if local {
        *ran = ran.saturating_add(1);
    }
    let someone_else = review.last.is_none() || review.last != review.reviewer;
    let ends = clean && (review.alone || (review.consecutive_clean > 0 && someone_else));
    if ends {
        return Phase::Ci {
            head: None,
            since: now,
        };
    }
    Phase::Review(Review {
        round: review.round + 1,
        consecutive_clean: if clean {
            review.consecutive_clean + 1
        } else {
            0
        },
        guard_cleared: review.guard_cleared,
        stage: ReviewStage::Round,
        reviewer: None,
        last: review.reviewer,
        alone: false,
    })
}

/// Runs `action` outside the runner's lock: the local round, or a fresh
/// Claude call for a review round or the judge
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
        ReviewCall::Judge(call) => {
            let (reply, spent) = run_claude(claude, &call);
            match reply {
                Err(AgentError::Stopped) => stopped(),
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
