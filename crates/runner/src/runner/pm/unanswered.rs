//! A stuck item's ruling the reply named nothing for
//!
//! A stuck item's ruling is the one wake reason with no fallback: a question,
//! a follow-up and a ruling to answer each reach the maintainer when the reply
//! leaves them alone. A reply that settles a parked item only in its `why`
//! leaves the ruling standing and its wake's log line reads `nothing`, so
//! kelpie asks once more, naming the ruling, and on the reply after that
//! leaves the item alone as `none` would.

use std::collections::BTreeSet;

use super::answer::Answer;
use super::{StuckState, Wake};
use crate::board::briefing::ticket_ref;
use crate::runner::Runner;
use crate::ticket::TicketId;

impl Runner {
    /// Asks the project manager once more about each stuck item whose ruling
    /// its reply named nothing for, and says what kelpie did, for the log
    ///
    /// The reply after the one that asks leaves the item alone as `none` would,
    /// so the same ruling cannot wake the project manager in a loop.
    pub(super) fn pm_unanswered(&mut self, wakes: &[Wake], answer: &Answer) -> Vec<String> {
        let now = self.ports.clock.now();
        let stands: BTreeSet<TicketId> = self.state.rulings.iter().map(|r| r.id).collect();
        self.pm.nudged.retain(|id| stands.contains(id));
        let mut done = Vec::new();
        for wake in wakes {
            let Wake::Stuck { ticket, state, .. } = wake else {
                continue;
            };
            let StuckState::Ruling { id, .. } = state else {
                continue;
            };
            if !stands.contains(id) || named(answer, *ticket, *id) {
                continue;
            }
            let item = ticket_ref(*ticket);
            if self.pm.nudged.remove(id) {
                self.pm_left_alone(*ticket, state.clone(), now);
                done.push(format!(
                    "left {item} to resolve itself: the reply held no unstick for ruling {id} \
                     again"
                ));
                continue;
            }
            self.pm.nudged.insert(*id);
            self.pm_due(wake.clone());
            self.pm.lost.push(format!(
                "{item} is parked on ruling {id}, and your reply held no `unstick` for it. \
                 Answer `none` in `unstick` to leave it alone on purpose."
            ));
            done.push(format!(
                "asked again about ruling {id}: the reply held no unstick for {item}"
            ));
        }
        done
    }
}

// Whether `answer` settles the item parked on ruling `id`: it names the item in
// a key that acts on a stuck item, which a `none` does, or answers the ruling
// itself. A ruling it answered but kelpie could not apply is the maintainer's to
// decide, not one to ask about again.
fn named(answer: &Answer, ticket: TicketId, id: TicketId) -> bool {
    (answer.unstick.iter()).any(|u| u.item == ticket)
        || (answer.fix.iter()).any(|f| f.item == ticket)
        || (answer.answer.iter()).any(|a| a.item == ticket)
        || (answer.votes.iter()).any(|(vote, _)| *vote == id)
}
