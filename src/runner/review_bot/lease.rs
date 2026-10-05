//! The leases on the review bots' windows
//!
//! A bot's round asks the dog for its lease, and summons once the dog grants
//! it. The ask goes back once the bot answers, unless another work item
//! waits on it.

use super::super::Runner;
use crate::lease::wire::WindowFact;
use crate::ports::Timestamp;
use crate::review_bot::Bot;
use crate::state::{LeaseHeld, StateError};
use crate::work_item::{Phase, Review, ReviewStage};

impl Runner {
    /// Asks for `bot`'s lease, and says whether the dog granted it to this
    /// work item: a grant another work item holds is not this one's to use
    pub(super) fn lease_granted(&self, bot: Bot) -> bool {
        let lease = self.profile(bot).lease();
        self.ports.leases.want(&lease);
        self.ports.leases.holds(&lease) && !self.lease_held_elsewhere(bot)
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
        if !held && (self.lease_held_elsewhere(bot) || self.lease_wanted_elsewhere(bot)) {
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

    /// Gives back the lease of every bot [`Runner::round_bots`] names, as
    /// [`Runner::release`] does each
    pub(in crate::runner) fn release_all(&mut self) -> Result<(), StateError> {
        for bot in self.round_bots() {
            self.release(bot)?;
        }
        Ok(())
    }

    /// Every listed bot, the one the work item's round names, and any it
    /// holds a lease row for, which a list changed mid-round leaves out
    pub(super) fn round_bots(&self) -> Vec<Bot> {
        let mut bots: Vec<Bot> = self.listed_bots().iter().map(|b| b.bot).collect();
        let Some(item) = self.current() else {
            return bots;
        };
        let own = |l: &&LeaseHeld| l.issue == Some(item.issue);
        let rows = self.state.leases.iter().filter(own);
        for bot in summoning(&item.phase)
            .into_iter()
            .chain(rows.filter_map(|l| Bot::of(l.resource)))
        {
            if !bots.contains(&bot) {
                bots.push(bot);
            }
        }
        bots
    }

    // Whether this work item holds `bot`'s lease under its own row
    pub(super) fn holds_own(&self, bot: Bot) -> bool {
        let issue = self.item().issue;
        let mine = |l: &LeaseHeld| l.resource == bot.resource() && l.issue == Some(issue);
        self.state.leases.iter().any(mine)
    }

    // Whether a work item other than this one waits to summon `bot`
    fn lease_wanted_elsewhere(&self, bot: Bot) -> bool {
        let this = self.current().map(|item| item.issue);
        self.state.work_items.iter().any(|item| {
            let waits = matches!(
                &item.phase,
                Phase::Review(Review {
                    stage: ReviewStage::Summon { bot: theirs, .. },
                    ..
                }) if *theirs == bot
            );
            Some(item.issue) != this && waits
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

    // The window a refusal that named no opening holds the bot for: its span
    // from `now`, as the bot's file gives it.
    pub(super) fn parked_until(&self, bot: Bot, now: Timestamp) -> Timestamp {
        let span = self.bot_reviewer(bot).window.seconds();
        Timestamp(now.0.saturating_add(span))
    }
}

// The bot whose round `phase` is in, waiting to summon or summoned.
fn summoning(phase: &Phase) -> Option<Bot> {
    match phase {
        Phase::Review(Review {
            stage: ReviewStage::Summon { bot, .. } | ReviewStage::Summoned { bot, .. },
            ..
        }) => Some(*bot),
        _ => None,
    }
}
