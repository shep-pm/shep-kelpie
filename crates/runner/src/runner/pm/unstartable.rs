//! The ready issues no agent could start on their last try, which wake no
//! pick, so the same ones never wake the project manager every poll

use super::Pick;
use crate::ports::Timestamp;
use crate::runner::Runner;
use crate::ticket::TicketId;

impl Runner {
    /// Notes that no agent could start `ticket` now, for `why`, until `lapses`
    /// when a pacer hold kept it: a pick of it is dropped saying so, and it
    /// wakes no pick until its work item opens or the hold lapses
    pub(in crate::runner) fn pm_unstartable(
        &mut self,
        ticket: TicketId,
        why: String,
        lapses: Option<Timestamp>,
    ) {
        self.pm.unstartable.insert(ticket, lapses);
        let Some(Pick::Issues(named)) = &mut self.pm.pick else {
            return;
        };
        if named.first().is_none_or(|head| head.issue != ticket) {
            return;
        }
        named.remove(0);
        if named.is_empty() {
            self.pm.pick = Some(Pick::Rule(self.free_slots().max(1)));
        }
        self.pick_dropped(ticket, why);
    }

    /// How many of `takeable` a pick could start, leaving out those no agent
    /// could start on their last try
    pub(super) fn pm_choices(&self, takeable: &[TicketId]) -> usize {
        let startable = takeable
            .iter()
            .filter(|t| !self.pm.unstartable.contains_key(t));
        startable.count()
    }

    /// Whether a pick wake is due for `takeable`: two issues an agent could
    /// start, or one whose hold just ended on a close
    pub(super) fn pm_asks_pick(&self, takeable: &[TicketId]) -> bool {
        self.pm_choices(takeable) >= 2 || self.pm_hold_ended(takeable)
    }

    /// Whether an issue in `takeable` has a hold that ended on a close, which
    /// no pick has decided, and an agent could start it
    pub(super) fn pm_hold_ended(&self, takeable: &[TicketId]) -> bool {
        takeable
            .iter()
            .any(|t| self.pm.after_hold.contains(t) && !self.pm.unstartable.contains_key(t))
    }

    /// Forgets each unstartable issue the board no longer lists, or whose
    /// pacer hold lapsed, called once a pass
    pub(super) fn pm_forget_unstartable(&mut self, takeable: &[TicketId]) {
        let now = self.ports.clock.now();
        (self.pm.unstartable)
            .retain(|t, lapses| takeable.contains(t) && lapses.is_none_or(|lapses| now < lapses));
    }
}
