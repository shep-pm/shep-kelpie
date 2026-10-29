//! Reading the board with the runner's lock let go
//!
//! The poll lists the ready issues, the open pull requests, and each finished
//! ticket's issue. A pass would hold the lock through all of it, so
//! [`read_board`] runs first: [`Runner::poll_job`] decides a poll is due,
//! [`BoardJob::run`] reads, and [`Runner::store_board_read`] keeps what was
//! read. [`Runner::take_board_read`] hands it to `dispatch`, which stamps the
//! look. A read a pass did not use, or one kelpie's own write follows, is
//! dropped. The next pass reads again.

use std::sync::{Arc, Mutex};

use super::Runner;
use super::parent::{self, Refused, Unbuilt, Unlisted};
use super::trigger::lock;
use super::turn::Slot;
use crate::board::{Blocker, OpenPullRequest, ReadyIssue, Skip, check};
use crate::ports::{Forge, ForgeError, Meter, MeterError, Tickets, Timestamp, Utilization};
use crate::settings::{Account, ForgeSlug};
use crate::ticket::TicketId;

/// A read of the board the runner decided was due, which needs no lock
pub(super) struct BoardJob {
    tickets: Arc<dyn Tickets>,
    forge: Arc<dyn Forge>,
    repo: ForgeSlug,
    now: Timestamp,
    /// The writes of kelpie's made when the job was built
    writes: u64,
    /// The open pull requests, when the pass has listed them already
    listed: Option<Result<Vec<OpenPullRequest>, ForgeError>>,
    finished: Vec<TicketId>,
    items: Vec<TicketId>,
    refused: Refused,
    unbuilt: Unbuilt,
    /// The accounts a new work item is paced on, read when the board may open one
    meters: Vec<(Account, Arc<dyn Meter>)>,
    adopting: bool,
}

/// What a read of the board found
#[derive(Debug)]
pub(super) struct BoardRead {
    /// The writes of kelpie's made before the read began, which it is stale after
    writes: u64,
    /// The ready issues, or why they could not be read
    pub(super) ready: Result<Vec<ReadyIssue>, ForgeError>,
    /// The open pull requests, listed once the ready issues were read
    open: Option<Result<Vec<OpenPullRequest>, ForgeError>>,
    /// The ready issues held because a blocker their `Blocked by:` line names could not be linked
    pub(super) unlinked: Vec<Skip>,
    /// The finished tickets the forge reads as closed
    pub(super) closed: Vec<TicketId>,
    /// The search for a parent off the ready list, when the board lists none to close
    pub(super) unlisted: Option<Unlisted>,
    usage: Vec<(Account, Result<Utilization, MeterError>)>,
}

/// Reads the board, if a poll is due, asking the forge with the runner's lock let go
pub(super) fn read_board(runner: &Mutex<Runner>) {
    let job = lock(runner).poll_job();
    let Some(job) = job else {
        return;
    };
    let read = job.run();
    lock(runner).store_board_read(read);
}

impl BoardJob {
    // Reads in the order the board always has: the ready issues, the open pull requests, the issue
    // of each finished ticket the board no longer lists, then the parents off its list. A read
    // that fails ends the rest. Then the usage the pacer reads before a new work item.
    fn run(self) -> BoardRead {
        let mut read = BoardRead {
            writes: self.writes,
            ready: self.tickets.ready_issues(&self.repo),
            open: None,
            unlinked: Vec::new(),
            closed: Vec::new(),
            unlisted: None,
            usage: Vec::new(),
        };
        if let Ok(ready) = &mut read.ready {
            read.unlinked = self.link_blockers(ready);
        }
        let Ok(ready) = &read.ready else {
            return read;
        };
        let open = (self.listed).unwrap_or_else(|| self.forge.open_pull_requests(&self.repo));
        let listed = open.is_ok();
        read.open = Some(open);
        if !listed {
            return read;
        }
        read.closed = (self.finished.iter().copied())
            .filter(|t| !ready.iter().any(|r| r.ticket == *t))
            .filter(|t| (self.tickets.issue(&self.repo, *t)).is_ok_and(|issue| !issue.open))
            .collect();
        if !parent::lists_done_parent(&self.refused, ready, &self.items) {
            let held: Vec<TicketId> = (ready.iter().map(|i| i.ticket))
                .chain(self.items.iter().copied())
                .collect();
            read.unlisted = Some(parent::find_unlisted(
                self.tickets.as_ref(),
                &self.repo,
                self.now,
                &held,
                &self.refused,
                self.unbuilt,
            ));
        }
        if self.adopting || ready.iter().any(|r| !self.items.contains(&r.ticket)) {
            read.usage = (self.meters.into_iter())
                .map(|(account, meter)| (account, meter.read(self.now)))
                .collect();
        }
        read
    }
}

