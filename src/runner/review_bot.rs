//! A review bot's round, in its place in the review pass
//!
//! A bot listed in `agents.reviewers` reads the pull request once a pass, as
//! any reviewer does. Its round marks a draft ready, since a bot may skip
//! drafts, waits for the bot's window and lease, summons it by its label or
//! its comment, and gives the lease back once the bot answers. A refusal
//! tells the dog when the window opens, and the round asks again. A window
//! that opens more than an hour on, by the dog's book or by the refusal,
//! passes the bot over for the pass, and so do two hours with no review, two
//! hours unable to summon, a bot the project stopped listing, and CodeRabbit
//! on a repo that is not public.
//! Once a review covers the head the label comes off, and once two reads
//! of the bot's open threads agree they are the round's findings, which go
//! to one fix turn as any reviewer's do. Kelpie resolves the threads it sent
//! once that fix moves the head. A bot that read the pull request before
//! has its nits on a head a nit-only fix pushed left open, not sent again.
//! A bot passed over that reviews the head anyway gets a round of its own
//! before the next reviewer's.
//!
//! On a pull request the bot read before, the label asks only for what is
//! new, and after an adoption or a catch-up with `main` it finds nothing.
//! Those summons ask for a full review by comment instead, where the bot's
//! profile has one. A head the bot marks done with nothing posted was read
//! and found clean, unless a summon is owed: that one asks once more, for a
//! full review.

mod lease;
mod on_ready;
mod settle;

#[cfg(test)]
pub(crate) use settle::SETTLE_LEAST;

use super::Runner;
use super::gate::settled;
use super::report::{Begin, StepReport};

use crate::lease::wire::WindowFact;
use crate::ports::{Finding, PullRequestState, Timestamp, Visibility};
use crate::review_bot::{Activity, Bot, BotReviewer, Reading, ReviewWindow};
use crate::settings::{AgentName, ListedReviewer};
use crate::state::{StateError, Stuck};
use crate::work_item::{BotSkipped, Phase, Review, ReviewStage, WorkItem};

// A summon the bot gave no sign of in fifteen minutes may never have
// reached it, and is sent once more. What counts as a sign is its profile's.
pub(super) const HEARD_WAIT: u64 = 900;

// A summon neither taken up nor refused in ten minutes is counted as spent,
// so the lease goes back. A summon with no sign waits for its re-send first.
pub(super) const ANSWER_WAIT: u64 = 600;

// A full review of a long branch took 24 minutes on shep. Two hours with
// none passes the bot over for the pass.
pub(super) const REVIEW_WAIT: u64 = 2 * 3600;

// CodeRabbit posts a review a few seconds before it marks the head done
// (nine on shep#614), so done with nothing posted is read a minute on.
pub(super) const DONE_SETTLE: u64 = 60;

// A window that opens further on than this would hold the whole pass, so
// the pass goes on without the bot.
pub(super) const FAR: u64 = 3600;

// Steps in a row that may fail to resolve the threads sent before the
// review goes on with them open: one the forge keeps refusing never resolves.
const RESOLVE_FAILURES: u32 = 3;

/// What resolving the threads a fix answered came to
pub(super) enum Resolved {
    /// Every thread sent is resolved, or none was sent
    Done,
    /// The forge refused for this reason, so the step is tried again
    Retry(String),
    /// The forge kept refusing, so the review goes on with these open
    LeftOpen {
        /// The forge's ids of the threads still open
        threads: Vec<String>,
        /// Why the last try failed
        reason: String,
    },
}

