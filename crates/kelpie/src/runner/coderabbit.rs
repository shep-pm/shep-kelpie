//! CodeRabbit rounds, between green CI and the merge ruling
//!
//! A round takes the CodeRabbit lease, puts the `review please` label on,
//! and gives the lease back once CodeRabbit answers. A refusal reschedules
//! the dog's window and the round asks again. Once a review covers the
//! head the label comes off, the judge reads every open thread, rejected
//! ones are resolved and held ones go to the worker. Satisfied means no
//! thread open and nothing held. Past the cap, held findings park the worker.

mod cap;
#[cfg(test)]
mod tests;

use super::Runner;
use super::report::{Begin, ReviewCall, ReviewResult, StepReport};
use super::review::{calls, findings};
use crate::coderabbit::{self, Activity, Reading};
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{Finding, PullRequestState, Timestamp, Verdict};
use crate::state::{LeaseHeld, Resource, RulingKind, StateError};
use crate::work_item::{CodeRabbitStage, OpenThread, Phase, ReviewCallState, Turn, WorkItem};

/// The label shep's `.coderabbit.yaml` gates auto review on: the summon
pub const LABEL: &str = "review please";

// A summon neither taken up nor refused in ten minutes is counted as spent,
// so the lease goes back.
const ANSWER_WAIT: u64 = 600;

// A full review of a long branch took 24 minutes on shep. Two hours with
// none is the maintainer's to look at.
const REVIEW_WAIT: u64 = 2 * 3600;

impl Runner {
    /// Whether the work item owes CodeRabbit a round before its merge ruling
    pub(super) fn coderabbit_due(&self) -> bool {
        self.settings.coderabbit.enabled && !self.item().coderabbit.satisfied
    }

    /// Starts a round on `head`, which CI has just passed
    pub(super) fn start_round(&mut self, head: String) -> Result<Begin, StateError> {
        self.update(|item| item.phase = Phase::CodeRabbit(CodeRabbitStage::Lease { head }))?;
        self.coderabbit_step()
    }

    pub(super) fn coderabbit_step(&mut self) -> Result<Begin, StateError> {
        let Phase::CodeRabbit(stage) = self.item().phase.clone() else {
            unreachable!("coderabbit_step only runs in a CodeRabbit round")
        };
        let number = self.number();
        let pr = self.ports.forge.pull_request(&self.settings.forge, number);
        match pr.map(|pr| pr.state) {
            Ok(PullRequestState::Open) => {}
            // The maintainer merged it by hand, which is their own ruling.
            Ok(PullRequestState::Merged) => {
                self.update(|item| item.phase = Phase::Done { merged: true })?;
                return self.finish(true);
            }
            Ok(PullRequestState::Closed) => {
                self.leave_round();
                return self.raise(number, RulingKind::Closed);
            }
            Err(e) => return Ok(self.gate_failed(format!("cannot read #{number}: {e}"))),
        }
        match stage {
            CodeRabbitStage::Lease { head } => self.summon(head),
            CodeRabbitStage::Summoned { head, at } => self.await_review(head, at),
            CodeRabbitStage::Judging {
                threads, verdicts, ..
            } => self.judge_threads(&threads, &verdicts),
        }
    }

