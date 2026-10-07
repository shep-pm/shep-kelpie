//! A review bot's threads, read until they settle
//!
//! The forge can list a bot's review before the threads posted with it. So
//! the round reads the threads again a little later, until two reads agree.
//! A bot the pass went on without may review the head anyway. Its review
//! then gets a round of its own before the next reviewer's or the pass's
//! end. The merge ruling names any listed bot's threads nothing addressed.

use super::super::Runner;
use super::super::report::Begin;
use crate::ports::{Severity, Timestamp};
use crate::review_bot::{Activity, Bot};
use crate::state::StateError;
use crate::work_item::{Phase, ReviewStage};

// CodeRabbit's two threads on shep#703 were missing two seconds after its
// review. Reads of the threads are this far apart.
pub(super) const THREAD_SETTLE: u64 = 20;

// Two reads that agree land the review only once it was seen this long ago.
pub(crate) const SETTLE_LEAST: u64 = 60;

// Threads still changing this long after the review are taken as they stand.
pub(super) const SETTLE_MOST: u64 = 120;

impl Runner {
    // A review covers the head: its threads are read again THREAD_SETTLE on.
    pub(super) fn settle_threads(
        &mut self,
        bot: Bot,
        activity: &Activity,
    ) -> Result<Begin, StateError> {
        let now = self.ports.clock.now();
        self.set_stage(ReviewStage::Settling {
            bot,
            since: now,
            read: now,
            open: open_ids(activity),
        })?;
        Ok(Begin::Idle)
    }

    // Reads the threads again, and lands the review once two reads agree
    // and SETTLE_LEAST has passed, or once SETTLE_MOST has.
    pub(super) fn threads_settling(
        &mut self,
        bot: Bot,
        since: Timestamp,
        read: Timestamp,
        open: &[String],
    ) -> Result<Begin, StateError> {
        let now = self.ports.clock.now();
        if now.0.saturating_sub(read.0) < THREAD_SETTLE {
            return Ok(Begin::Idle);
        }
        let number = self.number();
        let activity = match self.activity(bot, number) {
            Ok(activity) => activity,
            Err(reason) => return Ok(self.gate_failed(reason)),
        };
        let threads = open_ids(&activity);
        let age = now.0.saturating_sub(since.0);
        if (threads == open && age >= SETTLE_LEAST) || age >= SETTLE_MOST {
            return self.review_landed(number, bot, &activity);
        }
        self.set_stage(ReviewStage::Settling {
            bot,
            since,
            read: now,
            open: threads,
        })?;
        Ok(Begin::Idle)
    }

    /// Starts a round for a bot this pass went on without that has since
    /// reviewed the head anyway, whose threads then settle as any review's
    ///
    /// `None` when no such bot has, and the round goes to the next reviewer.
    pub(in crate::runner) fn late_review(&mut self) -> Result<Option<Begin>, StateError> {
        let skipped = self.item().bots_skipped.clone();
        if skipped.is_empty() {
            return Ok(None);
        }
        let head = match self.origin_head() {
            Ok(head) => head,
            Err(reason) => return Ok(Some(self.gate_failed(reason))),
        };
        let number = self.number();
        for reviewer in skipped.iter().map(|s| s.reviewer().clone()) {
            let listed = self.listed(&reviewer).and_then(|r| r.bot());
            // A review from before an adoption does not stand for one of kelpie's.
            let Some(bot) = listed
                .map(|b| b.bot)
                .filter(|bot| !self.item().summons_owed.contains(bot))
            else {
                continue;
            };
            let activity = match self.activity(bot, number) {
                Ok(activity) => activity,
                Err(reason) => return Ok(Some(self.gate_failed(reason))),
            };
            if !self.profile(bot).covers(&activity, &head) {
                continue;
            }
            let open = open_ids(&activity);
            let now = self.ports.clock.now();
            let read = head.clone();
            self.update(|item| {
                item.reviewed(read);
                item.bots_skipped.retain(|s| s.reviewer() != &reviewer);
                if let Phase::Review(review) = &mut item.phase {
                    review.reviewer = Some(reviewer.clone());
                    review.stage = ReviewStage::Settling {
                        bot,
                        since: now,
                        read: now,
                        open,
                    };
                }
            })?;
            return Ok(Some(Begin::Idle));
        }
        Ok(None)
    }

    /// The listed review bots' unaddressed threads on pull request
    /// `number`, as the merge ruling names them, or `None` when there are none
    ///
    /// A thread is unaddressed when it is open, not outdated, not sent to a
    /// fix turn awaiting it, and above a nit: a nit never holds a merge.
    ///
    /// # Errors
    ///
    /// A message when the forge cannot read a bot's activity.
    pub(in crate::runner) fn threads_open(&self, number: u64) -> Result<Option<String>, String> {
        let sent = self.current().map(|item| item.threads_sent.clone());
        let sent = sent.unwrap_or_default();
        let mut named = Vec::new();
        for listed in self.listed_bots() {
            let profile = self.profile(listed.bot);
            let activity = self.activity(listed.bot, number)?;
            let open = activity
                .open_threads()
                .filter(|t| !sent.contains(&t.id))
                .filter(|t| profile.finding(t).severity > Severity::Low)
                .count();
            if open > 0 {
                named.push(format!("{open} from {}", profile.name()));
            }
        }
        Ok((!named.is_empty()).then(|| named.join(", ")))
    }
}

// The ids of the bot's open threads, sorted so two reads compare as sets.
fn open_ids(activity: &Activity) -> Vec<String> {
    let mut ids: Vec<String> = activity.open_threads().map(|t| t.id.clone()).collect();
    ids.sort();
    ids
}
