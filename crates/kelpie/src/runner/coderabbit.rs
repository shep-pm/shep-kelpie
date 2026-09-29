//! CodeRabbit rounds, between green CI and the merge ruling
//!
//! A round marks a draft pull request ready, since CodeRabbit skips drafts,
//! takes the CodeRabbit lease, puts the `review please` label on,
//! and gives the lease back once CodeRabbit answers. A refusal reschedules
//! the dog's window and the round asks again. Once a review covers the
//! head the label comes off, the judge reads every open thread, rejected
//! ones are resolved and held ones go to the worker. Satisfied means no
//! thread open and nothing held. Past the cap, held findings park the worker.
//!
//! On a pull request CodeRabbit read before, the label asks only for what is
//! new, and after an adoption or a catch-up with `main` it finds nothing.
//! Those summons ask for a full review by comment instead. A head CodeRabbit
//! marks done with nothing posted was read and found clean, unless a summon
//! is owed: that one asks once more, for a full review.

mod cap;
#[cfg(test)]
pub(super) mod tests;

use super::Runner;
use super::gate::settled;
use super::report::{Begin, ReviewCall, ReviewResult, Spent, StepReport};
use super::review::{calls, findings, record_spent};
use crate::coderabbit::{self, Activity, Reading};
use crate::lease::LeaseKind;
use crate::lease::wire::WindowFact;
use crate::ports::{Finding, PullRequestState, Timestamp, Verdict};
use crate::state::{Fix, LeaseHeld, Resource, RulingKind, StateError};
use crate::work_item::{CodeRabbitStage, OpenThread, Phase, Turn, WorkItem};

/// The label shep's `.coderabbit.yaml` gates auto review on: the summon
pub const LABEL: &str = "review please";

/// The comment that asks CodeRabbit to read the whole pull request again
pub const FULL_REVIEW: &str = "@coderabbitai full review";

// A summon neither taken up nor refused in ten minutes is counted as spent,
// so the lease goes back.
pub(super) const ANSWER_WAIT: u64 = 600;

// A full review of a long branch took 24 minutes on shep. Two hours with
// none is the maintainer's to look at.
pub(super) const REVIEW_WAIT: u64 = 2 * 3600;

// CodeRabbit posts a review a few seconds before it marks the head done
// (nine on shep#614), so done with nothing posted is read a minute on.
pub(super) const DONE_SETTLE: u64 = 60;

impl Runner {
    /// Whether the work item owes CodeRabbit a round before its merge ruling
    pub(super) fn coderabbit_due(&self) -> bool {
        self.settings.coderabbit.enabled && !self.item().coderabbit.satisfied
    }

