//! Outside reviewers' rounds, between green CI and the merge ruling
//!
//! Gemini's round runs first, then CodeRabbit's, each while its settings
//! turn it on and it is not yet satisfied. A round marks a draft pull request
//! ready, takes the reviewer's lease, summons it (CodeRabbit by the `review
//! please` label, Gemini by a `/gemini review` comment) and gives the lease
//! back once it answers. A refusal reschedules the dog's window and the round
//! asks again. Once a review covers the head, the judge reads its open
//! threads, rejected ones are resolved and held ones go to the worker.
//! Satisfied means nothing held. Past CodeRabbit's cap, held findings park
//! the worker. Gemini's cap counts its reviews on the pull request, and at
//! the cap the work item moves on to CodeRabbit without asking.

mod cap;
#[cfg(test)]
mod coderabbit_tests;
#[cfg(test)]
mod gemini_tests;

use super::Runner;
use super::gate::settled;
use super::report::{Begin, ReviewCall, ReviewResult, Spent, StepReport};
use super::review::{calls, findings, record_spent};
use crate::lease::wire::WindowFact;
use crate::outside::{Outside, Reading};
use crate::ports::{Finding, PullRequestState, Timestamp, Verdict};
use crate::state::{Fix, LeaseHeld, Resource, RulingKind, StateError};
use crate::work_item::{OpenThread, OutsideStage, Phase, Turn, WorkItem};
use crate::{coderabbit, gemini};

/// The label shep's `.coderabbit.yaml` gates auto review on: CodeRabbit's summon
pub const LABEL: &str = "review please";

// A summon neither taken up nor refused in ten minutes is counted as spent,
// so the lease goes back.
const ANSWER_WAIT: u64 = 600;

// A full review of a long branch took 24 minutes on shep. Two hours with
// none is the maintainer's to look at.
const REVIEW_WAIT: u64 = 2 * 3600;

// What an outside reviewer has posted on the pull request.
enum Posted {
    CodeRabbit(coderabbit::Activity),
    Gemini(gemini::Activity),
}

impl Posted {
    // A review of `head` before any summon: CodeRabbit's alone, since a
    // Gemini review kelpie did not ask for is never read.
    fn covers(&self, head: &str) -> bool {
        match self {
            Self::CodeRabbit(seen) => seen.covers(head),
            Self::Gemini(_) => false,
        }
    }

    // Gemini's reviews on the pull request, asked for or not.
    fn reviews(&self) -> usize {
        match self {
            Self::CodeRabbit(_) => 0,
            Self::Gemini(seen) => seen.reviews.len(),
        }
    }

    fn read(&self, head: &str, since: Timestamp) -> Reading {
        match self {
            Self::CodeRabbit(seen) => seen.read(head, since),
            Self::Gemini(seen) => seen.read(head, since),
        }
    }

    // CodeRabbit's open threads are all read: it resolves the ones a fix
    // settles. Gemini's are read from the review that answered the summon.
    fn open_threads(&self, head: &str, since: Option<Timestamp>) -> Vec<OpenThread> {
        let thread = |t: &crate::outside::Thread, finding| OpenThread {
            id: t.id.clone(),
            finding,
        };
        match (self, since) {
            (Self::CodeRabbit(seen), _) => seen
                .open_threads()
                .map(|t| thread(t, coderabbit::finding(t)))
                .collect(),
            (Self::Gemini(seen), Some(since)) => seen
                .open_threads(head, since)
                .into_iter()
                .map(|t| thread(t, gemini::finding(t)))
                .collect(),
            (Self::Gemini(_), None) => Vec::new(),
        }
    }
}

impl Runner {
    /// The outside reviewer the work item owes a round before its merge
    /// ruling, in the order their rounds run
    pub(super) fn outside_due(&self) -> Option<Outside> {
        Outside::ALL.into_iter().find(|reviewer| {
            self.settings.rounds_on(*reviewer) && !self.item().tally(*reviewer).satisfied
        })
    }