impl BoardJob {
    // Links each blocker an issue's `Blocked by:` line names that the source does not list, so
    // the pick reads one source. An issue with a work item or a finished one is left alone. Gives
    // the skip of each issue a link failed for, which the pick must not take.
    fn link_blockers(&self, ready: &mut [ReadyIssue]) -> Vec<Skip> {
        let mut held = Vec::new();
        for issue in ready.iter_mut() {
            if self.items.contains(&issue.ticket) || self.finished.contains(&issue.ticket) {
                continue;
            }
            // A link joins two tickets of one source, so a line naming the other's is no link.
            let same_source =
                |b: &TicketId| b.github().is_some() == issue.ticket.github().is_some();
            let named = check::blockers(&issue.body).into_iter().filter(same_source);
            let missing: Vec<TicketId> = named
                .filter(|b| *b != issue.ticket && !issue.blocked_by.iter().any(|l| l.ticket == *b))
                .collect();
            let mut failed: Vec<(TicketId, String)> = Vec::new();
            for blocker in missing {
                match self
                    .tickets
                    .add_blocked_by(&self.repo, issue.ticket, blocker)
                {
                    Ok(()) => {
                        // A blocker that cannot be read counts as open until the next read.
                        let open = (self.tickets.issue(&self.repo, blocker)).is_ok_and(|b| b.open);
                        issue.blocked_by.push(Blocker {
                            ticket: blocker,
                            open,
                        });
                    }
                    Err(e) => failed.push((blocker, e.to_string())),
                }
            }
            if let Some((_, error)) = failed.first() {
                let mut by: Vec<TicketId> = failed.iter().map(|(b, _)| *b).collect();
                by.sort_unstable();
                held.push(Skip::Unlinked {
                    issue: issue.ticket,
                    by,
                    error: error.clone(),
                });
            }
        }
        held
    }
}

impl Runner {
    // Decides whether the board is read now: while the board may open a work item and its look is
    // due, which is what the rotation asks of its step. Forgets a read an earlier pass left.
    fn poll_job(&mut self) -> Option<BoardJob> {
        self.board_read = None;
        self.snapshot.drop_preload();
        let now = self.ports.clock.now();
        if !self.board_open() || !self.looks.due(Slot::Board, now) {
            return None;
        }
        // A limit used up is learned by the first call the pass makes, which is not the board's
        // listing.
        if self.forge_hold.until(now).is_some() {
            return None;
        }
        Some(self.job_at(now, None))
    }

    fn job_at(
        &self,
        now: Timestamp,
        listed: Option<Result<Vec<OpenPullRequest>, ForgeError>>,
    ) -> BoardJob {
        BoardJob {
            tickets: Arc::clone(&self.ports.tickets),
            forge: Arc::clone(&self.ports.forge),
            repo: self.remote.clone(),
            now,
            writes: self.forge_ledger.writes(),
            listed,
            finished: self.state.finished.clone(),
            items: self.state.work_items.iter().map(|i| i.issue).collect(),
            refused: self.close_refused.clone(),
            unbuilt: self.unbuilt.clone(),
            meters: self.dispatch_meters(now),
            adopting: !self.state.adopted.is_empty(),
        }
    }

    // Keeps what a read found for the pass that follows, listing included
    fn store_board_read(&mut self, read: BoardRead) {
        if let Some(open) = &read.open {
            self.snapshot.preload(open.clone());
        }
        self.board_read = Some(read);
    }

    /// Starts the pass's snapshot, with the listing the board read took unless a write of
    /// kelpie's came after it
    pub(super) fn begin_pass(&mut self) {
        if (self.board_read.as_ref()).is_some_and(|read| read.writes != self.forge_ledger.writes())
        {
            self.board_read = None;
            self.snapshot.drop_preload();
        }
        self.reads.clear();
        self.snapshot.begin_pass();
    }

    /// Ends the pass, dropping a board read it did not use
    pub(super) fn end_pass(&mut self) {
        self.board_read = None;
        self.reads.clear();
        self.snapshot.end_pass();
    }

    /// What the board shows: the read taken before the pass, or one made now with the lock held
    pub(super) fn take_board_read(&mut self) -> BoardRead {
        if let Some(mut read) = self.board_read.take() {
            for (account, usage) in read.usage.drain(..) {
                self.reads.keep_usage(account, usage);
            }
            return read;
        }
        let now = self.ports.clock.now();
        let read = self.job_at(now, self.snapshot.listed()).run();
        if let Some(open) = &read.open {
            self.snapshot.preload(open.clone());
        }
        read
    }
}
