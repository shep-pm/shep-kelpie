//! A bot that reviews a pull request when it leaves draft
//!
//! Codex does, where the repo's Codex settings say so, and a comment on top
//! would spend a second review that the lease book counts as one. So for it
//! marking a draft ready is the summon: taken under the lease, with no comment.

use super::super::Runner;
use super::super::report::Begin;
use crate::ports::Timestamp;
use crate::review_bot::Bot;
use crate::state::StateError;
use crate::work_item::ReviewStage;

// When the round began, and the head it summons for.
type Round<'a> = (Timestamp, &'a str);

impl Runner {
    /// Marks a draft ready as `bot`'s summon, when its file says it reviews
    /// on ready, once the dog grants its lease
    ///
    /// `None` leaves the round to the usual path: a bot that does not review
    /// on ready, a pull request already ready, or one kelpie already marked.
    pub(super) fn summon_by_ready(
        &mut self,
        bot: Bot,
        (started, head): Round<'_>,
        readied: Option<Timestamp>,
        full: bool,
    ) -> Result<Option<Begin>, StateError> {
        if readied.is_some() || !self.bot_reviewer(bot).reviews_on_ready {
            return Ok(None);
        }
        let number = self.number();
        let repo = self.settings.forge.clone();
        // A pull request that cannot be read is left to the step after, which says so.
        let draft = self
            .ports
            .forge
            .pull_request(&repo, number)
            .is_ok_and(|pr| pr.draft);
        if !draft {
            return Ok(None);
        }
        // Marking it ready without the lease would draw a review outside it.
        if !self.lease_granted(bot) {
            return Ok(Some(Begin::Idle));
        }
        // Saved before the pull request is marked, so a restart never marks it twice.
        let now = self.ports.clock.now();
        self.hold(bot, now)?;
        let head = head.to_owned();
        self.set_stage(ReviewStage::Summoned {
            bot,
            started,
            head: head.clone(),
            at: now,
            full: true,
            resent: false,
        })?;
        if let Err(e) = self.ports.forge.mark_ready(&repo, number) {
            self.set_stage(ReviewStage::Summon {
                bot,
                started,
                head,
                readied: None,
                full,
            })?;
            self.release(bot)?;
            let reason = format!("cannot mark #{number} ready: {e}");
            return Ok(Some(self.gate_failed(reason)));
        }
        self.update(|item| item.known.ready = true)?;
        Ok(Some(self.summoned(number, head, false)))
    }
}
