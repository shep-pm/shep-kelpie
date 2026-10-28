//! Reading parked work items' pull requests and issues with the runner's lock let go
//!
//! [`Runner::parked_reads_due`] picks the items due and keys each, [`Reads::run`] reads them,
//! [`settle_issue_reads`] asks git whether a closed issue's branch holds work, and
//! [`Runner::apply_parked`] re-checks each item's key and ends it, or leaves it. A read that
//! fails, or an item that moved meanwhile, is not stamped, so the next pass reads it again.

use std::sync::{Arc, Mutex};

use super::{PARKED_READ, covering, end_closed, end_covered, end_merged};
use crate::board::OpenPullRequest;
use crate::ports::{Forge, ForgeError, PullRequestState, Tickets};
use crate::runner::Runner;
use crate::runner::contributions::Logins;
use crate::runner::off_lock::{ItemKey, with_item};
use crate::runner::ruling::park;
use crate::runner::trigger::lock;
use crate::settings::ForgeSlug;
use crate::state::{RulingKind, StateError, Stuck};
use crate::ticket::TicketId;

/// Ends each parked work item whose issue was closed with no change, or whose pull request was
/// merged, and gives one whose pull request was closed without merging a `closed` ruling
///
/// See [`Runner::parked_reads_due`] for which items are read.
///
/// # Errors
///
/// [`StateError::Write`] when a change cannot be saved.
pub(in crate::runner) fn read_parked(runner: &Mutex<Runner>) -> Result<(), StateError> {
    let reads = lock(runner).parked_reads_due();
    if reads.items.is_empty() {
        return Ok(());
    }
    let mut seen = reads.run();
    let open = reads.list_open();
    reads.mark_covered(open.as_ref(), &mut seen);
    settle_issue_reads(runner, &mut seen);
    if let Some(note) = reads.drop_claimed(open.as_ref(), &mut seen) {
        lock(runner).notes.push(note);
    }
    for (item, seen) in seen {
        let applied = with_item(runner, &item.key, |runner| {
            runner.apply_parked(item.ticket, seen)
        });
        applied.transpose()?;
    }
    Ok(())
}

/// Asks git whether the branch of each item whose issue was read closed holds work, which a fetch
/// answers with the lock let go
fn settle_issue_reads(runner: &Mutex<Runner>, seen: &mut [(Parked, Seen)]) {
    let closed: Vec<_> = {
        let runner = lock(runner);
        (seen.iter().enumerate())
            .filter(|(_, (_, seen))| matches!(seen, Seen::IssueClosed))
            .map(|(at, (item, _))| (at, runner.tree_to_check(item.ticket)))
            .collect()
    };
    let read: Vec<_> = (closed.into_iter())
        .map(|(at, tree)| (at, tree.map(|tree| tree.holds_work())))
        .collect();
    let mut runner = lock(runner);
    for (at, read) in read {
        let (item, seen) = &mut seen[at];
        *seen = match runner.no_work(item.ticket, read) {
            true => Seen::Ended,
            false => Seen::Quiet,
        };
    }
}

/// The parked work items due a read, and what reads them
struct Reads {
    forge: Arc<dyn Forge>,
    tickets: Arc<dyn Tickets>,
    repo: ForgeSlug,
    items: Vec<Parked>,
    // The logins whose pull requests do not cover a work item, where they could be read
    own: Option<Logins>,
}

/// A parked work item as it stood when its read began
#[derive(Clone)]
struct Parked {
    key: ItemKey,
    ticket: TicketId,
    branch: String,
    /// Its pull request, which is read in place of its issue
    number: Option<u64>,
}

/// What a read of one parked work item found
enum Seen {
    /// Its pull request, as it stands
    Pull(u64, PullRequestState),
    /// Its issue is open, or closed over work its branch holds
    Quiet,
    /// Its issue is closed
    IssueClosed,
    /// Its issue is closed with no work on its branch: it ends
    Ended,
    /// An open pull request, by number, that a contributor opened closes its issue: it ends
    Covered(u64),
    /// The forge could not be read, with what is noted
    Failed(String),
    /// The open pull requests could not be listed, which was noted once
    Unlisted,
}

impl Reads {
    // Each parked item's pull request, else its issue
    fn run(&self) -> Vec<(Parked, Seen)> {
        let read = |item: &Parked| match item.number {
            Some(number) => match self.forge.pull_request(&self.repo, number) {
                Ok(pr) => Seen::Pull(number, pr.state),
                Err(e) => Seen::Failed(format!("cannot read #{number}: {e}")),
            },
            None => match self.tickets.issue(&self.repo, item.ticket) {
                Ok(issue) if issue.open => Seen::Quiet,
                Ok(_) => Seen::IssueClosed,
                Err(e) => Seen::Failed(read_failed(item.ticket, &e)),
            },
        };
        let items = self.items.iter();
        items.map(|item| (item.clone(), read(item))).collect()
    }

    // The open pull requests, listed once for the parked items that have no pull request of their
    // own, or none when every item has one
    fn list_open(&self) -> Option<Result<Vec<OpenPullRequest>, ForgeError>> {
        let wanted = self.items.iter().any(|item| item.number.is_none());
        wanted.then(|| self.forge.open_pull_requests(&self.repo))
    }

    // Marks each item with no pull request of its own that a contributor's open pull request
    // closes the issue of. A list or a login that could not be read marks none.
    fn mark_covered(
        &self,
        open: Option<&Result<Vec<OpenPullRequest>, ForgeError>>,
        seen: &mut [(Parked, Seen)],
    ) {
        let (Some(Ok(open)), Some(own)) = (open, &self.own) else {
            return;
        };
        for (item, seen) in seen {
            if item.number.is_some() {
                continue;
            }
            if let Some(number) = covering(open, item.ticket, &item.branch, own).next() {
                *seen = Seen::Covered(number);
            }
        }
    }

