//! A review bot the pass went on without, read again later
//!
//! A bot passed over for its pass may review the head anyway. Kelpie reads
//! whether it has at the start of each round, when CI goes green, and every
//! [`LATE_READ`] seconds while a merge ruling waits. One that has gets a round
//! of its own, whose threads settle as any review's. Once the pass has ended,
//! that round counts every listed reviewer as run, so its fix goes back to the
//! merge ruling, after any other late round, with no new pass; the ruling says
//! no reviewer read the fix. A merge ruling waiting on the item is withdrawn for
//! it, and the maintainer told so.

use super::super::Runner;
use super::super::report::Begin;
use super::super::talk::At;
use super::settle::open_ids;
use crate::board::HUMAN;
use crate::settings::AgentName;
use crate::state::{RulingKind, StateError};
use crate::ticket::TicketId;
use crate::work_item::{Phase, Review, ReviewStage};

// A read is a fetch and a forge call for each bot passed over, and a merge
// ruling can wait for hours. Five minutes keeps that to a dozen reads an
// hour, and a late review reaches the worker long before most answers.
pub(in crate::runner) const LATE_READ: u64 = 300;

impl Runner {
    /// Starts a round for a bot this pass went on without that has since
    /// reviewed the head anyway, whose threads then settle as any review's
    ///
    /// `None` when no such bot has. Parked on a merge ruling, the work item
    /// leaves it, and the ruling is withdrawn.
    pub(in crate::runner) fn late_review(&mut self) -> Result<Option<Begin>, StateError> {
        // A step made again after a read finds the work item as the read left it
        let Some(item) = self.current() else {
            return Ok(Some(Begin::Idle));
        };
        let Some(number) = item.pull_request else {
            return Ok(Some(Begin::Idle));
        };
        if item.bots_skipped.is_empty() {
            return Ok(None);
        }
        let skipped = item.bots_skipped.clone();
        let head = match self.pushed_head() {
            Ok(head) => head,
            Err(begin) => return Ok(Some(*begin)),
        };
        for reviewer in skipped.iter().map(|s| s.reviewer().clone()) {
            let listed = self.listed(&reviewer).and_then(|r| r.bot());
            // A review from before an adoption does not stand for one of kelpie's.
            let Some(bot) = listed
                .map(|b| b.bot)
                .filter(|bot| !self.item().summons_owed.contains(bot))
            else {
                continue;
            };
            let activity = match self.read_activity(bot, number) {
                Ok(activity) => activity,
                Err(begin) => return Ok(Some(*begin)),
            };
            if !self.profile(bot).covers(&activity, &head) {
                continue;
            }
            let now = self.ports.clock.now();
            let stage = ReviewStage::Settling {
                bot,
                since: now,
                read: now,
                open: open_ids(&activity),
            };
            self.late_round(reviewer, head, stage, true)?;
            return Ok(Some(Begin::Idle));
        }
        Ok(None)
    }

