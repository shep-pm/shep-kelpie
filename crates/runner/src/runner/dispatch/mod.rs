//! Dispatch: the board's next issue opens a work item in a free slot
//!
//! A project under `concurrency.active_items` working and under
//! `concurrency.pending_rulings` parked asks the board on its steps. Adopted
//! pull requests and the rework check go first. Then a due contribution's
//! call, then an issue, then research on an untouched one. A queued issue
//! waits for a free slot. An issue whose work item is open waits for it to
//! end. One naming a file a parked item's branch changes waits for that item.
//! With a project manager and two or more issues to take, its pick replaces
//! the board's.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::Runner;
use super::add::{AddError, Unpicked};
use super::pm::Choice;
use super::report::{Begin, StepReport};
use super::trigger::lock;
use crate::board::{self, ReadyIssue, Skip};
use crate::ports::{Issue, Tickets, Timestamp};
use crate::settings::ForgeSlug;
use crate::state::StateError;
use crate::ticket::TicketId;

// The board is read only while it may open a work item, so a dropped ticket's label is read on
// its own when it has not been, at most once a poll, with the runner's lock let go. One that is
// closed or lost `ready-for-agent` is forgotten, so putting the label back returns it. One the
// forge cannot answer for stays, and is read again at the next pass.
pub(super) fn recheck_dropped(runner: &Mutex<Runner>) -> Result<(), StateError> {
    let job = lock(runner).dropped_job();
    let Some(job) = job else {
        return Ok(());
    };
    let read = job.run();
    lock(runner).apply_dropped(read)
}

// The dropped tickets whose labels are due a read
struct DroppedJob {
    tickets: Arc<dyn Tickets>,
    repo: ForgeSlug,
    dropped: Vec<TicketId>,
}

// What the read found: the tickets no longer labelled ready, and whether every one was read
struct DroppedRead {
    gone: Vec<TicketId>,
    complete: bool,
}

impl DroppedJob {
    fn run(self) -> DroppedRead {
        let unlabelled =
            |issue: &Issue| !issue.open || !issue.labels.iter().any(|l| l == board::READY);
        let mut read = DroppedRead {
            gone: Vec::new(),
            complete: true,
        };
        for ticket in self.dropped {
            match self.tickets.issue(&self.repo, ticket) {
                Ok(issue) if unlabelled(&issue) => read.gone.push(ticket),
                Ok(_) => {}
                Err(_) => read.complete = false,
            }
        }
        read
    }
}

impl Runner {
    // The read, when a dropped ticket is due one
    fn dropped_job(&self) -> Option<DroppedJob> {
        if self.state.dropped.is_empty() || !self.looks.dropped_due(self.ports.clock.now()) {
            return None;
        }
        Some(DroppedJob {
            tickets: Arc::clone(&self.ports.tickets),
            repo: self.remote.clone(),
            dropped: self.state.dropped.clone(),
        })
    }

    // Forgets the tickets the read found unlabelled that are still dropped, and records the read
    // unless a ticket could not be read
    fn apply_dropped(&mut self, read: DroppedRead) -> Result<(), StateError> {
        let gone: Vec<TicketId> = (read.gone.into_iter())
            .filter(|t| self.state.dropped.contains(t))
            .collect();
        if !gone.is_empty() {
            let mut next = self.state.clone();
            next.dropped.retain(|n| !gone.contains(n));
            self.save(next)?;
        }
        if read.complete {
            self.looks.dropped_read(self.ports.clock.now());
        }
        Ok(())
    }

