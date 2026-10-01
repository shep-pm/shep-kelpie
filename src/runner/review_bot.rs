//! Review bot rounds, between green CI and the merge ruling
//!
//! A round marks a draft pull request ready, since a review bot may skip
//! drafts, takes the bot's lease, puts its label on, and gives the lease back
//! once the bot answers. A refusal reschedules the dog's window and the round
//! asks again. Once a review covers the head the label comes off, the judge
//! reads every open thread, rejected ones are resolved and held ones go to the
//! worker. Satisfied means no thread open and nothing held. Past the cap,
//! held findings park the worker. A fixed number of rounds replaces the
//! cap: the last round's held findings go to the worker with the label off,
//! and the fix push summons nothing.
//!
//! On a pull request the bot read before, the label asks only for what is
//! new, and after an adoption or a catch-up with `main` it finds nothing.
//! Those summons ask for a full review by comment instead, where the bot's
//! profile has one. A head the bot marks done with nothing posted was read
//! and found clean, unless a summon is owed: that one asks once more, for a
//! full review.
//!
//! A project may list several bots. Each round goes to the first listed
//! whose window is free, and the round cap counts rounds from all of them.

pub(super) mod cap;
#[cfg(test)]
mod codex_bot;
mod lease;
mod on_ready;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod two_bots;

use super::Runner;
use super::gate::settled;
use super::report::{Begin, ReviewCall, ReviewResult, Spent, StepReport};
use super::review::{calls, findings, record_spent};

use crate::lease::wire::WindowFact;
use crate::ports::{Finding, PullRequestState, Timestamp, Verdict};
use crate::review_bot::{Activity, Bot, Profile, Reading};
use crate::state::{Fix, RulingKind, StateError};
use crate::work_item::{CallKind, CodeRabbitStage, OpenThread, Phase, Turn, WorkItem};

// A summon the bot gave no sign of in fifteen minutes may never have
// reached it, and is sent once more. What counts as a sign is its profile's.
pub(super) const HEARD_WAIT: u64 = 900;

// A summon neither taken up nor refused in ten minutes is counted as spent,
// so the lease goes back. A summon with no sign waits for its re-send first.
pub(super) const ANSWER_WAIT: u64 = 600;

// A full review of a long branch took 24 minutes on shep. Two hours with
// none is the maintainer's to look at.
pub(super) const REVIEW_WAIT: u64 = 2 * 3600;

// CodeRabbit posts a review a few seconds before it marks the head done
// (nine on shep#614), so done with nothing posted is read a minute on.
pub(super) const DONE_SETTLE: u64 = 60;

impl Runner {
    /// Whether the work item owes the review bot a round before its merge ruling
    ///
    /// A summon owed since an adoption is due whatever rounds came before it.
    pub(super) fn review_bot_due(&self) -> bool {
        let item = self.item();
        let spent = cap::spent(item.coderabbit.rounds, self.settings.coderabbit.rounds);
        self.settings.coderabbit.enabled
            && !item.coderabbit.satisfied
            && (item.summon_owed || !spent)
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
        self.review_bot_step()
    }

    pub(super) fn review_bot_step(&mut self) -> Result<Begin, StateError> {
        let Phase::CodeRabbit(stage) = self.item().phase.clone() else {
            unreachable!("review_bot_step only runs in a review bot round")
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
            CodeRabbitStage::Summoned {
                bot,
                head,
                at,
                full,
                resent,
            } => self.await_review(bot, head, at, full, resent),
            CodeRabbitStage::Judging {
                bot,
                threads,
                verdicts,
                ..
            } => self.judge_threads(bot, &threads, &verdicts),
            CodeRabbitStage::Fixing { head } => self.fix_turn_ended(head),
        }
    }