    /// Starts a round for a listed bot that had none this pass, though it
    /// left threads above a nit open on the pull request
    ///
    /// A bot that reviews every push on its own can post before the pass
    /// reaches its round, and the pass ends once the reads the item calls for
    /// have run. Its threads would only be named in the merge ruling and
    /// never reach the worker. `None` when no bot is in that case.
    pub(in crate::runner) fn unsent_review(&mut self) -> Result<Option<Begin>, StateError> {
        let Some(item) = self.current() else {
            return Ok(Some(Begin::Idle));
        };
        let Some(number) = item.pull_request else {
            return Ok(Some(Begin::Idle));
        };
        let candidates: Vec<_> = (self.lineup.iter())
            .filter_map(|r| Some((r.name.clone(), r.bot()?.bot)))
            .filter(|(name, bot)| {
                let item = self.item();
                !item.bot_reads.contains_key(bot)
                    && !item.summons_owed.contains(bot)
                    && !item.bots_skipped.iter().any(|s| s.reviewer() == name)
            })
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }
        let head = match self.pushed_head() {
            Ok(head) => head,
            Err(begin) => return Ok(Some(*begin)),
        };
        for (reviewer, bot) in candidates {
            let activity = match self.read_activity(bot, number) {
                Ok(activity) => activity,
                Err(begin) => return Ok(Some(*begin)),
            };
            if self
                .threads_in(&[(bot, activity.clone())])
                .holding
                .is_none()
            {
                continue;
            }
            let now = self.ports.clock.now();
            let stage = ReviewStage::Settling {
                bot,
                since: now,
                read: now,
                open: open_ids(&activity),
            };
            self.late_round(reviewer, head, stage, false)?;
            return Ok(Some(Begin::Idle));
        }
        Ok(None)
    }

    // Puts the work item in `reviewer`'s round, which read `head`: the next
    // round of a pass under way, or a late one once the pass has ended.
    fn late_round(
        &mut self,
        reviewer: AgentName,
        head: String,
        stage: ReviewStage,
        reads_head: bool,
    ) -> Result<(), StateError> {
        let parked = match self.item().phase {
            Phase::Ruling { id } => Some(id),
            _ => None,
        };
        let number = self.number();
        let ran: Vec<AgentName> = self.lineup.iter().map(|r| r.name.clone()).collect();
        let mut next = self.state.clone();
        next.rulings.retain(|r| Some(r.id) != parked);
        let item = self
            .current_in(&mut next)
            .expect("a late round is a work item's");
        if reads_head {
            item.reviewed(head.clone());
        }
        item.bots_skipped.retain(|s| s.reviewer() != &reviewer);
        match &mut item.phase {
            Phase::Review(review) => {
                review.reviewer = Some(reviewer.clone());
                review.stage = stage;
            }
            _ => {
                item.noted_from = None;
                item.late_from = Some(head);
                item.phase = Phase::Review(Review::late(reviewer.clone(), ran, stage));
            }
        }
        let issue = item.issue;
        self.save(next)?;
        let Some(id) = parked else {
            return Ok(());
        };
        self.late_reads.remove(&issue);
        let why = format!(
            "{reviewer} reviewed its head after the review pass went on without it, \
             so the worker gets that review first, and the merge ruling is raised \
             again once CI is green after it"
        );
        self.say_withdrawn(id, issue, Some(number), &why);
        // The worker's turn again, so the hand-back label comes off. One the
        // forge keeps is still kelpie's, and the next merge ruling keeps it.
        match (self.ports.forge).set_label(&self.remote, number, HUMAN, false) {
            Ok(()) => self.update(|item| item.known.labels.retain(|l| l != HUMAN)),
            Err(e) => {
                eprintln!("cannot take the `{HUMAN}` label off #{number}: {e}");
                Ok(())
            }
        }
    }

    /// Reads, while the work item waits on merge ruling `id`, whether a bot
    /// its pass went on without has reviewed the head, at most once every
    /// [`LATE_READ`] seconds from when the ruling was raised
    pub(in crate::runner) fn late_while_parked(
        &mut self,
        id: TicketId,
    ) -> Result<Begin, StateError> {
        let merge = |kind: &RulingKind| matches!(kind, RulingKind::Merge { .. });
        let on_merge = self
            .state
            .rulings
            .iter()
            .any(|r| r.id == id && merge(&r.kind));
        if !on_merge {
            return Ok(Begin::Idle);
        }
        let Some(issue) = self.current().map(|item| item.issue) else {
            return Ok(Begin::Idle);
        };
        let now = self.ports.clock.now();
        let last = *self.late_reads.entry(issue).or_insert(now);
        if now.0.saturating_sub(last.0) < LATE_READ {
            return Ok(Begin::Idle);
        }
        let begin = match self.late_review()? {
            Some(begin) => Some(begin),
            None => self.talk_turn(&At::Parked(id))?,
        };
        // Stamped once the reads are made, since a step made again after one is due as well, and
        // unless the work item left the ruling, which forgets the stamp
        let parked = self
            .current()
            .is_some_and(|item| item.phase == Phase::Ruling { id });
        if parked && !matches!(begin, Some(Begin::Reads(_))) {
            self.late_reads.insert(issue, now);
        }
        Ok(begin.unwrap_or(Begin::Idle))
    }

    /// Notes that the work item's merge ruling, about to be raised, last
    /// read the bots its pass went on without now
    pub(in crate::runner) fn late_read_now(&mut self) {
        let Some(item) = self.current() else {
            return;
        };
        let issue = item.issue;
        self.late_reads.insert(issue, self.ports.clock.now());
    }
}
