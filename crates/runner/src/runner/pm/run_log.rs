//! The project manager's ask for a green run's log, and the wake that carries it
//!
//! A ruling waits on the project manager for one wake. One it left unanswered while
//! it asked for a log is put to it again, so the maintainer is not asked before it
//! has read what it asked for.

use super::Wake;
use super::answer::Answer;
use crate::board::briefing::ticket_ref;
use crate::runner::Runner;
use crate::ticket::TicketId;

impl Runner {
    /// Queues the wake that carries the log of run `run` on `ticket`'s `## CI` list,
    /// and says what it did
    ///
    /// # Errors
    ///
    /// Why the ask was dropped: the item is not parked, the run is not on the list, or its
    /// case holds the log.
    pub(super) fn ask_run_log(&mut self, ticket: TicketId, run: u64) -> Result<String, String> {
        let named = ticket_ref(ticket);
        let parked = self.parked_items();
        if !parked.iter().any(|p| p.ticket == ticket && !p.contribution) {
            return Err(format!(
                "run_log {named} run {run}: it is no longer parked, so no case carries the log"
            ));
        }
        let listed = self.pm.ci_runs.get(&ticket).into_iter().flatten();
        let mut listed = listed.filter(|r| r.run == run).peekable();
        if listed.peek().is_none() {
            return Err(format!(
                "run_log {named} run {run}: the run is not on its `## CI` list"
            ));
        }
        let open = listed
            .map(|r| (ticket, run, r.attempt))
            .find(|key| !self.pm.run_logs.contains_key(key));
        if open.is_some_and(|key| self.pm.run_log_tried.contains(&key)) {
            return Err(format!(
                "run_log {named} run {run}: its log was read once and could not be"
            ));
        }
        let Some(key) = open else {
            return Err(format!(
                "run_log {named} run {run}: its case already holds the log"
            ));
        };
        if !self.pm.run_log_asks.contains(&key) {
            self.pm.run_log_asks.push(key);
        }
        self.pm_due(Wake::RunLog { ticket, run });
        Ok(format!("asked for the log of run {run} on {named}"))
    }

    /// Queues again each ruling in `wakes` that `answer` left unanswered while it asked for
    /// a log on the ruling's item, and holds it from the maintainer for that wake
    ///
    /// It ends by itself: a log is asked for once, and a head has only so many runs.
    pub(super) fn pm_asks_again(&mut self, wakes: &[Wake], answer: &Answer) {
        let asked: Vec<TicketId> = self.pm.run_log_asks.iter().map(|&(t, ..)| t).collect();
        for wake in wakes {
            let (id, item, left) = match wake {
                Wake::Question { ticket, id, .. } => (
                    *id,
                    Some(*ticket),
                    !answer.answer.iter().any(|s| s.item == *ticket),
                ),
                Wake::FollowUp { ticket, id, .. } => (
                    *id,
                    Some(*ticket),
                    answer.follow_up.as_ref().is_none_or(|d| d.item != *ticket),
                ),
                Wake::Answer {
                    id, request: None, ..
                } => (
                    *id,
                    self.state
                        .rulings
                        .iter()
                        .find(|r| r.id == *id)
                        .and_then(|r| r.issue),
                    !answer.votes.iter().any(|(vote, _)| vote == id),
                ),
                _ => continue,
            };
            let waiting =
                (self.state.rulings.iter()).any(|r| r.id == id && r.unposted && !r.alerted);
            if left && waiting && item.is_some_and(|item| asked.contains(&item)) {
                self.pm.asking.insert(id, self.ports.clock.now());
                self.pm_due(wake.clone());
            }
        }
    }
}