    // No summon without the lease, and none for a head already reviewed:
    // that one costs the hour and buys nothing. CodeRabbit skips a draft, so
    // a draft is marked ready first and the summon waits for the next pass:
    // the forge can show the old state for a few seconds after. `full` asks
    // for a full review whatever the bot read before. The first listed bot
    // is read while the round waits, as its footer states its quota. A bot that
    // reviews a draft when it is marked ready is summoned by marking it.
    fn summon(
        &mut self,
        head: String,
        readied: Option<Timestamp>,
        full: bool,
    ) -> Result<Begin, StateError> {
        let number = self.number();
        let first = self.settings.reviewers()[0];
        // A first bot that cannot be read still leaves the round to another.
        let read_first = self.activity(first, number);
        if let Ok(activity) = &read_first
            && self.lands_unsummoned(first, &head, activity)
        {
            return self.review_landed(number, first, activity);
        }
        if let Some(begin) = self.summon_by_ready(&head, readied, full)? {
            return Ok(begin);
        }
        if let Some(begin) = self.ready_for_review(number, &head, readied, full)? {
            return Ok(begin);
        }
        let Some(bot) = self.choose_bot()? else {
            return Ok(match read_first {
                Ok(_) => Begin::Idle,
                Err(reason) => self.gate_failed(reason),
            });
        };
        let read = if bot == first {
            read_first
        } else {
            self.activity(bot, number)
        };
        let activity = match read {
            Ok(activity) => activity,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        if self.lands_unsummoned(bot, &head, &activity) {
            self.release(bot)?;
            return self.review_landed(number, bot, &activity);
        }
        let now = self.ports.clock.now();
        self.hold(bot, now)?;
        let full = self.asks_full(bot, full, &activity);
        self.send(bot, number, head, now, full, false)
    }

    // Whether `bot` already reviewed `head`, so the round needs no summon.
    // A review from before an adoption lands only to hand on its findings.
    fn lands_unsummoned(&self, bot: Bot, head: &str, activity: &Activity) -> bool {
        let owed = self.item().summon_owed;
        self.profile(bot).covers(activity, head)
            && (!owed || activity.open_threads().next().is_some())
    }

    // Whether a summon asks for a full review by comment: when it was told
    // to, when the bot has no label, or when the bot read the pull request
    // before and the label would find nothing new, because a summon is owed
    // or kelpie caught the branch up. A bot with no such comment always gets
    // the label.
    fn asks_full(&self, bot: Bot, full: bool, activity: &Activity) -> bool {
        let (item, bot) = (self.item(), self.profile(bot));
        let read_before = bot.reviewed_besides(activity, "") > 0;
        let wanted =
            full || bot.label().is_none() || (read_before && (item.summon_owed || item.rebased));
        wanted && bot.full_review().is_some()
    }

    // Sends the summon, under the lease, in whichever form `full` says.
    // `again` is a re-send of a summon the bot gave no sign of: it goes
    // out as a fresh event, and `at` stays the first one's time, so the
    // round is the same and its hour is not counted twice.
    fn send(
        &mut self,
        bot: Bot,
        number: u64,
        head: String,
        at: Timestamp,
        full: bool,
        again: bool,
    ) -> Result<Begin, StateError> {
        if full {
            return self.ask_full(bot, number, head, at, again);
        }
        // A label kelpie put on is a summon made before a restart could save
        // it. Any other label on sends no event, so it comes off first, and
        // so does one the bot never acted on.
        let label = self.profile(bot).label().map(str::to_owned);
        let ours = self
            .item()
            .known
            .labels
            .iter()
            .any(|l| Some(l) == label.as_ref());
        let summoned = match self.labelled(bot, number) {
            Ok(on) => ours && on && !again,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        if !summoned
            && let Err(reason) = self
                .label(bot, number, false)
                .and_then(|()| self.label(bot, number, true))
        {
            return Ok(self.gate_failed(reason));
        }
        let stage = CodeRabbitStage::Summoned {
            bot,
            head: head.clone(),
            at,
            full: false,
            resent: again,
        };
        self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
        Ok(self.summoned(number, head, again))
    }

    fn summoned(&self, number: u64, head: String, again: bool) -> Begin {
        let (issue, pull_request) = (self.item().issue, number);
        Begin::Report(if again {
            StepReport::SummonedAgain {
                issue,
                pull_request,
                head,
            }
        } else {
            StepReport::Summoned {
                issue,
                pull_request,
                head,
            }
        })
    }

    // The summon is saved before the comment goes out, so a restart never
    // posts it twice. A comment the forge refused leaves the round waiting
    // for the lease again, or for the re-send again. The label comes off
    // first, since a push while it is on would summon outside the lease.
    fn ask_full(
        &mut self,
        bot: Bot,
        number: u64,
        head: String,
        at: Timestamp,
        again: bool,
    ) -> Result<Begin, StateError> {
        if let Err(reason) = self.label(bot, number, false) {
            return Ok(self.gate_failed(reason));
        }
        let stage = CodeRabbitStage::Summoned {
            bot,
            head: head.clone(),
            at,
            full: true,
            resent: again,
        };
        self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
        let profile = self.profile(bot);
        let body = profile.full_review().unwrap_or_default();
        let posted = self.ports.forge.comment(&self.settings.forge, number, body);
        if let Err(e) = posted {
            let stage = if again {
                CodeRabbitStage::Summoned {
                    bot,
                    head,
                    at,
                    full: true,
                    resent: false,
                }
            } else {
                CodeRabbitStage::Lease {
                    head,
                    readied: None,
                    full: true,
                }
            };
            self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
            let name = profile.name();
            let reason = format!("cannot ask {name} for a full review on #{number}: {e}");
            return Ok(self.gate_failed(reason));
        }
        Ok(self.summoned(number, head, again))
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
        bot: Bot,
        head: String,
        at: Timestamp,
        full: bool,
        resent: bool,
    ) -> Result<Begin, StateError> {
        let number = self.number();
        let activity = match self.activity(bot, number) {
            Ok(activity) => activity,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let now = self.ports.clock.now().0;
        let waited = now.saturating_sub(at.0);
        // A re-send asks the way the first summon did, which is read from
        // everything the bot has done, not only since the summon.
        let full_again = self.asks_full(bot, full, &activity);
        let profile = self.profile(bot);
        let heard = profile.heard(&activity, &head, at);
        // A head reviewed before an adoption needs a review of kelpie's own.
        let owed = self.item().summon_owed;
        let activity = if owed { activity.since(at) } else { activity };
        match profile.read(&activity, &head, at) {
            Reading::Reviewed => self.answered(bot, number, &activity, at),
            Reading::Completed { at: done } if now.saturating_sub(done.0) < DONE_SETTLE => {
                self.accepted(bot, at).map(|()| Begin::Idle)
            }
            // A bot with no full review has nothing more to ask for.
            Reading::Completed { .. } if !owed || profile.full_review().is_none() => {
                self.answered(bot, number, &activity, at)
            }
            // An owed summon found nothing new: ask once more, for a full review.
            Reading::Completed { .. } if !full => {
                self.accepted(bot, at)?;
                if let Err(reason) = self.label(bot, number, false) {
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
            // The bot is parked until its window opens, and the round falls
            // to whichever listed bot is free first.
            Reading::Refused { opens } => {
                let opens = opens.unwrap_or_else(|| self.parked_until(bot, Timestamp(now)));
                let kind = profile.lease();
                self.ports.leases.window(&kind, WindowFact::Opens, opens.0);
                self.release(bot)?;
                if let Err(reason) = self.label(bot, number, false) {
                    return Ok(self.gate_failed(reason));
                }
                // A bot with no label is always asked by comment, which asks
                // nothing of the bot that takes the round next.
                let stage = CodeRabbitStage::Lease {
                    head,
                    readied: None,
                    full: full && profile.label().is_some(),
                };
                self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
                Ok(Begin::Report(StepReport::SummonRefused {
                    issue: self.item().issue,
                    pull_request: number,
                    opens,
                }))
            }
            Reading::Silent | Reading::Processing | Reading::Completed { .. }
                if waited >= REVIEW_WAIT =>
            {
                self.accepted(bot, at)?;
                if let Err(reason) = self.label(bot, number, false) {
                    return Ok(self.gate_failed(reason));
                }
                self.raise(number, RulingKind::CodeRabbitSilent { bot, head })
            }
            Reading::Processing | Reading::Completed { .. } => {
                self.accepted(bot, at).map(|()| Begin::Idle)
            }
            // No sign of the summon yet: it may never have been seen, so it
            // goes out once more before the lease is counted spent.
            Reading::Silent if !resent && !heard => {
                if waited < HEARD_WAIT {
                    Ok(Begin::Idle)
                } else {
                    self.resend(bot, head, at, full_again)
                }
            }
            Reading::Silent if waited >= ANSWER_WAIT => {
                self.accepted(bot, at).map(|()| Begin::Idle)
            }
            Reading::Silent => Ok(Begin::Idle),
        }
    }

    // The same summon, once more, under the lease this work item took. A
    // restart clears the book of them, and the grant then held may be
    // another item's, so without its own row nothing is sent.
    fn resend(
        &mut self,
        bot: Bot,
        head: String,
        at: Timestamp,
        full: bool,
    ) -> Result<Begin, StateError> {
        let number = self.number();
        if !self.holds_own(bot) {
            return self.accepted(bot, at).map(|()| Begin::Idle);
        }
        self.send(bot, number, head, at, full, true)
    }

    // A summon answered, by a review of the head or by the bot finding
    // nothing new in it, which is a clean read. An answer from any listed
    // bot settles a summon owed since an adoption.
    fn answered(
        &mut self,
        bot: Bot,
        number: u64,
        activity: &Activity,
        at: Timestamp,
    ) -> Result<Begin, StateError> {
        self.accepted(bot, at)?;
        if self.item().summon_owed {
            self.update(|item| item.summon_owed = false)?;
        }
        if let Err(reason) = self.label(bot, number, false) {
            return Ok(self.gate_failed(reason));
        }
        self.review_landed(number, bot, activity)
    }

    // A round counts only here, once the bot has read the head, which
    // also reads whatever a catch-up with `main` brought.
    fn review_landed(
        &mut self,
        number: u64,
        bot: Bot,
        activity: &Activity,
    ) -> Result<Begin, StateError> {
        if self.item().rebased {
            self.update(|item| item.rebased = false)?;
        }
        let head = self.round_head();
        let mut threads = open_threads(&*self.profile(bot), activity);
        // Another listed bot's threads still open are findings too, so the
        // round is satisfied only with none open from any of them.
        for other in self.settings.reviewers().into_iter().filter(|b| *b != bot) {
            match self.activity(other, number) {
                Ok(theirs) => threads.extend(open_threads(&*self.profile(other), &theirs)),
                Err(reason) => return Ok(self.gate_failed(reason)),
            }
        }
        let round = self.item().coderabbit.rounds + 1;
        let open_threads = threads.len();
        if threads.is_empty() {
            return self.satisfied(number, round);
        }
        self.update(|item| {
            item.coderabbit.rounds = round;
            item.phase = Phase::CodeRabbit(CodeRabbitStage::Judging {
                bot,
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
        bot: Bot,
        threads: &[OpenThread],
        verdicts: &[Verdict],
    ) -> Result<Begin, StateError> {
        let Some(next) = threads.get(verdicts.len()) else {
            return self.judged(bot, threads, verdicts);
        };
        let item = self.item();
        let (issue, worktree, folder) =
            (item.issue, item.worktree.clone(), self.paths.worker.clone());
        let model = self.agents.judge.clone();
        // A review bot reviews the whole pull request, so its judge diffs from `main`.
        let main = format!("origin/{}", crate::worktree::BASE);
        match calls::judge_call(
            issue,
            &worktree,
            &main,
            &folder,
            (&model, &self.agents.limits.judge),
            &next.finding,
            None,
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

    // Resolving is safe to repeat, so a failure part way retries it all.
    fn judged(
        &mut self,
        bot: Bot,
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
        let prompt = fix_prompt(self.profile(bot).name(), number, round, held.len(), &path);
        self.update(|item| item.record_held(&held))?;
        let head = match self.origin_head() {
            Ok(head) => head,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let fixed = self.settings.coderabbit.rounds;
        let capped = match fixed {
            Some(_) => false,
            None => match cap::changed_lines(&self.item().worktree, &self.settings.generated) {
                Ok(changed) => {
                    round >= cap::cap(changed, self.settings.coderabbit.divisor)
                        && !tally.cap_cleared
                }
                Err(reason) => return Ok(self.gate_failed(reason)),
            },
        };
        // After the last round a push with the label on would summon another.
        if cap::spent(round, fixed)
            && let Err(reason) = self.label(bot, number, false)
        {
            return Ok(self.gate_failed(reason));
        }
        if capped {
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
        let head = self.round_head();
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
    pub(super) fn review_bot_verdict(
        &mut self,
        result: ReviewResult,
        spent: Option<Spent>,
    ) -> Result<Option<StepReport>, StateError> {
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        let Some(item) = self.current_in(&mut next) else {
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

    /// Gives back the lease of every bot the round may hold and takes each
    /// one's label off, when the work item leaves a round for good
    pub(super) fn leave_round(&mut self) {
        let Some(number) = self.current().and_then(|i| i.pull_request) else {
            return;
        };
        for bot in self.round_bots() {
            let _ = self.release(bot);
            let _ = self.label(bot, number, false);
        }
    }

    // The head the round is on, whatever its stage.
    fn round_head(&self) -> String {
        let Phase::CodeRabbit(stage) = &self.item().phase else {
            unreachable!("a review bot round is in its phase")
        };
        match stage {
            CodeRabbitStage::Lease { head, .. } | CodeRabbitStage::Summoned { head, .. } => head,
            CodeRabbitStage::Judging { head, .. } | CodeRabbitStage::Fixing { head } => head,
        }
        .clone()
    }

    // Reads `bot`'s activity and passes the quota it last stated on.
    fn activity(&self, bot: Bot, number: u64) -> Result<Activity, String> {
        let bot = self.profile(bot);
        let activity = self
            .ports
            .forge
            .review_bot(&self.settings.forge, number, bot.login())
            .map_err(|e| format!("cannot read {} on #{number}: {e}", bot.name()))?;
        if let Some((per_hour, at)) = bot.quota(&activity) {
            self.ports
                .leases
                .window(&bot.lease(), WindowFact::Quota(per_hour), at.0);
        }
        Ok(activity)
    }

    // Changes `bot`'s label only when it is not already as asked, and does
    // nothing for a bot with no label. Records the label as kelpie's own, so
    // the gate never reads it as a change someone else made.
    fn label(&mut self, bot: Bot, number: u64, on: bool) -> Result<(), String> {
        let profile = self.profile(bot);
        let Some(label) = profile.label() else {
            return Ok(());
        };
        if self.labelled(bot, number)? != on {
            self.ports
                .forge
                .set_label(&self.settings.forge, number, label, on)
                .map_err(|e| format!("cannot change `{label}` on #{number}: {e}"))?;
        }
        self.update(|item| {
            item.known.labels.retain(|l| l != label);
            if on {
                item.known.labels.push(label.to_owned());
            }
        })
        .map_err(|e| e.to_string())
    }

    fn labelled(&self, bot: Bot, number: u64) -> Result<bool, String> {
        let Some(label) = self.profile(bot).label().map(str::to_owned) else {
            return Ok(false);
        };
        let pr = self
            .ports
            .forge
            .pull_request(&self.settings.forge, number)
            .map_err(|e| format!("cannot read #{number}: {e}"))?;
        Ok(pr.labels.contains(&label))
    }

    fn item(&self) -> &WorkItem {
        self.current()
            .expect("a review bot round is of a work item")
    }

    fn number(&self) -> u64 {
        self.item()
            .pull_request
            .expect("a review bot round is of a known pull request")
    }
}

fn open_threads(profile: &dyn Profile, activity: &Activity) -> Vec<OpenThread> {
    let open = activity.open_threads().map(|t| OpenThread {
        id: t.id.clone(),
        finding: profile.finding(t),
    });
    open.collect()
}

fn fix_prompt(bot: &str, number: u64, round: u32, count: usize, path: &std::path::Path) -> String {
    format!(
        "{bot} round {round} on your pull request #{number} left {count} \
         finding(s) that hold, in {}. Fix each one, then commit and push with \
         `git push origin HEAD`.",
        path.display()
    )
}
