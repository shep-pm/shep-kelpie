//! A head no reviewer read goes back to a reviewer, not to a ruling
//!
//! On a project whose project manager rules on its items' open review threads,
//! the merge ruling is the last resort: a head no round read, and a fix no
//! reviewer read, go back through a pass of the review instead. A pass cut
//! short by a reviewer down waits at the gate for that reviewer to come back,
//! and one new pass starts each [`READ_AGAIN`] until one reads the head. A
//! project with no `agents.pm`, and an adopted contribution, keep the warned
//! merge ruling, which names what it warns of.

use crate::runner::Runner;
use crate::runner::pm::ReadWait;
use crate::runner::report::Begin;
use crate::runner::turn::held_up::Reason;
use crate::state::StateError;
use crate::work_item::{Phase, Review};

// A pass whose reviewers are all down costs no call, so sending the item
// straight back would start pass after pass for nothing, and no budget would
// ever stop it. Kelpie waits this out first, as long as the project manager's
// first wait on an idle call.
pub(in crate::runner) const READ_AGAIN: u64 = 30 * 60;

/// What the gate finds of `head` that only a pass of the review clears
#[derive(Debug, Default)]
pub(super) struct Unread {
    /// Why the last pass read less than it was planned for, when it did
    pub(super) unreviewed: Option<String>,
    /// No round read this head, and no fix turn kelpie sent pushed it
    pub(super) unread_head: bool,
    /// This head is the worker's fix for a merge ruling's `no` or a comment
    pub(super) note_fix: bool,
    /// This head is the worker's fix for a review bot's late review
    pub(super) late_fix: bool,
}

impl Unread {
    // Whether any of these holds the merge
    fn holds(&self) -> bool {
        self.unreviewed.is_some() || self.unread_head || self.note_fix || self.late_fix
    }
}

/// What the gate makes of them
pub(super) enum Back {
    /// The gate may go on, with what it found, for the ruling it raises where
    /// it sends nothing back: with no project manager, or on a contribution
    On(Unread),
    /// The item went back through a pass of the review, or waits for a reviewer
    Done(Box<Begin>),
}

impl Runner {
    /// Whether `head` goes back through a pass of the review instead of a merge
    /// ruling, on a project whose project manager rules on its items' threads
    ///
    /// A pass that ended short of its reads waits rather than going back at once:
    /// its reviewers are down, so a new pass would read nothing. Each
    /// [`READ_AGAIN`] that ends unread starts one, the first waking the project
    /// manager.
    pub(super) fn unread_back(&mut self, head: &str) -> Result<Back, StateError> {
        let unread = self.unread_of(head);
        let Some(ticket) = self.current().map(|item| item.issue) else {
            return Ok(Back::On(unread));
        };
        if !self.rules_threads() {
            return Ok(Back::On(unread));
        }
        if !unread.holds() {
            self.pm_read_clear(ticket);
            return Ok(Back::On(Unread::default()));
        }
        match unread.unreviewed {
            Some(why) => match self.pm_read_wait(ticket, READ_AGAIN) {
                ReadWait::Running => Ok(Back::Done(Box::new(
                    self.held_for(Reason::ReviewerDown(why)),
                ))),
                ReadWait::Ended { wake } => {
                    if wake {
                        self.pm_ask_unread(ticket);
                    }
                    Ok(Back::Done(Box::new(self.read_back(true)?)))
                }
            },
            None => {
                self.pm_read_clear(ticket);
                Ok(Back::Done(Box::new(self.read_back(false)?)))
            }
        }
    }

    // What `head` holds that only a pass of the review clears
    fn unread_of(&self, head: &str) -> Unread {
        let Some(item) = self.current() else {
            return Unread::default();
        };
        let known = |heads: &[String]| heads.iter().any(|h| h == head);
        // A head kelpie sent unread itself is the fix of a `no` or a late round
        // only while that fix has not been read. Someone else's push is no fix.
        let fix = |from: &Option<String>| {
            from.as_ref().is_some_and(|from| *from != head)
                && !known(&item.reviewed_heads)
                && known(&item.sent_unread)
        };
        Unread {
            unreviewed: item.unreviewed.clone(),
            unread_head: !item.vouches_for(head),
            note_fix: fix(&item.noted_from),
            late_fix: fix(&item.late_from),
        }
    }

    // The item goes back through a pass of the review, as a `no` on a warned
    // merge ruling sends one: a new pass, the unreviewed mark taken off, and the
    // plan's reads to run. `tried` gives each local reviewer kelpie had left out
    // of this item's picks another go, which nothing else does once one is down.
    fn read_back(&mut self, tried: bool) -> Result<Begin, StateError> {
        self.update(|item| {
            item.new_pass();
            item.unreviewed = None;
            if tried {
                item.local_failures.clear();
            }
            item.phase = Phase::Review(Review::first());
        })?;
        self.review_step()
    }
}
