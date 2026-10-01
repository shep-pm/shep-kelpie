//! A bot that reviews a pull request when it leaves draft
//!
//! Codex does, where the repo's Codex settings say so, and a comment on top
//! would spend a second review that the lease book counts as one. So for it
//! marking a draft ready is the summon: taken under the lease, with no comment.

use super::super::Runner;
use super::super::report::Begin;
use crate::ports::Timestamp;
use crate::state::StateError;
use crate::work_item::{CodeRabbitStage, Phase};

impl Runner {
    /// Marks a draft ready as the summon of the bot that takes the round, when
    /// that bot reviews on ready, and so the lease it holds is the review's
    ///
    /// `None` leaves the round to the usual path: a pull request already
    /// ready, one kelpie already marked, a round that goes to another bot
    /// (whose own mark-ready then draws a review outside any lease, which the
    /// repo's Codex settings are the only cure for), or none at all.
    pub(super) fn summon_by_ready(
        &mut self,
        head: &str,
        readied: Option<Timestamp>,
        full: bool,
    ) -> Result<Option<Begin>, StateError> {
        let listed = self.settings.reviewers();
        if readied.is_some() || !listed.iter().any(|b| self.reviewers.ready_summons(*b)) {
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
        let Some(bot) = self.choose_bot()? else {
            return Ok(None);
        };
        if !self.reviewers.ready_summons(bot) {
            return Ok(None);
        }
        // Saved before the pull request is marked, so a restart never marks it twice.
        let now = self.ports.clock.now();
        self.hold(bot, now)?;
        let head = head.to_owned();
        let stage = CodeRabbitStage::Summoned {
            bot,
            head: head.clone(),
            at: now,
            full: true,
            resent: false,
        };
        self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
        if let Err(e) = self.ports.forge.mark_ready(&repo, number) {
            let stage = CodeRabbitStage::Lease {
                head,
                readied: None,
                full,
            };
            self.update(|item| item.phase = Phase::CodeRabbit(stage))?;
            let reason = format!("cannot mark #{number} ready: {e}");
            return Ok(Some(self.gate_failed(reason)));
        }
        self.update(|item| item.known.ready = true)?;
        Ok(Some(self.summoned(number, head, false)))
    }
}
