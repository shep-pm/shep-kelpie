//! Which listed bot takes a round, and the leases on their windows
//!
//! A round waiting to summon asks for the lease of every bot the project
//! lists, and summons the first listed whose lease the dog granted. The
//! asks it did not take go back, unless another work item waits on them.

use super::super::Runner;
use crate::lease::wire::WindowFact;
use crate::ports::Timestamp;
use crate::review_bot::{Bot, ReviewWindow};
use crate::state::{LeaseHeld, StateError};
use crate::work_item::{CodeRabbitStage, Phase};

impl Runner {
    /// Asks for every listed bot's lease and returns the first listed one
    /// this work item may summon, if the dog granted any
    ///
    /// A grant another work item holds is not this one's to use.
    pub(super) fn choose_bot(&mut self) -> Result<Option<Bot>, StateError> {
        let listed = self.settings.reviewers();
        for bot in &listed {
            self.ports.leases.want(&self.profile(*bot).lease());
        }
        let granted = |bot: &Bot| {
            self.ports.leases.holds(&self.profile(*bot).lease()) && !self.lease_held_elsewhere(*bot)
        };
        let Some(chosen) = listed.iter().copied().find(granted) else {
            return Ok(None);
        };
        for bot in listed.into_iter().filter(|b| *b != chosen) {
            self.release(bot)?;
        }
        Ok(Some(chosen))
    }

    // What a summon's answer tells the dog: the window runs from `at`.
    pub(super) fn accepted(&mut self, bot: Bot, at: Timestamp) -> Result<(), StateError> {
        let kind = self.profile(bot).lease();
        self.ports.leases.window(&kind, WindowFact::Summoned, at.0);
        self.release(bot)
    }

    pub(super) fn hold(&mut self, bot: Bot, since: Timestamp) -> Result<(), StateError> {
        let mut next = self.state.clone();
        next.leases.retain(|l| l.resource != bot.resource());
        next.leases.push(LeaseHeld {
            resource: bot.resource(),
            issue: Some(self.item().issue),
            since,
        });
        self.save(next)
    }

    /// Gives `bot`'s lease back when this work item holds it, or when no
    /// other item holds it or waits for it
    ///
    /// The ask and the grant are the project's, not an item's: giving them
    /// back for an item that never took the lease would cancel a sibling's
    /// place in the dog's queue, or a grant it has yet to take up.
    pub(super) fn release(&mut self, bot: Bot) -> Result<(), StateError> {
        let this = self.current().map(|item| item.issue);
        let mine = |l: &LeaseHeld| l.resource == bot.resource() && l.issue == this;
        let held = self.state.leases.iter().any(mine);
        if !held && (self.lease_held_elsewhere(bot) || self.lease_wanted_elsewhere()) {
            return Ok(());
        }
        self.ports.leases.give_back(&self.profile(bot).lease());
        if !held {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.leases.retain(|l| !mine(l));
        self.save(next)
    }

    /// Gives back every listed bot's lease, as [`Runner::release`] does each
    pub(in crate::runner) fn release_all(&mut self) -> Result<(), StateError> {
        for bot in self.settings.reviewers() {
            self.release(bot)?;
        }
        Ok(())
    }

    // Whether this work item holds `bot`'s lease under its own row
    pub(super) fn holds_own(&self, bot: Bot) -> bool {
        let issue = self.item().issue;
        let mine = |l: &LeaseHeld| l.resource == bot.resource() && l.issue == Some(issue);
        self.state.leases.iter().any(mine)
    }

    // Whether a work item other than this one waits in a round for a lease
    fn lease_wanted_elsewhere(&self) -> bool {
        let this = self.current().map(|item| item.issue);
        self.state.work_items.iter().any(|item| {
            Some(item.issue) != this
                && matches!(item.phase, Phase::CodeRabbit(CodeRabbitStage::Lease { .. }))
        })
    }

    // Whether a work item other than this one holds `bot`'s lease
    fn lease_held_elsewhere(&self, bot: Bot) -> bool {
        let this = self.current().map(|item| item.issue);
        self.state
            .leases
            .iter()
            .any(|l| l.resource == bot.resource() && l.issue != this)
    }

    // The window a refusal that named no opening is parked for: its span
    // from `now`, as the bot's definition gives it.
    pub(super) fn parked_until(&self, bot: Bot, now: Timestamp) -> Timestamp {
        let span = self
            .reviewers
            .window(bot)
            .unwrap_or(ReviewWindow::HOURLY)
            .seconds();
        Timestamp(now.0.saturating_add(span))
    }
}