    // The step runs this only while the board may open a work item, and it
    // opens at most one, so nothing here checks `concurrency.active_items` again.
    pub(super) fn dispatch(&mut self) -> Result<Begin, StateError> {
        // The board was read before the pass took the lock, unless it opened since.
        let read = self.take_board_read();
        let listed = read
            .ready
            .and_then(|ready| self.open_listed().map(|open| (ready, open)));
        let (mut ready, open) = match listed {
            Ok(listed) => listed,
            Err(e) => {
                return Ok(Begin::Report(StepReport::BoardFailed {
                    reason: format!("cannot read the board: {e}"),
                }));
            }
        };
        // A read that failed leaves no stamp, so it waits its backoff and no poll. Stamped before
        // anything the board does, so a change that does lets the next pass read again.
        let working = !self.state.work_items.is_empty();
        self.looks.reading_board(self.ports.clock.now(), working);
        self.follow_order(&ready)?;
        self.trim_finished(&read.closed)?;
        self.trim_dropped(&ready)?;
        // An issue in flight is left out, and not listed in `skipped`: it is
        // being worked on, not passed over.
        ready.retain(|issue| self.state.item(issue.ticket).is_none());
        self.ready_read(&ready)?;
        if let Some(begin) = self.close_done_parent(&ready, read.unlisted) {
            return Ok(begin);
        }
        // An adopted pull request, then one asking for a rework, goes before
        // any ready issue, and one that cannot start is passed over like one.
        let (begin, mut failed, adopt_hold) = self.adopt_waiting(&open)?;
        if let Some(begin) = begin {
            self.set_failed_skips(failed);
            return Ok(begin);
        }
        let (begin, reworks, rework_hold) = self.rework_asked(&open)?;
        failed.extend(reworks);
        if let Some(begin) = begin {
            self.set_failed_skips(failed);
            return Ok(begin);
        }
        // A contribution's call goes before any new issue, and takes the slot
        // the board would have.
        if let Some(begin) = self.contribution_due()? {
            self.set_failed_skips(failed);
            return Ok(begin);
        }
        // An untouched issue's research goes after that and before the board's
        // own pick, and takes the slot it would have.
        if let Some(begin) = self.triage_due()? {
            self.set_failed_skips(failed);
            return Ok(begin);
        }
        // One whose paths a parked item's branch touches would build on code
        // that may yet change, and one whose paths are unread may be one.
        // One the issue check refused waits for its triage or an edit.
        let mut overlaps = self.check_holds(&ready);
        ready.retain(|issue| !overlaps.iter().any(|s| s.issue() == Some(issue.ticket)));
        overlaps.extend(self.parked_overlap(&ready));
        ready.retain(|issue| !overlaps.iter().any(|s| s.issue() == Some(issue.ticket)));
        let implementers = self.implementer_files();
        let all = takeable(
            &ready,
            &open,
            &self.state.finished,
            &self.state.dropped,
            &implementers,
        );
        let held = self.pm_holds(&all, &ready);
        ready.retain(|issue| !held.contains(&issue.ticket));
        let mut paced = false;
        // Issues the agent board or the pacer could not start, and the first pacer hold,
        // which is what the step reports if no other issue can start.
        let mut unstarted: Vec<Skip> = Vec::new();
        let mut first_hold = adopt_hold.or(rework_hold);
        loop {
            let implementers = self.implementer_files();
            let (finished, dropped) = (self.state.finished.clone(), self.state.dropped.clone());
            let order = self.state.order.clone();
            let pick = board::pick(&ready, &open, &finished, &dropped, &implementers, &order);
            let mut skipped = pick.skipped;
            skipped.extend(failed.iter().cloned());
            skipped.extend(overlaps.iter().cloned());
            skipped.extend(unstarted.iter().cloned());
            skipped.sort_by_key(Skip::issue);
            let takeable = takeable(&ready, &open, &finished, &dropped, &implementers);
            let Some(picked) = pick.issue else {
                if failed.is_empty()
                    && let Some(held) = first_hold
                {
                    self.set_skipped(skipped);
                    return Ok(held);
                }
                let reason = failed
                    .iter()
                    .filter_map(|skip| match skip {
                        Skip::Failed { issue, error } => {
                            Some(format!("cannot dispatch #{issue}: {error}"))
                        }
                        Skip::Rework {
                            pull_request,
                            error,
                            ..
                        } => Some(format!("cannot rework #{pull_request}: {error}")),
                        Skip::Adopt {
                            pull_request,
                            error,
                            ..
                        } => Some(format!("cannot adopt #{pull_request}: {error}")),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                self.set_skipped(skipped);
                return Ok(if failed.is_empty() {
                    Begin::Idle
                } else {
                    Begin::Report(StepReport::BoardFailed { reason })
                });
            };
            if !paced {
                let pace = self.pace_dispatch()?;
                if let Some(hold) = pace.reason() {
                    skipped.push(Skip::Paced {
                        issue: picked,
                        hold: hold.to_owned(),
                    });
                    skipped.sort_by_key(Skip::issue);
                }
                if let Some(held) = pace.holds() {
                    self.set_skipped(skipped);
                    return Ok(held);
                }
                paced = true;
            }
            let (ticket, chosen) = match self.pm_pick(&takeable) {
                Choice::Take(chosen) => (chosen.issue, Some(chosen)),
                Choice::Rule => (picked, None),
                Choice::Wait => {
                    skipped.push(Skip::PmWaits { issue: picked });
                    skipped.sort_by_key(Skip::issue);
                    self.set_skipped(skipped);
                    return Ok(Begin::Idle);
                }
            };
            let labels = (ready.iter().find(|i| i.ticket == ticket))
                .map(|i| i.labels.clone())
                .unwrap_or_default();
            let ranked = match self.ranked(ticket, &labels, chosen.as_ref()) {
                Ok(ranked) => ranked,
                // A dog out of reach answers no issue, so this poll asks for none.
                Err(AddError::Unpicked(why @ Unpicked::Unreachable)) => {
                    let why = why.to_string();
                    skipped.push(Skip::Unpicked { issue: ticket, why });
                    skipped.sort_by_key(Skip::issue);
                    self.set_skipped(skipped);
                    return Ok(Begin::Idle);
                }
                Err(AddError::Unpicked(why)) => {
                    let why = why.to_string();
                    self.pm_unstartable(ticket, why.clone(), None);
                    unstarted.push(Skip::Unpicked { issue: ticket, why });
                    ready.retain(|i| i.ticket != ticket);
                    continue;
                }
                Err(e) => {
                    failed.push(Skip::Failed {
                        issue: ticket,
                        error: e.to_string(),
                    });
                    ready.retain(|i| i.ticket != ticket);
                    continue;
                }
            };
            // The dog reads no day allowance, so the runner's own pacer has the last word.
            let (mut opening, mut hold, mut lapses) = (None, None, None);
            for agent in ranked.agents() {
                let pace = self.pace_agent(agent)?;
                if let Some(reason) = pace.reason() {
                    hold.get_or_insert(reason.to_owned());
                }
                // The item could start once the soonest hold on its agents lapses.
                if let Some(until) = pace.until() {
                    lapses = Some(lapses.map_or(until, |soonest: Timestamp| soonest.min(until)));
                }
                match pace.holds() {
                    None => {
                        opening = Some(agent.clone());
                        break;
                    }
                    // Its usage is read first, and the step made again.
                    Some(reads @ Begin::Reads(_)) => {
                        self.set_skipped(skipped);
                        return Ok(reads);
                    }
                    Some(held) => {
                        first_hold.get_or_insert(held);
                    }
                }
            }
            let Some(agent) = opening else {
                if let Some(hold) = hold {
                    let why = format!("held by the pacer: {hold}");
                    self.pm_unstartable(ticket, why, lapses);
                    unstarted.push(Skip::Paced {
                        issue: ticket,
                        hold,
                    });
                }
                ready.retain(|i| i.ticket != ticket);
                continue;
            };
            match self.add_picked(ticket, chosen.as_ref(), Some(ranked.opening(agent))) {
                Ok(agent) => {
                    self.pm_started(ticket);
                    self.set_skipped(skipped.clone());
                    return Ok(Begin::Report(StepReport::Dispatched {
                        issue: ticket,
                        agent,
                        skipped,
                    }));
                }
                Err(e @ (AddError::Forge(_) | AddError::Label(_))) => {
                    failed.push(Skip::Failed {
                        issue: ticket,
                        error: e.to_string(),
                    });
                    ready.retain(|i| i.ticket != ticket);
                }
                Err(AddError::State(e)) => return Err(e),
                Err(e) => {
                    self.set_skipped(skipped);
                    return Ok(Begin::Report(StepReport::BoardFailed {
                        reason: format!("cannot dispatch {}: {e}", ticket.shown()),
                    }));
                }
            }
        }
    }
}

// The ready issues the board could take now, each on its own.
fn takeable(
    ready: &[ReadyIssue],
    open: &[board::OpenPullRequest],
    finished: &[TicketId],
    dropped: &[TicketId],
    implementers: &[crate::settings::AgentName],
) -> Vec<TicketId> {
    (ready.iter())
        .filter(|i| {
            let one = std::slice::from_ref(*i);
            board::pick(one, open, finished, dropped, implementers, &[])
                .issue
                .is_some()
        })
        .map(|i| i.ticket)
        .collect()
}

impl Runner {
    // Each read of the board updates the order it takes the ready issues in:
    // a new issue joins it, one whose priority label changed moves, and one
    // the forge stopped listing leaves it.
    fn follow_order(&mut self, ready: &[ReadyIssue]) -> Result<(), StateError> {
        let (order, ranked) = board::keep_order(&self.state.order, &self.state.ranked, ready);
        if order == self.state.order && ranked == self.state.ranked {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.order = order;
        next.ranked = ranked;
        self.save(next)
    }

    // The board skips a finished issue until the forge closes it, so an entry
    // goes only once its ticket reads as closed. One the forge lists ready is
    // open, and one it cannot answer for stays. The read names the `closed` ones.
    fn trim_finished(&mut self, closed: &[TicketId]) -> Result<(), StateError> {
        if closed.is_empty() {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.finished.retain(|n| !closed.contains(n));
        self.save(next)
    }

    // The board leaves a dropped ticket be while the forge lists it ready, so
    // an entry goes once the forge stops listing it: a label put back is then
    // new. It goes too once the issue's text reads differently from the first
    // time the board saw it after the drop: an edit re-scopes it.
    fn trim_dropped(&mut self, ready: &[ReadyIssue]) -> Result<(), StateError> {
        let mut next = self.state.clone();
        let mut text = BTreeMap::new();
        next.dropped.retain(|t| {
            let Some(issue) = ready.iter().find(|r| r.ticket == *t) else {
                return false;
            };
            let now = issue.fingerprint();
            let unedited = (self.state.dropped_text.get(t)).is_none_or(|then| *then == now);
            if unedited {
                text.insert(*t, now);
            }
            unedited
        });
        next.dropped_text = text;
        if next == self.state {
            return Ok(());
        }
        self.save(next)
    }
}

#[cfg(test)]
mod tests;