impl Runner {
    /// Starts `chosen`'s round, a review bot's, on the pull request's head
    pub(super) fn bot_round(
        &mut self,
        chosen: &ListedReviewer,
        bot: BotReviewer,
        review: Review,
    ) -> Result<Begin, StateError> {
        let head = match self.origin_head() {
            Ok(head) => head,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let reviewer = Some(chosen.name.clone());
        let stage = ReviewStage::Summon {
            bot: bot.bot,
            started: self.ports.clock.now(),
            head,
            readied: None,
            full: false,
        };
        self.update(|item| {
            // A bot reads the pull request, not the worktree.
            item.phase = Phase::Review(Review {
                stage,
                reviewer,
                failures: 0,
                reading: None,
                ..review
            });
        })?;
        self.bot_step()
    }

    /// Steps a review bot's round, waiting to summon or waiting for its review
    pub(super) fn bot_step(&mut self) -> Result<Begin, StateError> {
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
                return self.raise(number, Stuck::Closed.into());
            }
            Err(e) => return Ok(self.gate_failed(format!("cannot read #{number}: {e}"))),
        }
        let stage = self.pass().stage;
        let (ReviewStage::Summon { bot, .. }
        | ReviewStage::Summoned { bot, .. }
        | ReviewStage::Settling { bot, .. }) = stage
        else {
            unreachable!("bot_step only runs a review bot's summon")
        };
        // Nothing summons a bot the list no longer names: no check of the
        // project's own holds for it.
        if !self.listed_bots().iter().any(|listed| listed.bot == bot) {
            self.release(bot)?;
            if let Err(reason) = self.label(bot, number, false) {
                return Ok(self.gate_failed(reason));
            }
            let reviewer = self.round_reviewer(bot);
            return self.pass_over(number, BotSkipped::Unlisted { reviewer });
        }
        match stage {
            ReviewStage::Summon {
                started,
                head,
                readied,
                full,
                ..
            } => self.summon(bot, started, head, readied, full),
            ReviewStage::Summoned {
                started,
                head,
                at,
                full,
                resent,
                ..
            } => self.await_review(bot, started, head, at, full, resent),
            ReviewStage::Settling {
                since, read, open, ..
            } => self.threads_settling(bot, since, read, &open),
            _ => unreachable!("bot_step only runs a review bot's summon"),
        }
    }

    // No summon without the lease, and none for a head already reviewed:
    // that one costs the window and buys nothing. A bot skips a draft, so a
    // draft is marked ready first and the summon waits for the next step:
    // the forge can show the old state for a few seconds after. `full` asks
    // for a full review whatever the bot read before. A bot that reviews a
    // draft when it is marked ready is summoned by marking it.
    fn summon(
        &mut self,
        bot: Bot,
        started: Timestamp,
        head: String,
        readied: Option<Timestamp>,
        full: bool,
    ) -> Result<Begin, StateError> {
        let number = self.number();
        let activity = match self.activity(bot, number) {
            Ok(activity) => activity,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        if self.lands_unsummoned(bot, &head, &activity) {
            self.release(bot)?;
            self.update(|item| item.reviewed(head))?;
            return self.settle_threads(bot, &activity);
        }
        let now = self.ports.clock.now();
        let lease = self.profile(bot).lease();
        let window = self.bot_reviewer(bot).window;
        if let Some(opens) = self.ports.leases.opens(&lease, window, now)
            && opens.0 > now.0.saturating_add(FAR)
        {
            self.release(bot)?;
            let reviewer = self.round_reviewer(bot);
            return self.pass_over(number, BotSkipped::Window { reviewer, opens });
        }
        // A lease the dog never grants, or a book it cannot read, holds the
        // pass no longer than a bot that never answers does.
        if now.0.saturating_sub(started.0) >= REVIEW_WAIT {
            self.release(bot)?;
            let reviewer = self.round_reviewer(bot);
            let since = started;
            return self.pass_over(number, BotSkipped::Waited { reviewer, since });
        }
        let at = (started, &head[..]);
        if let Some(begin) = self.summon_by_ready(bot, at, readied, full)? {
            return Ok(begin);
        }
        if let Some(begin) = self.ready_for_review(number, bot, at, readied, full)? {
            return Ok(begin);
        }
        if !self.lease_granted(bot) {
            return Ok(Begin::Idle);
        }
        if let Some(begin) = self.public_for(bot, number)? {
            return Ok(begin);
        }
        self.hold(bot, now)?;
        let full = self.asks_full(bot, full, &activity);
        self.send(bot, number, (started, head), now, full, false)
    }

    // CodeRabbit's free plan reviews public repos only, so it is asked
    // just before each of its summons, which a repo made private since the
    // runner started would spend for nothing. `None` lets the summon go on.
    fn public_for(&mut self, bot: Bot, number: u64) -> Result<Option<Begin>, StateError> {
        if bot != Bot::Coderabbit {
            return Ok(None);
        }
        match self.ports.forge.visibility(&self.settings.forge) {
            Ok(Visibility::Public) => Ok(None),
            Ok(Visibility::Private | Visibility::Internal) => {
                self.release(bot)?;
                if let Err(reason) = self.label(bot, number, false) {
                    return Ok(Some(self.gate_failed(reason)));
                }
                let reviewer = self.round_reviewer(bot);
                self.pass_over(number, BotSkipped::NotPublic { reviewer })
                    .map(Some)
            }
            Err(e) => {
                let repo = self.settings.forge.as_str();
                Ok(Some(self.gate_failed(format!(
                    "cannot read whether {repo} is public: {e}"
                ))))
            }
        }
    }

    // Whether `bot` already reviewed `head`, so the round needs no summon.
    // A review from before an adoption does not stand for one of kelpie's.
    fn lands_unsummoned(&self, bot: Bot, head: &str, activity: &Activity) -> bool {
        let owed = self.item().summons_owed.contains(&bot);
        !owed && self.profile(bot).covers(activity, head)
    }

    // Whether a summon asks for a full review by comment: when it was told
    // to, when the bot has no label, or when the bot read the pull request
    // before and the label would find nothing new, because a summon is owed
    // or kelpie caught the branch up. A bot with no such comment always gets
    // the label.
    fn asks_full(&self, bot: Bot, full: bool, activity: &Activity) -> bool {
        let (item, profile) = (self.item(), self.profile(bot));
        let read_before = profile.reviewed_besides(activity, "") > 0;
        let owed = item.summons_owed.contains(&bot);
        let wanted = full || profile.label().is_none() || (read_before && (owed || item.rebased));
        wanted && profile.full_review().is_some()
    }

    // Sends the summon, under the lease, in whichever form `full` says.
    // `again` is a re-send of a summon the bot gave no sign of: it goes
    // out as a fresh event, and `at` stays the first one's time, so the
    // round is the same and its window is not counted twice.
    fn send(
        &mut self,
        bot: Bot,
        number: u64,
        (started, head): (Timestamp, String),
        at: Timestamp,
        full: bool,
        again: bool,
    ) -> Result<Begin, StateError> {
        if full {
            return self.ask_full(bot, number, (started, head), at, again);
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
        self.set_stage(ReviewStage::Summoned {
            bot,
            started,
            head: head.clone(),
            at,
            full: false,
            resent: again,
        })?;
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
        (started, head): (Timestamp, String),
        at: Timestamp,
        again: bool,
    ) -> Result<Begin, StateError> {
        if let Err(reason) = self.label(bot, number, false) {
            return Ok(self.gate_failed(reason));
        }
        self.set_stage(ReviewStage::Summoned {
            bot,
            started,
            head: head.clone(),
            at,
            full: true,
            resent: again,
        })?;
        let profile = self.profile(bot);
        let body = profile.full_review().unwrap_or_default();
        let posted = self.ports.forge.comment(&self.settings.forge, number, body);
        if let Err(e) = posted {
            let stage = if again {
                ReviewStage::Summoned {
                    bot,
                    started,
                    head,
                    at,
                    full: true,
                    resent: false,
                }
            } else {
                ReviewStage::Summon {
                    bot,
                    started,
                    head,
                    readied: None,
                    full: true,
                }
            };
            self.set_stage(stage)?;
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
        bot: Bot,
        (started, head): (Timestamp, &str),
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
        self.update(|item| item.known.ready = true)?;
        self.set_stage(ReviewStage::Summon {
            bot,
            started,
            head: head.to_owned(),
            readied: Some(now),
            full,
        })?;
        Ok(Some(Begin::Report(StepReport::MarkedReady {
            issue: self.item().issue,
            pull_request: number,
        })))
    }

    fn await_review(
        &mut self,
        bot: Bot,
        started: Timestamp,
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
        // A summon by marking ready is never sent again: a comment on top
        // would spend a second review.
        let resends = !resent && !self.bot_reviewer(bot).reviews_on_ready;
        // A head reviewed before an adoption needs a review of kelpie's own.
        let owed = self.item().summons_owed.contains(&bot);
        let activity = if owed { activity.since(at) } else { activity };
        match profile.read(&activity, &head, at) {
            Reading::Reviewed => self.answered(bot, number, &activity, (at, &head)),
            Reading::Completed { at: done } if now.saturating_sub(done.0) < DONE_SETTLE => {
                self.accepted(bot, at).map(|()| Begin::Idle)
            }
            // A bot with no full review has nothing more to ask for.
            Reading::Completed { .. } if !owed || profile.full_review().is_none() => {
                self.answered(bot, number, &activity, (at, &head))
            }
            // An owed summon found nothing new: ask once more, for a full review.
            Reading::Completed { .. } if !full => {
                self.accepted(bot, at)?;
                if let Err(reason) = self.label(bot, number, false) {
                    return Ok(self.gate_failed(reason));
                }
                // The head marked done is an answer: the wait to summon
                // for a full review starts afresh.
                let started = Timestamp(now);
                self.set_stage(ReviewStage::Summon {
                    bot,
                    started,
                    head: head.clone(),
                    readied: None,
                    full: true,
                })?;
                self.summon(bot, started, head, None, true)
            }
            // The dog holds the bot until its window opens, and the round
            // asks again, unless that is too far on to wait for.
            Reading::Refused { opens } => {
                let opens = opens.unwrap_or_else(|| self.parked_until(bot, Timestamp(now)));
                let kind = profile.lease();
                self.ports.leases.window(&kind, WindowFact::Opens, opens.0);
                self.release(bot)?;
                if let Err(reason) = self.label(bot, number, false) {
                    return Ok(self.gate_failed(reason));
                }
                if opens.0 > now.saturating_add(FAR) {
                    let reviewer = self.round_reviewer(bot);
                    return self.pass_over(number, BotSkipped::Window { reviewer, opens });
                }
                // A bot with no label is always asked by comment. The wait
                // to summon runs on from the round's start, so refusals
                // inside the hour cannot hold the pass for good.
                self.set_stage(ReviewStage::Summon {
                    bot,
                    started,
                    head,
                    readied: None,
                    full: full && profile.label().is_some(),
                })?;
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
                let reviewer = self.round_reviewer(bot);
                self.pass_over(number, BotSkipped::Silent { reviewer, head })
            }
            Reading::Processing | Reading::Completed { .. } => {
                self.accepted(bot, at).map(|()| Begin::Idle)
            }
            // No sign of the summon yet: it may never have been seen, so it
            // goes out once more before the lease is counted spent.
            Reading::Silent if resends && !heard => {
                if waited < HEARD_WAIT {
                    Ok(Begin::Idle)
                } else {
                    self.resend(bot, (started, head), at, full_again)
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
        round: (Timestamp, String),
        at: Timestamp,
        full: bool,
    ) -> Result<Begin, StateError> {
        let number = self.number();
        if !self.holds_own(bot) {
            return self.accepted(bot, at).map(|()| Begin::Idle);
        }
        if let Some(begin) = self.public_for(bot, number)? {
            return Ok(begin);
        }
        self.send(bot, number, round, at, full, true)
    }

    // A summon answered at `at`, by a review of `head` or by the bot finding
    // nothing new in it, which is a clean read of it. It settles a summon
    // owed since an adoption.
    fn answered(
        &mut self,
        bot: Bot,
        number: u64,
        activity: &Activity,
        (at, head): (Timestamp, &str),
    ) -> Result<Begin, StateError> {
        self.accepted(bot, at)?;
        self.update(|item| {
            item.summons_owed.remove(&bot);
            item.reviewed(head.to_owned());
        })?;
        if let Err(reason) = self.label(bot, number, false) {
            return Ok(self.gate_failed(reason));
        }
        self.settle_threads(bot, activity)
    }

    // The bot read the head, which also reads whatever a catch-up with
    // `main` brought, so this pass has been read. Its open threads are the
    // round's findings; with none, the pass goes on to the next reviewer.
    // A bot that read the pull request before has its nits on a head a
    // nit-only fix pushed left open, so it cannot loop on nits.
    fn review_landed(
        &mut self,
        number: u64,
        bot: Bot,
        activity: &Activity,
    ) -> Result<Begin, StateError> {
        let profile = self.profile(bot);
        let nits_left = self.nits_left_open(bot);
        // The report counts every open thread; the findings leave out the nits left open.
        let open_threads = activity.open_threads().count();
        let (threads, findings): (Vec<String>, Vec<Finding>) = activity
            .open_threads()
            .map(|t| (t.id.clone(), profile.finding(t)))
            .filter(|(_, f)| !(nits_left && f.is_nit()))
            .unzip();
        let review = Review {
            unread: false,
            failures: 0,
            ..self.pass()
        };
        let (round, reviewer) = (review.round, self.round_reviewer(bot));
        let next = match threads.len() {
            0 => self.after_round(review, self.ports.clock.now()),
            _ => Phase::Review(Review {
                stage: ReviewStage::Found { findings, threads },
                ..review
            }),
        };
        self.update(|item| {
            item.rebased = false;
            item.bots_after_ci = false;
            item.unreviewed = None;
            *item.bot_reads.entry(bot).or_default() += 1;
            item.counts.review_rounds = item.counts.review_rounds.saturating_add(1);
            item.phase = next;
        })?;
        Ok(Begin::Report(StepReport::BotReviewed {
            issue: self.item().issue,
            pull_request: number,
            round,
            reviewer,
            open_threads,
        }))
    }

    // Whether `bot` read the pull request before and the head on `origin`
    // is one a nit-only fix pushed. Git that cannot say counts as no, which
    // sends the nits as any round's.
    fn nits_left_open(&self, bot: Bot) -> bool {
        let item = self.item();
        let read_before = item.bot_reads.get(&bot).is_some_and(|reads| *reads > 0);
        if !read_before || item.nit_fix_heads.is_empty() {
            return false;
        }
        match self.origin_head() {
            Ok(head) => item.nit_fix_heads.contains(&head),
            Err(e) => {
                let name = bot.name();
                eprintln!(
                    "cannot read issue #{}'s head on origin, so {name}'s nits are sent: {e}",
                    item.issue
                );
                false
            }
        }
    }

    // The pass goes on without the bot, and says why. A pass no other
    // reviewer reads is marked unreviewed for it as it ends.
    fn pass_over(&mut self, number: u64, skipped: BotSkipped) -> Result<Begin, StateError> {
        let review = self.pass();
        let round = review.round;
        let next = self.after_round(review, self.ports.clock.now());
        let reviewer = skipped.reviewer().clone();
        let reason = skipped.why();
        self.update(|item| {
            item.bots_skipped
                .retain(|s| s.reviewer() != skipped.reviewer());
            item.bots_skipped.push(skipped);
            item.phase = next;
        })?;
        Ok(Begin::Report(StepReport::ReviewerSkipped {
            issue: self.item().issue,
            pull_request: number,
            round,
            reviewer,
            reason,
        }))
    }

    /// Resolves the bot threads sent to the worker, once its fix on pull
    /// request `number` moved the head
    ///
    /// Resolving is safe to repeat, so a failure part way retries it all.
    pub(super) fn resolve_sent(&mut self, number: u64) -> Result<Resolved, StateError> {
        let mut left = self.item().threads_sent.clone();
        if left.is_empty() {
            return Ok(Resolved::Done);
        }
        let mut failed = None;
        for id in self.item().threads_sent.clone() {
            match self.ports.forge.resolve_thread(&self.settings.forge, &id) {
                Ok(()) => left.retain(|t| t != &id),
                Err(e) => {
                    failed = Some(format!("cannot resolve a thread on #{number}: {e}"));
                    break;
                }
            }
        }
        let failures = self.item().resolve_failures + 1;
        match failed {
            Some(reason) if failures < RESOLVE_FAILURES => {
                self.update(|item| {
                    item.threads_sent.clone_from(&left);
                    item.resolve_failures = failures;
                })?;
                Ok(Resolved::Retry(reason))
            }
            // Open still, they go to the worker again with the bot's next review.
            Some(reason) => {
                self.update(WorkItem::forget_threads)?;
                Ok(Resolved::LeftOpen {
                    threads: left,
                    reason,
                })
            }
            None => {
                self.update(WorkItem::forget_threads)?;
                Ok(Resolved::Done)
            }
        }
    }

    /// Takes `review`'s bot's label off before its threads go to the fix
    /// turn, where someone left it on: a push with it on summons the bot
    /// outside its lease
    ///
    /// # Errors
    ///
    /// A message when the forge cannot read or change the label.
    pub(super) fn bot_label_off(&mut self, review: &Review) -> Result<(), String> {
        let listed = review.reviewer.as_ref().and_then(|name| self.listed(name));
        let Some(bot) = listed.and_then(|r| r.bot()) else {
            return Ok(());
        };
        let number = self.number();
        self.label(bot.bot, number, false)
    }

    /// Gives back the lease of every bot the work item may hold and takes
    /// each one's label off, when it leaves a bot's round for good
    pub(super) fn leave_round(&mut self) {
        let Some(number) = self.current().and_then(|i| i.pull_request) else {
            return;
        };
        for bot in self.round_bots() {
            let _ = self.release(bot);
            let _ = self.label(bot, number, false);
        }
        let _ = self.update(WorkItem::forget_threads);
    }

    // `bot` as the project's list defines it, or kelpie's own file when the
    // list no longer names it.
    fn bot_reviewer(&self, bot: Bot) -> BotReviewer {
        let listed = self.lineup.iter().filter_map(ListedReviewer::bot);
        let filed = self.book.get(&AgentName::kelpies(bot.as_str()));
        listed
            .chain(filed.and_then(|agent| agent.runs.bot()))
            .find(|defined| defined.bot == bot)
            .unwrap_or(BotReviewer {
                bot,
                window: ReviewWindow::HOURLY,
                reviews_on_ready: false,
                rounds: None,
            })
    }

    // Who the round is, as the pass records it, or the bot's own file's name.
    fn round_reviewer(&self, bot: Bot) -> AgentName {
        let named = self.pass().reviewer;
        named.unwrap_or_else(|| AgentName::kelpies(bot.as_str()))
    }

    // The review the work item is in.
    fn pass(&self) -> Review {
        match &self.item().phase {
            Phase::Review(review) => review.clone(),
            _ => unreachable!("a review bot's round is in the review"),
        }
    }

    fn set_stage(&mut self, stage: ReviewStage) -> Result<(), StateError> {
        self.update(|item| {
            if let Phase::Review(review) = &mut item.phase {
                review.stage = stage;
            }
        })
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

#[cfg(test)]
mod codex_bot;
#[cfg(test)]
mod nit_fixes;
#[cfg(test)]
mod owed;
#[cfg(test)]
mod settling;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod two_bots;