    // Keeps open an ended item that an open pull request on another branch closes the issue of:
    // that is work too. The open pull requests are listed once, and only when one ended. A list
    // that cannot be read keeps every ended item open, and its note is returned.
    fn drop_claimed(
        &self,
        open: Option<&Result<Vec<OpenPullRequest>, ForgeError>>,
        seen: &mut [(Parked, Seen)],
    ) -> Option<String> {
        if !seen.iter().any(|(_, seen)| matches!(seen, Seen::Ended)) {
            return None;
        }
        let open = open.expect("an ended item has no pull request, so the open ones were listed");
        let claimed = |ticket: TicketId| match (open, ticket.github()) {
            (Ok(open), Some(number)) => open.iter().any(|pr| pr.closes.contains(&number)),
            // A local ticket is closed by no GitHub pull request.
            (Ok(_), None) => false,
            (Err(_), _) => true,
        };
        let listed = open
            .as_ref()
            .err()
            .map(|e| format!("cannot list open pull requests: {e}"));
        for (item, seen) in seen {
            if matches!(seen, Seen::Ended) && claimed(item.ticket) {
                *seen = if listed.is_some() {
                    Seen::Unlisted
                } else {
                    Seen::Quiet
                };
            }
        }
        listed
    }
}

fn read_failed(ticket: TicketId, why: &ForgeError) -> String {
    format!("cannot read {}: {why}", ticket.named().to_lowercase())
}

impl Runner {
    /// The parked work items due a read, each keyed
    ///
    /// Each such item is read at most once every [`PARKED_READ`] seconds, counted in memory, so a
    /// restart reads it at once. An attached item is left alone, and so is one parked on its
    /// follow-up ruling, whose merge kelpie made.
    fn parked_reads_due(&mut self) -> Reads {
        let now = self.ports.clock.now();
        let due = |ticket: TicketId| {
            (self.parked_reads.get(&ticket))
                .is_none_or(|at| now.0.saturating_sub(at.0) >= PARKED_READ)
        };
        let items: Vec<Parked> = (self.state.work_items.iter())
            .filter(|i| {
                i.parked()
                    && i.attached.is_none()
                    && due(i.issue)
                    && !self.parked_on(i, |k| matches!(k, RulingKind::FollowUp { .. }))
            })
            .map(|item| Parked {
                key: ItemKey::of(item),
                ticket: item.issue,
                branch: item.branch.clone(),
                number: item.pull_request,
            })
            .collect();
        // Asked only when an item might be covered; a login that cannot be read is noted.
        let own = match items.iter().any(|item| item.number.is_none()) {
            true => self.own_logins_noted(),
            false => None,
        };
        Reads {
            forge: Arc::clone(&self.ports.forge),
            tickets: Arc::clone(&self.ports.tickets),
            repo: self.remote.clone(),
            items,
            own,
        }
    }

    // Applies a read of the work item for `ticket`, which is as its key had it, and stamps the
    // read unless it failed or could not be saved. A merged pull request ends the item, a closed
    // one gives it the gate's `closed` ruling in its old one's place, unless it has that one
    // already: only a merge changes it then.
    fn apply_parked(&mut self, ticket: TicketId, seen: Seen) -> Result<(), StateError> {
        match seen {
            Seen::Failed(note) => {
                self.notes.push(note);
                return Ok(());
            }
            Seen::Unlisted => return Ok(()),
            Seen::Quiet | Seen::IssueClosed => {}
            Seen::Ended => {
                let mut next = self.state.clone();
                let withdrawn = end_closed(&mut next, ticket).flatten();
                self.save(next)?;
                self.say_closed(ticket, withdrawn);
            }
            Seen::Covered(number) => {
                let mut next = self.state.clone();
                let withdrawn = end_covered(&mut next, ticket).flatten();
                self.save(next)?;
                self.say_covered(ticket, number, withdrawn);
            }
            Seen::Pull(_, PullRequestState::Open) => {}
            Seen::Pull(number, PullRequestState::Merged) => {
                let mut next = self.state.clone();
                let withdrawn = end_merged(&mut next, ticket).flatten();
                self.save(next)?;
                self.say_merged(ticket, number, withdrawn);
            }
            Seen::Pull(number, PullRequestState::Closed) => {
                let on_closed = self.state.item(ticket).is_some_and(|item| {
                    self.parked_on(item, |k| matches!(k, RulingKind::Stuck(Stuck::Closed)))
                });
                if !on_closed {
                    self.park_closed(ticket, number)?;
                }
            }
        }
        self.parked_reads.insert(ticket, self.ports.clock.now());
        Ok(())
    }

    // Parks the work item for `ticket` on a `closed` ruling about its pull request `number`
    fn park_closed(&mut self, ticket: TicketId, number: u64) -> Result<(), StateError> {
        let mut next = self.state.clone();
        next.rulings.retain(|r| r.issue != Some(ticket));
        let kind = Stuck::Closed.into();
        let (id, _) = park(self.names(), &mut next, ticket, Some(number), kind);
        self.save(next)?;
        let note =
            format!("pull request #{number} was closed without merging, so ruling {id} asks");
        self.notes.push(note);
        if let Some(why) = self.post_ruling(Some(number), id) {
            let note = format!("cannot post ruling {id} on its thread: {why}");
            self.notes.push(note);
        }
        Ok(())
    }
}