    /// Starts a round on `head`, which CI has just passed
    pub(super) fn start_round(&mut self, head: String) -> Result<Begin, StateError> {
        self.update(|item| {
            item.phase = Phase::CodeRabbit(CodeRabbitStage::Lease {
                head,
                readied: None,
                full: false,
            })
        })?;
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
            CodeRabbitStage::Lease {
                head,
                readied,
                full,
            } => self.summon(head, readied, full),
            CodeRabbitStage::Summoned { head, at, full } => self.await_review(head, at, full),
            CodeRabbitStage::Judging {
                threads, verdicts, ..
            } => self.judge_threads(&threads, &verdicts),
            CodeRabbitStage::Fixing { head } => self.fix_turn_ended(head),
        }
    }

    // No summon without the lease, and none for a head already reviewed:
    // that one costs the hour and buys nothing. CodeRabbit skips a draft, so
    // a draft is marked ready first and the summon waits for the next pass:
    // the forge can show the old state for a few seconds after. `full` asks
    // for a full review whatever CodeRabbit read before.
    fn summon(
        &mut self,
        head: String,
        readied: Option<Timestamp>,
        full: bool,
    ) -> Result<Begin, StateError> {
        let number = self.number();
        let activity = match self.activity(number) {
            Ok(activity) => activity,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        // A review from before an adoption lands only to hand on its findings.
        let owed = self.item().summon_owed;
        if activity.covers(&head) && (!owed || activity.open_threads().next().is_some()) {
            return self.review_landed(number, &activity);
        }
        if let Some(begin) = self.ready_for_review(number, &head, readied, full)? {
            return Ok(begin);
        }
        let kind = LeaseKind::coderabbit();
        self.ports.leases.want(&kind);
        if !self.ports.leases.holds(&kind) {
            return Ok(Begin::Idle);
        }
        let now = self.ports.clock.now();
        self.hold(now)?;
        let item = self.item();
        let read_before = activity.reviewed_besides("") > 0;
        if full || (read_before && (item.summon_owed || item.rebased)) {
            return self.ask_full(number, head, now);
        }
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
            full: false,
        };
        self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
        Ok(Begin::Report(StepReport::Summoned {
            issue: self.item().issue,
            pull_request: number,
            head,
        }))
    }

    // The summon is saved before the comment goes out, so a restart never
    // posts it twice. A comment the forge refused leaves the round waiting
    // for the lease again. The label comes off first, since a push while it
    // is on would summon outside the lease.
    fn ask_full(&mut self, number: u64, head: String, now: Timestamp) -> Result<Begin, StateError> {
        if let Err(reason) = self.label(number, false) {
            return Ok(self.gate_failed(reason));
        }
        let stage = CodeRabbitStage::Summoned {
            head: head.clone(),
            at: now,
            full: true,
        };
        self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
        let posted = self
            .ports
            .forge
            .comment(&self.settings.forge, number, FULL_REVIEW);
        if let Err(e) = posted {
            let stage = CodeRabbitStage::Lease {
                head,
                readied: None,
                full: true,
            };
            self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
            let reason = format!("cannot ask CodeRabbit for a full review on #{number}: {e}");
            return Ok(self.gate_failed(reason));
        }
        Ok(Begin::Report(StepReport::Summoned {
            issue: self.item().issue,
            pull_request: number,
            head,
        }))
    }

    // A draft is marked ready once, and the round then waits out the settle
    // for the forge to read it so: marking again on every stale read would
    // loop with no backoff. A forge still reading draft after the settle is
    // marked again, since a draft cannot be summoned. A pull request the forge
    // shows ready is recorded as kelpie's own, so the gate never reads it as
    // someone else's change.
    fn ready_for_review(
        &mut self,
        number: u64,
        head: &str,
        readied: Option<Timestamp>,
        full: bool,
    ) -> Result<Option<Begin>, StateError> {
        let repo = self.settings.forge.clone();
        let pr = match self.ports.forge.pull_request(&repo, number) {
            Ok(pr) => pr,
            Err(e) => {
                let reason = format!("cannot read #{number}: {e}");
                return Ok(Some(self.gate_failed(reason)));
            }
        };
        if !pr.draft {
            if !self.item().known.ready {
                self.update(|item| item.known.ready = true)?;
            }
            return Ok(None);
        }
        let now = self.ports.clock.now();
        if readied.is_some_and(|at| !settled(at, now)) {
            return Ok(Some(Begin::Idle));
        }
        if let Err(e) = self.ports.forge.mark_ready(&repo, number) {
            let reason = format!("cannot mark #{number} ready: {e}");
            return Ok(Some(self.gate_failed(reason)));
        }
        let head = head.to_owned();
        self.update(|item| {
            item.known.ready = true;
            item.phase = Phase::CodeRabbit(CodeRabbitStage::Lease {
                head,
                readied: Some(now),
                full,
            });
        })?;
        Ok(Some(Begin::Report(StepReport::MarkedReady {
            issue: self.item().issue,
            pull_request: number,
        })))
    }

    fn await_review(
        &mut self,
        head: String,
        at: Timestamp,
        full: bool,
    ) -> Result<Begin, StateError> {
        let number = self.number();
        let activity = match self.activity(number) {
            Ok(activity) => activity,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let now = self.ports.clock.now().0;
        let waited = now.saturating_sub(at.0);
        // A head reviewed before an adoption needs a review of kelpie's own.
        let owed = self.item().summon_owed;
        let activity = if owed { activity.since(at) } else { activity };
        match activity.read(&head, at) {
            Reading::Reviewed => self.answered(number, &activity, at),
            Reading::Completed { at: done } if now.saturating_sub(done.0) < DONE_SETTLE => {
                self.accepted(at).map(|()| Begin::Idle)
            }
            Reading::Completed { .. } if !owed => self.answered(number, &activity, at),
            // An owed summon found nothing new: ask once more, for a full review.
            Reading::Completed { .. } if !full => {
                self.accepted(at)?;
                if let Err(reason) = self.label(number, false) {
                    return Ok(self.gate_failed(reason));
                }
                let stage = CodeRabbitStage::Lease {
                    head: head.clone(),
                    readied: None,
                    full: true,
                };
                self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
                self.summon(head, None, true)
            }
            Reading::Refused { opens } => {
                let kind = LeaseKind::coderabbit();
                self.ports.leases.window(&kind, WindowFact::Opens, opens.0);
                self.release()?;
                if let Err(reason) = self.label(number, false) {
                    return Ok(self.gate_failed(reason));
                }
                let stage = CodeRabbitStage::Lease {
                    head,
                    readied: None,
                    full,
                };
                self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
                Ok(Begin::Report(StepReport::SummonRefused {
                    issue: self.item().issue,
                    pull_request: number,
                    opens,
                }))
            }
            Reading::Silent | Reading::Completed { .. } if waited >= REVIEW_WAIT => {
                self.accepted(at)?;
                if let Err(reason) = self.label(number, false) {
                    return Ok(self.gate_failed(reason));
                }
                self.raise(number, RulingKind::CodeRabbitSilent { head })
            }
            Reading::Processing | Reading::Completed { .. } => {
                self.accepted(at).map(|()| Begin::Idle)
            }
            Reading::Silent if waited >= ANSWER_WAIT => self.accepted(at).map(|()| Begin::Idle),
            Reading::Silent => Ok(Begin::Idle),
        }
    }

    // A summon answered, by a review of the head or by CodeRabbit finding
    // nothing new in it, which is a clean read.
    fn answered(
        &mut self,
        number: u64,
        activity: &Activity,
        at: Timestamp,
    ) -> Result<Begin, StateError> {
        self.accepted(at)?;
        if self.item().summon_owed {
            self.update(|item| item.summon_owed = false)?;
        }
        if let Err(reason) = self.label(number, false) {
            return Ok(self.gate_failed(reason));
        }
        self.review_landed(number, activity)
    }

    // A round counts only here, once CodeRabbit has read the head, which
    // also reads whatever a catch-up with `main` brought.
    fn review_landed(&mut self, number: u64, activity: &Activity) -> Result<Begin, StateError> {
        if self.item().rebased {
            self.update(|item| item.rebased = false)?;
        }
        let Phase::CodeRabbit(stage) = self.item().phase.clone() else {
            unreachable!("a review lands in a CodeRabbit round")
        };
        let head = match stage {
            CodeRabbitStage::Lease { head, .. } | CodeRabbitStage::Summoned { head, .. } => head,
            CodeRabbitStage::Judging { head, .. } | CodeRabbitStage::Fixing { head } => head,
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
        let (issue, worktree, folder) =
            (item.issue, item.worktree.clone(), self.paths.worker.clone());
        let model = self.settings.models.judge.clone();
        // CodeRabbit reviews the whole pull request, so its judge diffs from `main`.
        let main = format!("origin/{}", crate::worktree::BASE);
        match calls::judge_call(
            issue,
            &worktree,
            &main,
            &folder,
            &model,
            &next.finding,
            None,
        ) {
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
        let build = &self.item().build;
        let path = findings::findings_path(build);
        let round = tally.rounds;
        if let Err(reason) = findings::write_findings_file(build, &path, round, &held) {
            return Ok(self.gate_failed(reason));
        }
        let prompt = fix_prompt(number, round, held.len(), &path);
        let head = match self.origin_head() {
            Ok(head) => head,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
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
                head: Some(head),
            };
            return self.raise(number, kind);
        }
        self.update(|item| {
            item.turn = Turn::Next { prompt };
            item.phase = Phase::CodeRabbit(CodeRabbitStage::Fixing { head });
        })?;
        Ok(Begin::Report(StepReport::CodeRabbitJudged {
            issue: self.item().issue,
            pull_request: number,
            round,
            held: held.len(),
            resolved,
        }))
    }

    // A fix turn that pushed nothing fixed nothing, whatever it says: the
    // held findings still stand, and CI would pass the same head again.
    fn fix_turn_ended(&mut self, head: String) -> Result<Begin, StateError> {
        let number = self.number();
        let round = self.item().coderabbit.rounds;
        let pushed = match self.origin_head() {
            Ok(pushed) => pushed,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        if pushed == head {
            let path = findings::findings_path(&self.item().build);
            let prompt = findings::again_prompt(number, round, &path);
            let fix = Fix::CodeRabbit { round, head };
            return self.raise(number, RulingKind::FixNotPushed { fix, prompt });
        }
        let since = self.ports.clock.now();
        self.update(|item| item.phase = Phase::Ci { head: None, since })?;
        Ok(Begin::Report(StepReport::FixPushed {
            issue: self.item().issue,
            pull_request: number,
            round,
            head: Some(pushed),
        }))
    }

    // The merge ruling still waits for CI, which a ready or rebased head reruns.
    fn satisfied(&mut self, number: u64, rounds: u32) -> Result<Begin, StateError> {
        if self.item().summon_owed {
            return self.summon_owed(rounds);
        }
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

    // Reviews from before an adoption count toward the cap but never satisfy
    // a round, so the round summons instead.
    fn summon_owed(&mut self, rounds: u32) -> Result<Begin, StateError> {
        let Phase::CodeRabbit(stage) = self.item().phase.clone() else {
            unreachable!("a round is satisfied in a CodeRabbit round")
        };
        let head = match stage {
            CodeRabbitStage::Lease { head, .. } | CodeRabbitStage::Summoned { head, .. } => head,
            CodeRabbitStage::Judging { head, .. } | CodeRabbitStage::Fixing { head } => head,
        };
        let lease = CodeRabbitStage::Lease {
            head: head.clone(),
            readied: None,
            full: false,
        };
        self.update(|item| {
            item.coderabbit.rounds = rounds;
            item.phase = Phase::CodeRabbit(lease);
        })?;
        self.summon(head, None, false)
    }

    /// Takes the judge's verdict on one open thread
    pub(super) fn coderabbit_verdict(
        &mut self,
        result: ReviewResult,
        spent: Option<Spent>,
    ) -> Result<Option<StepReport>, StateError> {
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let Some(item) = next.work_item.as_mut() else {
            return Ok(None);
        };
        record_spent(item, spent, now);
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
        if let Some((per_hour, at)) = activity.quota() {
            let kind = LeaseKind::coderabbit();
            self.ports
                .leases
                .window(&kind, WindowFact::Quota(per_hour), at.0);
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
