//! The triage poll's reads, made with the runner's lock let go
//!
//! A poll lists the open issues, reads each issue whose triage waits on the
//! author's own session, and where the pass may need them lists the open pull
//! requests. [`read_triage`] asks the forge for all of it before the pass takes
//! the lock, so a trigger sent meanwhile is answered: [`Runner::triage_poll_job`]
//! decides a poll is due, [`PollJob::run`] reads, and [`Runner::apply_poll`] acts
//! on what was read, on the state it finds then. The pull requests are read only
//! to take back a hand-labelled issue ([`super::hand`]) or to tell the triager
//! what is open. The listing and pull requests stay as the poll's snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use super::super::Runner;
use super::super::trigger::lock;
use super::{opened_by_us_or_people, ticket, untouched};
use crate::board::{READY, check};
use crate::ports::{Forge, ForgeError, Issue, OpenIssue, OpenPull, Tickets, Timestamp};
use crate::settings::{ForgeSlug, TriagePick};
use crate::state::{CheckTriage, StateError, TriageStep};

/// A poll the runner decided is due, which needs no lock
pub(in crate::runner) struct PollJob {
    tickets: Arc<dyn Tickets>,
    forge: Arc<dyn Forge>,
    repo: ForgeSlug,
    /// The issues whose triage waits on the author's session, to read one by one
    local: Vec<u64>,
    /// The issues kelpie took back, whose swap is not yet seen done
    taken: BTreeSet<u64>,
    /// Whether a hand-labelled issue may be taken back
    take_back: bool,
    /// Whether the triager may be started: an issue is queued or a wait is open
    research: bool,
    /// `triage.since` and `pruned_through`, while `triage.pick` is not `off`
    triage: Option<(Timestamp, u64)>,
    /// The verb's asks when the job was built, to tell a read that predates one
    asked: u64,
    /// The key of each issue's check result that needs no new check
    checked: BTreeMap<u64, u64>,
    /// The App's login, whose issues are read as a person's
    app: Option<String>,
}

/// What a poll read
pub(in crate::runner) struct PollRead {
    open: Vec<OpenIssue>,
    /// The open pull requests, when the poll had a use for them
    pulls: Option<Result<Vec<OpenPull>, ForgeError>>,
    /// Each waiting issue as the forge shows it
    local: BTreeMap<u64, Result<Issue, ForgeError>>,
}

/// The open issues and pull requests the last poll read, which the triager's call
/// is made from
#[derive(Debug, Clone)]
pub(in crate::runner) struct Listing {
    /// The open issues, as the poll's take-back left their labels
    pub(super) open: Vec<OpenIssue>,
    /// The open pull requests, or why they could not be listed. None when the
    /// poll had no use for them.
    pub(super) pulls: Option<Result<Vec<OpenPull>, ForgeError>>,
}

/// Reads the triage poll, if one is due, asking the forge with the runner's lock
/// let go, and acts on it
pub(in crate::runner) fn read_triage(runner: &Mutex<Runner>) -> Result<(), StateError> {
    let job = lock(runner).triage_poll_job();
    let Some((now, job)) = job else {
        return Ok(());
    };
    let asked = job.asked;
    let read = job.run();
    lock(runner).apply_poll(now, asked, read)
}

impl PollJob {
    // Lists the open issues, then reads what the poll needs of them. A listing
    // that cannot be read fails the whole read, as it leaves nothing to act on.
    fn run(self) -> Result<PollRead, ForgeError> {
        let open = self.tickets.open_issues(&self.repo)?;
        let local = (self.local.iter())
            .map(|&issue| {
                let read = ticket(issue).and_then(|id| self.tickets.issue(&self.repo, id));
                (issue, read)
            })
            .collect();
        let pulls = self
            .pulls_used(&open)
            .then(|| self.forge.open_pulls(&self.repo));
        Ok(PollRead { open, pulls, local })
    }

    // Whether the pass has a use for the open pull requests
    fn pulls_used(&self, open: &[OpenIssue]) -> bool {
        let hand_labelled = self.take_back
            && (open.iter())
                .any(|i| i.labels.iter().any(|l| l == READY) || self.taken.contains(&i.number));
        let app = self.app.as_deref();
        // Every issue the pick rule may list, whoever opened it: a superset of
        // what the apply lists, which decides it from the state it finds.
        let to_triage = self.research
            || self.triage.is_some_and(|(since, pruned_through)| {
                (open.iter()).any(|i| untouched(i, since, pruned_through, None, true, app))
            });
        // The check makes the maintainer's passing issue ready only where no
        // open pull request closes it.
        let to_check = (open.iter()).any(|i| {
            let key = check::key(&i.title, &i.body, &i.labels);
            opened_by_us_or_people(i, app)
                && self.checked.get(&i.number) != Some(&key)
                && check::check(&i.title, &i.body, &i.labels).is_empty()
        });
        hand_labelled || to_triage || to_check
    }
}

impl Runner {
    // Whether `issue` carries `ready-for-agent` and is being worked on: it has
    // a work item, or an open pull request closes it, which pull requests that
    // were not read may. One taken back keeps it until its swap is done.
    fn ready_at_work(
        &self,
        issue: &OpenIssue,
        pulls: Option<&Result<Vec<OpenPull>, ForgeError>>,
    ) -> bool {
        let closed = || match pulls {
            Some(Ok(pulls)) => (pulls.iter()).any(|pull| pull.closes.contains(&issue.number)),
            _ => true,
        };
        let taken = self.taken_seen(issue.number).is_some();
        (issue.labels.iter().any(|l| l == READY))
            && (taken || self.has_work_item(issue.number) || closed())
    }