    // No summon without the lease, and none for a head already reviewed:
    // that one costs the hour and buys nothing.
    fn summon(&mut self, head: String) -> Result<Begin, StateError> {
        let number = self.number();
        let activity = match self.activity(number) {
            Ok(activity) => activity,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        if activity.covers(&head) {
            return self.review_landed(number, &activity);
        }
        let kind = LeaseKind::coderabbit();
        self.ports.leases.want(&kind);
        if !self.ports.leases.holds(&kind) {
            return Ok(Begin::Idle);
        }
        let now = self.ports.clock.now();
        self.hold(now)?;
        // A label kelpie put on is a summon made before a restart could save
        // it. Any other label on sends no event, so it comes off first.
        let ours = self.item().known.labels.iter().any(|l| l == LABEL);
        let summoned = match self.labelled(number) {
            Ok(on) => ours && on,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        if !summoned
            && let Err(reason) = self
                .label(number, false)
                .and_then(|()| self.label(number, true))
        {
            return Ok(self.gate_failed(reason));
        }
        let stage = CodeRabbitStage::Summoned {
            head: head.clone(),
            at: now,
        };
        self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
        Ok(Begin::Report(StepReport::Summoned {
            issue: self.item().issue,
            pull_request: number,
            head,
        }))
    }

    fn await_review(&mut self, head: String, at: Timestamp) -> Result<Begin, StateError> {
        let number = self.number();
        let activity = match self.activity(number) {
            Ok(activity) => activity,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let waited = self.ports.clock.now().0.saturating_sub(at.0);
        match activity.read(&head, at) {
            Reading::Reviewed => {
                self.accepted(at)?;
                if let Err(reason) = self.label(number, false) {
                    return Ok(self.gate_failed(reason));
                }
                self.review_landed(number, &activity)
            }
            Reading::Refused { opens } => {
                let kind = LeaseKind::coderabbit();
                self.ports.leases.window(&kind, WindowFact::Opens, opens.0);
                self.release()?;
                if let Err(reason) = self.label(number, false) {
                    return Ok(self.gate_failed(reason));
                }
                let stage = CodeRabbitStage::Lease { head };
                self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
                Ok(Begin::Report(StepReport::SummonRefused {
                    issue: self.item().issue,
                    pull_request: number,
                    opens,
                }))
            }
            Reading::Silent if waited >= REVIEW_WAIT => {
                self.accepted(at)?;
                if let Err(reason) = self.label(number, false) {
                    return Ok(self.gate_failed(reason));
                }
                self.raise(number, RulingKind::CodeRabbitSilent { head })
            }
            Reading::Processing => self.accepted(at).map(|()| Begin::Idle),
            Reading::Silent if waited >= ANSWER_WAIT => self.accepted(at).map(|()| Begin::Idle),
            Reading::Silent => Ok(Begin::Idle),
        }
    }

    // A round counts only here, once a review covers the head.
    fn review_landed(&mut self, number: u64, activity: &Activity) -> Result<Begin, StateError> {
        let Phase::CodeRabbit(stage) = self.item().phase.clone() else {
            unreachable!("a review lands in a CodeRabbit round")
        };
        let head = match stage {
            CodeRabbitStage::Lease { head } | CodeRabbitStage::Summoned { head, .. } => head,
            CodeRabbitStage::Judging { head, .. } => head,
        };
        let threads: Vec<OpenThread> = activity
            .open_threads()
            .map(|t| OpenThread {
                id: t.id.clone(),
                finding: coderabbit::finding(t),
            })
            .collect();
        let round = self.item().coderabbit.rounds + 1;
        let open_threads = threads.len();
        if threads.is_empty() {
            return self.satisfied(number, round);
        }
        self.update(|item| {
            item.coderabbit.rounds = round;
            item.phase = Phase::CodeRabbit(CodeRabbitStage::Judging {
                head,
                threads,
                verdicts: Vec::new(),
            });
        })?;
        Ok(Begin::Report(StepReport::CodeRabbitReviewed {
            issue: self.item().issue,
            pull_request: number,
            round,
            open_threads,
        }))
    }

    fn judge_threads(
        &mut self,
        threads: &[OpenThread],
        verdicts: &[Verdict],
    ) -> Result<Begin, StateError> {
        let Some(next) = threads.get(verdicts.len()) else {
            return self.judged(threads, verdicts);
        };
        let item = self.item();
        let (worktree, folder) = (item.worktree.clone(), self.paths.worker.clone());
        let model = self.settings.models.judge.clone();
        match calls::judge_call(&worktree, &folder, &model, &next.finding) {
            Ok(call) => {
                self.mark_review_call_running()?;
                Ok(Begin::Review(ReviewCall::Judge(call)))
            }
            Err(reason) => Ok(self.gate_failed(reason)),
        }
    }

    // Resolving is safe to repeat, so a failure part way retries it all.
    fn judged(
        &mut self,
        threads: &[OpenThread],
        verdicts: &[Verdict],
    ) -> Result<Begin, StateError> {
        let number = self.number();
        let mut held = Vec::new();
        let mut resolved = 0;
        for (thread, verdict) in threads.iter().zip(verdicts) {
            if verdict.holds {
                held.push(Finding {
                    severity: verdict.severity,
                    ..thread.finding.clone()
                });
                continue;
            }
            let done = self
                .ports
                .forge
                .resolve_thread(&self.settings.forge, &thread.id);
            if let Err(e) = done {
                return Ok(self.gate_failed(format!("cannot resolve a thread on #{number}: {e}")));
            }
            resolved += 1;
        }
        let tally = self.item().coderabbit;
        if held.is_empty() {
            return self.satisfied(number, tally.rounds);
        }
        let path = findings::findings_path(&self.paths.worker);
        let round = tally.rounds;
        if let Err(reason) = findings::write_findings_file(&self.paths.worker, &path, round, &held)
        {
            return Ok(self.gate_failed(reason));
        }
        let prompt = fix_prompt(number, round, held.len(), &path);
        let changed = cap::changed_lines(&self.item().worktree, &self.settings.generated);
        let cap = match changed {
            Ok(changed) => cap::cap(changed, self.settings.coderabbit.divisor),
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        if round >= cap && !tally.cap_cleared {
            let kind = RulingKind::CodeRabbitCap {
                rounds: round,
                held: u32::try_from(held.len()).unwrap_or(u32::MAX),
                prompt,
            };
            return self.raise(number, kind);
        }
        self.update(|item| {
            item.turn = Turn::Next { prompt };
            item.phase = Phase::Implement;
        })?;
        Ok(Begin::Report(StepReport::CodeRabbitJudged {
            issue: self.item().issue,
            pull_request: number,
            round,
            held: held.len(),
            resolved,
        }))
    }

    // The merge ruling still waits for CI, which a ready or rebased head reruns.
    fn satisfied(&mut self, number: u64, rounds: u32) -> Result<Begin, StateError> {
        let since = self.ports.clock.now();
        self.update(|item| {
            item.coderabbit.rounds = rounds;
            item.coderabbit.satisfied = true;
            item.phase = Phase::Ci { head: None, since };
        })?;
        Ok(Begin::Report(StepReport::CodeRabbitSatisfied {
            issue: self.item().issue,
            pull_request: number,
            rounds,
        }))
    }

    /// Takes the judge's verdict on one open thread
    pub(super) fn coderabbit_verdict(
        &mut self,
        result: ReviewResult,
    ) -> Result<Option<StepReport>, StateError> {
        let mut next = self.state.clone();
        let Some(item) = next.work_item.as_mut() else {
            return Ok(None);
        };
        item.review_call = ReviewCallState::Idle;
        let issue = item.issue;
        let round = item.coderabbit.rounds;
        let report = match (result, &mut item.phase) {
            (
                ReviewResult::Verdict(Ok(verdict)),
                Phase::CodeRabbit(CodeRabbitStage::Judging { verdicts, .. }),
            ) => {
                let (holds, severity) = (verdict.holds, verdict.severity);
                verdicts.push(verdict);
                Some(StepReport::FindingJudged {
                    issue,
                    round,
                    holds,
                    severity,
                })
            }
            (ReviewResult::Verdict(Err(reason)) | ReviewResult::Findings(Err(reason)), _) => {
                Some(StepReport::GateFailed { issue, reason })
            }
            _ => None,
        };
        self.save(next)?;
        Ok(report)
    }

    /// Gives the CodeRabbit lease back and takes the label off, when the
    /// work item leaves a round for good
    pub(super) fn leave_round(&mut self) {
        let Some(number) = self.state.work_item.as_ref().and_then(|i| i.pull_request) else {
            return;
        };
        let _ = self.release();
        let _ = self.label(number, false);
    }

    // What a summon's answer tells the dog: the hour runs from `at`.
    fn accepted(&mut self, at: Timestamp) -> Result<(), StateError> {
        let kind = LeaseKind::coderabbit();
        self.ports.leases.window(&kind, WindowFact::Summoned, at.0);
        self.release()
    }

    fn hold(&mut self, since: Timestamp) -> Result<(), StateError> {
        let mut next = self.state.clone();
        next.leases.retain(|l| l.resource != Resource::Coderabbit);
        next.leases.push(LeaseHeld {
            resource: Resource::Coderabbit,
            since,
        });
        self.save(next)
    }

    /// Gives the CodeRabbit lease back, if this run holds or asks for it
    pub(super) fn release(&mut self) -> Result<(), StateError> {
        self.ports.leases.give_back(&LeaseKind::coderabbit());
        if !self
            .state
            .leases
            .iter()
            .any(|l| l.resource == Resource::Coderabbit)
        {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.leases.retain(|l| l.resource != Resource::Coderabbit);
        self.save(next)
    }

    // Reads CodeRabbit's activity and passes the latest footer's quota on.
    fn activity(&self, number: u64) -> Result<Activity, String> {
        let activity = self
            .ports
            .forge
            .coderabbit(&self.settings.forge, number)
            .map_err(|e| format!("cannot read CodeRabbit on #{number}: {e}"))?;
        if let Some(quota) = activity.quota() {
            let kind = LeaseKind::coderabbit();
            self.ports
                .leases
                .window(&kind, WindowFact::Quota, quota.into());
        }
        Ok(activity)
    }

    // Changes the label only when it is not already as asked.
    // Records the label as kelpie's own, so the gate never reads it as a
    // change someone else made.
    fn label(&mut self, number: u64, on: bool) -> Result<(), String> {
        if self.labelled(number)? != on {
            self.ports
                .forge
                .set_label(&self.settings.forge, number, LABEL, on)
                .map_err(|e| format!("cannot change `{LABEL}` on #{number}: {e}"))?;
        }
        self.update(|item| {
            item.known.labels.retain(|l| l != LABEL);
            if on {
                item.known.labels.push(LABEL.to_owned());
            }
        })
        .map_err(|e| e.to_string())
    }

    fn labelled(&self, number: u64) -> Result<bool, String> {
        let pr = self
            .ports
            .forge
            .pull_request(&self.settings.forge, number)
            .map_err(|e| format!("cannot read #{number}: {e}"))?;
        Ok(pr.labels.iter().any(|l| l == LABEL))
    }

    fn item(&self) -> &WorkItem {
        self.state
            .work_item
            .as_ref()
            .expect("a CodeRabbit round is of a work item")
    }

    fn number(&self) -> u64 {
        self.item()
            .pull_request
            .expect("a CodeRabbit round is of a known pull request")
    }
}

fn fix_prompt(number: u64, round: u32, count: usize, path: &std::path::Path) -> String {
    format!(
        "CodeRabbit round {round} on your pull request #{number} left {count} \
         finding(s) that hold, in {}. Fix each one, then commit and push with \
         `git push origin HEAD`.",
        path.display()
    )
}