    /// Starts `reviewer`'s round on `head`, which CI has just passed
    pub(super) fn start_round(
        &mut self,
        reviewer: Outside,
        head: String,
    ) -> Result<Begin, StateError> {
        let stage = OutsideStage::Lease {
            head,
            readied: None,
        };
        self.update(|item| item.phase = Phase::round(reviewer, stage))?;
        self.outside_step()
    }

    pub(super) fn outside_step(&mut self) -> Result<Begin, StateError> {
        let Some((reviewer, stage)) = self.item().phase.outside() else {
            unreachable!("outside_step only runs in an outside reviewer's round")
        };
        let stage = stage.clone();
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
            OutsideStage::Lease { head, readied } => self.summon(reviewer, head, readied),
            OutsideStage::Summoned { head, at, posted } => {
                self.await_review(reviewer, head, at, posted)
            }
            OutsideStage::Judging {
                threads, verdicts, ..
            } => self.judge_threads(&threads, &verdicts),
            OutsideStage::Fixing { head } => self.fix_turn_ended(reviewer, head),
        }
    }

    // No summon without the lease, and none for a head already reviewed:
    // that one costs the window and buys nothing. Both reviewers are asked
    // on a pull request marked ready, since CodeRabbit skips a draft, and
    // the summon waits for the next pass: the forge can show the old state
    // for a few seconds after.
    fn summon(
        &mut self,
        reviewer: Outside,
        head: String,
        readied: Option<Timestamp>,
    ) -> Result<Begin, StateError> {
        let number = self.number();
        let posted = match self.posted(reviewer, number) {
            Ok(posted) => posted,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        if posted.covers(&head) {
            return self.review_landed(reviewer, number, &posted, None);
        }
        let max_rounds = usize::from(self.settings.gemini.max_rounds.get());
        if reviewer == Outside::Gemini && posted.reviews() >= max_rounds {
            return self.capped(number, posted.reviews());
        }
        if let Some(begin) = self.ready_for_review(reviewer, number, &head, readied)? {
            return Ok(begin);
        }
        let kind = reviewer.lease_kind();
        self.ports.leases.want(&kind);
        if !self.ports.leases.holds(&kind) {
            return Ok(Begin::Idle);
        }
        let now = self.ports.clock.now();
        self.hold(reviewer, now)?;
        match reviewer {
            Outside::CodeRabbit => self.label_summon(number, head, now),
            Outside::Gemini => {
                let stage = OutsideStage::Summoned {
                    head: head.clone(),
                    at: now,
                    posted: false,
                };
                self.update(|item| item.phase = Phase::Gemini(stage))?;
                self.comment_summon(number, head, now)
            }
        }
    }

    // A label kelpie put on is a summon made before a restart could save
    // it. Any other label on sends no event, so it comes off first.
    fn label_summon(
        &mut self,
        number: u64,
        head: String,
        now: Timestamp,
    ) -> Result<Begin, StateError> {
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
        let stage = OutsideStage::Summoned {
            head: head.clone(),
            at: now,
            posted: true,
        };
        self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
        Ok(Begin::Report(StepReport::Summoned {
            reviewer: Outside::CodeRabbit,
            issue: self.item().issue,
            pull_request: number,
            head,
        }))
    }

    // Posts `/gemini review` for the round saved as summoned at `at`, and
    // records it posted. A failed post is tried again on the next pass.
    fn comment_summon(
        &mut self,
        number: u64,
        head: String,
        at: Timestamp,
    ) -> Result<Begin, StateError> {
        let repo = self.settings.forge.clone();
        if let Err(e) = self.ports.forge.comment(&repo, number, gemini::SUMMON) {
            return Ok(self.gate_failed(format!("cannot summon Gemini on #{number}: {e}")));
        }
        let stage = OutsideStage::Summoned {
            head: head.clone(),
            at,
            posted: true,
        };
        self.update(|item| item.phase = Phase::Gemini(stage))?;
        Ok(Begin::Report(StepReport::Summoned {
            reviewer: Outside::Gemini,
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
        reviewer: Outside,
        number: u64,
        head: &str,
        readied: Option<Timestamp>,
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
            let stage = OutsideStage::Lease {
                head,
                readied: Some(now),
            };
            item.phase = Phase::round(reviewer, stage);
        })?;
        Ok(Some(Begin::Report(StepReport::MarkedReady {
            issue: self.item().issue,
            pull_request: number,
        })))
    }

    fn await_review(
        &mut self,
        reviewer: Outside,
        head: String,
        at: Timestamp,
        posted: bool,
    ) -> Result<Begin, StateError> {
        let number = self.number();
        let seen = match self.posted(reviewer, number) {
            Ok(seen) => seen,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        if !posted {
            return match &seen {
                Posted::Gemini(activity) if activity.summoned(at) => {
                    let stage = OutsideStage::Summoned {
                        head,
                        at,
                        posted: true,
                    };
                    self.update(|item| item.phase = Phase::Gemini(stage))
                        .map(|()| Begin::Idle)
                }
                _ => self.comment_summon(number, head, at),
            };
        }
        let waited = self.ports.clock.now().0.saturating_sub(at.0);
        match seen.read(&head, at) {
            Reading::Reviewed => {
                self.accepted(reviewer, at)?;
                if let Err(reason) = self.unsummon(reviewer, number) {
                    return Ok(self.gate_failed(reason));
                }
                self.review_landed(reviewer, number, &seen, Some(at))
            }
            Reading::Refused { opens } => {
                let kind = reviewer.lease_kind();
                self.ports.leases.window(&kind, WindowFact::Opens, opens.0);
                self.release_one(reviewer)?;
                if let Err(reason) = self.unsummon(reviewer, number) {
                    return Ok(self.gate_failed(reason));
                }
                let stage = OutsideStage::Lease {
                    head,
                    readied: None,
                };
                self.update(|item| item.phase = Phase::round(reviewer, stage))?;
                Ok(Begin::Report(StepReport::SummonRefused {
                    reviewer,
                    issue: self.item().issue,
                    pull_request: number,
                    opens,
                }))
            }
            Reading::Silent if waited >= REVIEW_WAIT => {
                self.accepted(reviewer, at)?;
                if let Err(reason) = self.unsummon(reviewer, number) {
                    return Ok(self.gate_failed(reason));
                }
                self.raise(number, RulingKind::OutsideSilent { reviewer, head })
            }
            Reading::Processing => self.accepted(reviewer, at).map(|()| Begin::Idle),
            Reading::Silent if waited >= ANSWER_WAIT => {
                self.accepted(reviewer, at).map(|()| Begin::Idle)
            }
            Reading::Silent => Ok(Begin::Idle),
        }
    }

    // A round counts only here, once a review covers the head.
    fn review_landed(
        &mut self,
        reviewer: Outside,
        number: u64,
        seen: &Posted,
        since: Option<Timestamp>,
    ) -> Result<Begin, StateError> {
        let Some((_, stage)) = self.item().phase.outside() else {
            unreachable!("a review lands in an outside reviewer's round")
        };
        let head = match stage.clone() {
            OutsideStage::Lease { head, .. } | OutsideStage::Summoned { head, .. } => head,
            OutsideStage::Judging { head, .. } | OutsideStage::Fixing { head } => head,
        };
        let threads = seen.open_threads(&head, since);
        let round = self.item().tally(reviewer).rounds + 1;
        let open_threads = threads.len();
        if threads.is_empty() {
            return self.satisfied(reviewer, number, round);
        }
        self.update(|item| {
            item.tally_mut(reviewer).rounds = round;
            let stage = OutsideStage::Judging {
                head,
                threads,
                verdicts: Vec::new(),
            };
            item.phase = Phase::round(reviewer, stage);
        })?;
        Ok(Begin::Report(StepReport::OutsideReviewed {
            reviewer,
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
        let Some((reviewer, _)) = self.item().phase.outside() else {
            unreachable!("threads are judged in an outside reviewer's round")
        };
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
        let tally = self.item().tally(reviewer);
        if held.is_empty() {
            return self.satisfied(reviewer, number, tally.rounds);
        }
        let build = &self.item().build;
        let path = findings::findings_path(build);
        let round = tally.rounds;
        if let Err(reason) = findings::write_findings_file(build, &path, round, &held) {
            return Ok(self.gate_failed(reason));
        }
        let prompt = fix_prompt(reviewer, number, round, held.len(), &path);
        let head = match self.origin_head() {
            Ok(head) => head,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        // Gemini's cap is checked before each summon instead, and never asks.
        let cap = match reviewer {
            Outside::CodeRabbit => {
                let changed = cap::changed_lines(&self.item().worktree, &self.settings.generated);
                match changed {
                    Ok(changed) => cap::cap(changed, self.settings.coderabbit.divisor),
                    Err(reason) => return Ok(self.gate_failed(reason)),
                }
            }
            Outside::Gemini => u32::MAX,
        };
        if round >= cap && !tally.cap_cleared {
            let kind = RulingKind::OutsideCap {
                reviewer,
                rounds: round,
                held: u32::try_from(held.len()).unwrap_or(u32::MAX),
                prompt,
                head: Some(head),
            };
            return self.raise(number, kind);
        }
        self.update(|item| {
            item.turn = Turn::Next { prompt };
            item.phase = Phase::round(reviewer, OutsideStage::Fixing { head });
        })?;
        Ok(Begin::Report(StepReport::OutsideJudged {
            reviewer,
            issue: self.item().issue,
            pull_request: number,
            round,
            held: held.len(),
            resolved,
        }))
    }

    // A fix turn that pushed nothing fixed nothing, whatever it says: the
    // held findings still stand, and CI would pass the same head again.
    fn fix_turn_ended(&mut self, reviewer: Outside, head: String) -> Result<Begin, StateError> {
        let number = self.number();
        let round = self.item().tally(reviewer).rounds;
        let pushed = match self.origin_head() {
            Ok(pushed) => pushed,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        if pushed == head {
            let path = findings::findings_path(&self.item().build);
            let prompt = findings::again_prompt(number, round, &path);
            let fix = Fix::Outside {
                reviewer,
                round,
                head,
            };
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

    // The merge ruling still waits for CI, which a ready or rebased head
    // reruns, and for any round still owed.
    fn satisfied(
        &mut self,
        reviewer: Outside,
        number: u64,
        rounds: u32,
    ) -> Result<Begin, StateError> {
        let since = self.ports.clock.now();
        self.update(|item| {
            let tally = item.tally_mut(reviewer);
            tally.rounds = rounds;
            tally.satisfied = true;
            item.phase = Phase::Ci { head: None, since };
        })?;
        Ok(Begin::Report(StepReport::OutsideSatisfied {
            reviewer,
            issue: self.item().issue,
            pull_request: number,
            rounds,
        }))
    }

    // Gemini's rounds reached their cap: the work item moves on through CI
    // to CodeRabbit, the later gate, with any fix Gemini's last round asked
    // for already made.
    fn capped(&mut self, number: u64, reviews: usize) -> Result<Begin, StateError> {
        let since = self.ports.clock.now();
        self.update(|item| {
            item.gemini.satisfied = true;
            item.phase = Phase::Ci { head: None, since };
        })?;
        Ok(Begin::Report(StepReport::GeminiCapped {
            issue: self.item().issue,
            pull_request: number,
            reviews,
        }))
    }

    /// Takes the judge's verdict on one open thread
    pub(super) fn outside_verdict(
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
        let round = item
            .phase
            .outside()
            .map_or(0, |(reviewer, _)| item.tally(reviewer).rounds);
        let report = match (result, &mut item.phase) {
            (
                ReviewResult::Verdict(Ok(verdict)),
                Phase::CodeRabbit(OutsideStage::Judging { verdicts, .. })
                | Phase::Gemini(OutsideStage::Judging { verdicts, .. }),
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

    /// Gives the round's lease back and takes CodeRabbit's label off, when
    /// the work item leaves a round for good
    pub(super) fn leave_round(&mut self) {
        let Some(item) = self.state.work_item.as_ref() else {
            return;
        };
        let (Some(number), Some((reviewer, _))) = (item.pull_request, item.phase.outside()) else {
            return;
        };
        let _ = self.release_one(reviewer);
        let _ = self.unsummon(reviewer, number);
    }

    // What a summon's answer tells the dog: the window runs from `at`.
    fn accepted(&mut self, reviewer: Outside, at: Timestamp) -> Result<(), StateError> {
        let kind = reviewer.lease_kind();
        self.ports.leases.window(&kind, WindowFact::Summoned, at.0);
        self.release_one(reviewer)
    }

    fn hold(&mut self, reviewer: Outside, since: Timestamp) -> Result<(), StateError> {
        let resource = Resource::window(reviewer);
        let mut next = self.state.clone();
        next.leases.retain(|l| l.resource != resource);
        next.leases.push(LeaseHeld { resource, since });
        self.save(next)
    }

    /// Gives every outside reviewer's lease back, if this run holds or
    /// asks for one
    pub(super) fn release(&mut self) -> Result<(), StateError> {
        Outside::ALL
            .into_iter()
            .try_for_each(|reviewer| self.release_one(reviewer))
    }

    fn release_one(&mut self, reviewer: Outside) -> Result<(), StateError> {
        self.ports.leases.give_back(&reviewer.lease_kind());
        let resource = Resource::window(reviewer);
        if !self.state.leases.iter().any(|l| l.resource == resource) {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.leases.retain(|l| l.resource != resource);
        self.save(next)
    }

    // Reads what `reviewer` posted, passing the latest footer's quota on.
    fn posted(&self, reviewer: Outside, number: u64) -> Result<Posted, String> {
        let repo = &self.settings.forge;
        let unread = |e| format!("cannot read {reviewer} on #{number}: {e}");
        let posted = match reviewer {
            Outside::CodeRabbit => {
                Posted::CodeRabbit(self.ports.forge.coderabbit(repo, number).map_err(unread)?)
            }
            Outside::Gemini => {
                Posted::Gemini(self.ports.forge.gemini(repo, number).map_err(unread)?)
            }
        };
        if let Posted::CodeRabbit(seen) = &posted
            && let Some((per_hour, at)) = seen.quota()
        {
            let kind = reviewer.lease_kind();
            self.ports
                .leases
                .window(&kind, WindowFact::Quota(per_hour), at.0);
        }
        Ok(posted)
    }

    // A Gemini summon is a comment, which stays: only CodeRabbit's label
    // comes off once it answers.
    fn unsummon(&mut self, reviewer: Outside, number: u64) -> Result<(), String> {
        match reviewer {
            Outside::CodeRabbit => self.label(number, false),
            Outside::Gemini => Ok(()),
        }
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
            .expect("an outside reviewer's round is of a work item")
    }

    fn number(&self) -> u64 {
        self.item()
            .pull_request
            .expect("an outside reviewer's round is of a known pull request")
    }
}

fn fix_prompt(
    reviewer: Outside,
    number: u64,
    round: u32,
    count: usize,
    path: &std::path::Path,
) -> String {
    format!(
        "{reviewer} round {round} on your pull request #{number} left {count} \
         finding(s) that hold, in {}. Fix each one, then commit and push with \
         `git push origin HEAD`.",
        path.display()
    )
}