    // Decides whether the poll is read now: at most once a board poll. The issue
    // check reads it whatever `triage.pick` says, and the triager only while it is
    // not `off`. Returns the time and what the read needs.
    fn triage_poll_job(&self) -> Option<(Timestamp, PollJob)> {
        let now = self.ports.clock.now();
        if !self.looks.triage_due(now) {
            return None;
        }
        let triage = self.state.triage.as_ref();
        let waiting = self.triage_waiting();
        // No read under a call in flight: its comment is what ends the wait, and a
        // body read between the call and its comment is the body that call was made on.
        let local = (waiting.iter())
            .filter(|record| record.local.is_some())
            .map(|record| record.issue)
            .filter(|&issue| self.flights.triaging() != Some(issue) && !self.triage_given_up(issue))
            .collect();
        let taken = (triage.iter().flat_map(|t| t.issues.iter()))
            .filter(|record| record.step == TriageStep::Taken)
            .map(|record| record.issue)
            .collect();
        let job = PollJob {
            tickets: Arc::clone(&self.ports.tickets),
            forge: Arc::clone(&self.ports.forge),
            repo: self.remote.clone(),
            local,
            taken,
            take_back: triage.is_some() && self.settings.triage.hand_ready && self.app_covers(),
            research: triage.is_some() && (!self.triage_queue.is_empty() || !waiting.is_empty()),
            triage: triage.map(|t| (t.since, t.pruned_through)),
            asked: self.looks.triage_asks(),
            checked: (self.state.checks.iter())
                .filter(|c| c.triage != CheckTriage::Ran)
                .map(|c| (c.issue, c.key))
                .collect(),
            app: self.app_login(),
        };
        Some((now, job))
    }

    // Acts on what the poll read, on the state as it stands now. A listing that
    // failed or the verb outdated leaves the look due while triage is on, so no
    // triager starts from no listing. The same poll retries a failed issue and
    // finishes the triage a restart cut short.
    fn apply_poll(
        &mut self,
        now: Timestamp,
        asked: u64,
        read: Result<PollRead, ForgeError>,
    ) -> Result<(), StateError> {
        let Ok(PollRead {
            mut open,
            pulls,
            local,
        }) = read
        else {
            self.triage_run.listing = None;
            // With no triager to start, the check waits for the next poll.
            if self.state.triage.is_none() {
                self.looks.triage_listed(now);
            }
            return Ok(());
        };
        // The verb queued an issue while the listing was read, so it may predate
        // the issue: it is used for nothing and the next pass lists again.
        if self.looks.triage_asks() != asked {
            self.triage_run.listing = None;
            return Ok(());
        }
        self.looks.triage_listed(now);
        // A pass that ends in this poll is checked again from a listing made
        // after its end, which the next poll reads.
        let busy: BTreeSet<u64> = (open.iter())
            .map(|i| i.number)
            .filter(|&n| self.triage_busy(n))
            .collect();
        let Some((since, pruned_through)) =
            (self.state.triage.as_ref()).map(|t| (t.since, t.pruned_through))
        else {
            self.check_issues(&open, pulls.as_ref(), &busy);
            return Ok(());
        };
        self.flights.triage_retry();
        // A record that names no author, which is what a file saved before the
        // caps existed leaves, gets it from the listing: the author is what the
        // author's cap counts by and what tells the maintainer's own issues from
        // everyone else's.
        self.note_triage_authors(&open)?;
        // A wait on the author ends when the forge says the issue is closed or
        // carries a triage label the maintainer put on by hand, before anything
        // the record owes is written.
        self.settle_triage_waits(&open)?;
        // An author who took `local` answers by rewriting the issue body, which is
        // what the next pass is made on.
        self.watch_local_waits(&local)?;
        self.resume_triage()?;
        // The check reads the listing before the take-back and the due list.
        // Those reach only an issue it refused.
        self.check_issues(&open, pulls.as_ref(), &busy);
        let app = self.app_login();
        let only = match self.settings.triage.pick {
            TriagePick::Maintainer => match self.maintainer() {
                Ok(login) => Some(login),
                Err(_) => {
                    self.triage_run.listing = Some(Listing { open, pulls });
                    return Ok(());
                }
            },
            TriagePick::All | TriagePick::Off => None,
        };
        // A hand-labelled `ready-for-agent` the maintainer's own is swapped for
        // `needs-triage` here, before the board reads its pick, and listed as
        // the swap left it.
        self.take_back_ready(&mut open, pulls.as_ref(), since, pruned_through)?;
        // Worked out from the state as it stands, so an issue that got a record
        // while the listing was read is not listed as untouched.
        let mut due: Vec<&OpenIssue> = (open.iter())
            .filter(|i| {
                untouched(
                    i,
                    since,
                    pruned_through,
                    only.as_deref(),
                    self.check_owed(i.number),
                    app.as_deref(),
                ) && self.first_pass_owed(i.number)
                    && !self.ready_at_work(i, pulls.as_ref())
            })
            .collect();
        due.sort_by_key(|issue| (issue.created, issue.number));
        self.triage_due = due
            .into_iter()
            .map(|issue| super::DueStatus {
                issue: issue.number,
                title: issue.title.clone(),
                author: issue.author.clone(),
                author_association: issue.author_association,
                created: issue.created,
            })
            .collect();
        self.triage_run.listing = Some(Listing { open, pulls });
        Ok(())
    }
}
