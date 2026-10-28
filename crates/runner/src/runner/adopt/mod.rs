//! Adopting an open pull request kelpie didn't open
//!
//! The maintainer adopts one with `adopt <pr>`, or with `ready-for-agent` on
//! one whose branch isn't `kelpie/N`. They wait in the state file for a free
//! slot, one at a time. They go before the board's issues. Each starts on its
//! branch as `origin` holds it. A pass of the project's review bots comes
//! first, or CI when it lists none. A review asking for changes starts at a
//! worker's turn instead. Only a push the worker's worktree holds is the
//! worker's. A fork is adopted only when its contributor released it (ADR 0010).

use std::fmt;
use std::path::{Path, PathBuf};

use super::Runner;
use super::add::AddError;
use super::pace::{Passed, Started};
use super::report::{Begin, StepReport};
use super::rework::review_text;
use super::trigger;
use super::turn;
use crate::board::{HUMAN, OpenPullRequest, READY, Skip};
use crate::ports::{
    ForgeError, Issue, MaintainerReview, PullRequestState, Reviewed, Role, Timestamp,
};
use crate::settings::AgentName;
use crate::state::{How, Ruling, RulingKind, StateError, Stuck, Waiting};
use crate::ticket::TicketId;
use crate::work_item::{Known, Phase, Review, Turn, WorkItem, new_session_id};
use crate::worktree::{self, Remote, Start};

/// The file in the build folder that tells the worker what it adopted
const ADOPTED_FILE: &str = "adopted-pull-request.md";

/// An adopted pull request that fails to start on every pass
#[derive(Debug)]
pub(super) struct Failing {
    // The first pass that failed
    since: Timestamp,
    // The error its ruling was last raised for
    raised: Option<String>,
}

/// Why a pull request cannot be adopted
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptError {
    /// The forge could not show the pull request
    PullRequest(u64, ForgeError),
    /// The pull request is merged or closed, as named
    NotOpen(u64, &'static str),
    /// The pull request's branch is on a fork, not the repo itself
    Fork(u64),
    /// The pull request's branch is on a fork that kelpie cannot push to: its
    /// owner does not allow edits by maintainers, deleted it, or is an
    /// organization, whose forks GitHub does not let maintainers push to
    NoEdits(u64),
    /// The pull request merges into the first branch, not the project's base
    /// branch, the second
    Base(u64, String, String),
    /// The pull request was opened by this login, not the account kelpie acts as
    Author(u64, String),
    /// The pull request names no issue on the repo that it closes
    NoIssue(u64),
    /// A work item for the issue it closes is open
    InFlight(TicketId),
    /// The forge could not say which account kelpie acts as
    Viewer(ForgeError),
    /// The forge could not show the pull request's issue
    Issue(u64, ForgeError),
    /// The issue's `agent:` label cannot be used, or the agent board named no agent
    Agent(AddError),
    /// No random session id could be drawn, with the OS's reason
    Session(String),
    /// The worktree could not be prepared on the branch, with the reason
    Worktree(String),
    /// The repo's worktrees could not be listed, with the reason
    Holder(String),
    /// The forge could not show the review bot's reviews of the pull request
    ReviewBot(String, u64, ForgeError),
    /// The file for the worker could not be written, with the reason
    File(String),
    /// A label could not be taken off the pull request
    Unlabel(u64, String, ForgeError),
    /// `finish` holds the board back, so nothing new is taken on
    Finishing,
    /// The project's tickets are local, and an adopted pull request closes a
    /// GitHub issue
    LocalStore,
    /// The change could not be saved
    State(StateError),
}

impl fmt::Display for AdoptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PullRequest(number, e) => write!(f, "cannot read pull request #{number}: {e}"),
            Self::NotOpen(number, state) => write!(f, "pull request #{number} is {state}"),
            Self::Fork(number) => write!(f, "pull request #{number} comes from a fork"),
            Self::NoEdits(number) => write!(
                f,
                "pull request #{number} comes from a fork kelpie cannot push to: it does not let \
                 maintainers edit it, it has been deleted, or an organization owns it"
            ),
            Self::Base(number, merges_into, base) => write!(
                f,
                "pull request #{number} merges into `{merges_into}`, not `{base}`"
            ),
            Self::Author(number, login) => {
                write!(
                    f,
                    "pull request #{number} was opened by {login}, not by kelpie's account"
                )
            }
            Self::NoIssue(number) => {
                write!(f, "pull request #{number} names no issue it closes")
            }
            Self::InFlight(ticket) => {
                write!(f, "the work item for {} is in flight", ticket.shown())
            }
            Self::Viewer(e) => write!(f, "cannot read the account kelpie acts as: {e}"),
            Self::Issue(issue, e) => write!(f, "cannot read issue #{issue}: {e}"),
            Self::Agent(e) => e.fmt(f),
            Self::Session(e) => write!(f, "cannot draw a session id: {e}"),
            Self::Worktree(e) => write!(f, "cannot prepare its worktree: {e}"),
            Self::Holder(e) => write!(f, "cannot list the worktrees of the repo: {e}"),
            Self::ReviewBot(bot, number, e) => {
                write!(f, "cannot read {bot}'s reviews of #{number}: {e}")
            }
            Self::File(e) => f.write_str(e),
            Self::Unlabel(number, label, e) => {
                write!(f, "cannot take the `{label}` label off #{number}: {e}")
            }
            Self::Finishing => f.write_str(super::finishing::NOTHING_NEW),
            Self::LocalStore => f.write_str(
                "adoption needs GitHub issues, and this project's tickets are local \
                 (`tickets.source = \"local\"`)",
            ),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for AdoptError {}

impl AdoptError {
    // Whether asking again, with nothing changed on the pull request, is
    // refused the same way
    fn settled(&self) -> bool {
        matches!(
            self,
            Self::NotOpen(..)
                | Self::Fork(_)
                | Self::NoEdits(_)
                | Self::Base(..)
                | Self::Author(..)
                | Self::NoIssue(_)
                | Self::Agent(AddError::Label(_))
        )
    }
}

impl Runner {
    /// Adopts open pull request `number`, which waits for a free slot and
    /// any adopted before it
    ///
    /// Adopting one already waiting or in flight changes nothing.
    ///
    /// # Errors
    ///
    /// [`AdoptError`] naming why the pull request cannot be adopted, which
    /// also goes to it as a comment, or why the change cannot be saved.
    /// Nothing is adopted then.
    pub fn adopt(&mut self, number: u64) -> Result<(), AdoptError> {
        if !self.adopt_wanted(number)? {
            return Ok(());
        }
        let pr = (self.ports.forge.reviewed(&self.remote, number))
            .map_err(|e| AdoptError::PullRequest(number, e))?;
        self.adopt_found(number, &pr)
    }

    // Whether `number` is yet to be adopted, which is not so while it waits or
    // is in flight, before the pull request is read.
    pub(super) fn adopt_wanted(&self, number: u64) -> Result<bool, AdoptError> {
        if !self.picks() {
            return Err(AdoptError::Finishing);
        }
        if self.local_store() {
            return Err(AdoptError::LocalStore);
        }
        let in_flight = (self.state.work_items.iter()).any(|i| i.pull_request == Some(number));
        Ok(!(in_flight || self.waiting(number)))
    }

    // Adopts `number` from the pull request as read. What was checked before
    // the read is checked again, since the runner may have moved since.
    pub(super) fn adopt_found(&mut self, number: u64, pr: &Reviewed) -> Result<(), AdoptError> {
        if !self.adopt_wanted(number)? {
            return Ok(());
        }
        if let Err(e) = self.adoptable(number, pr) {
            if e.settled() {
                let _ = self.refusal_comment(number, &e);
            }
            return Err(e);
        }
        let mut next = self.state.clone();
        next.adopted.push(Waiting {
            pull_request: number,
            by_label: false,
        });
        self.save(next).map_err(AdoptError::State)
    }

    fn waiting(&self, number: u64) -> bool {
        self.state.adopted.iter().any(|w| w.pull_request == number)
    }

    // Queues each open pull request labelled `ready-for-agent` whose branch isn't
    // `kelpie/N`. Starts the oldest that can. A refusal takes the label off and
    // posts on the pull request. Returns each that could not start, and the
    // first the pacer held.
    pub(super) fn adopt_waiting(&mut self, open: &[OpenPullRequest]) -> Result<Passed, StateError> {
        let mut skipped = Vec::new();
        // A queue saved before the project moved to the local store is let go,
        // since an adopted pull request closes a GitHub issue.
        if self.local_store() {
            for waiting in self
                .state
                .adopted
                .iter()
                .map(|w| w.pull_request)
                .collect::<Vec<_>>()
            {
                self.let_go(waiting)?;
            }
            return Ok((None, skipped, None));
        }
        if let Some(mut labelled) = self.labelled_for_adoption(open) {
            labelled.sort_unstable();
            let mut next = self.state.clone();
            next.adopted
                .retain(|w| !w.by_label || labelled.contains(&w.pull_request));
            for number in labelled {
                if !next.adopted.iter().any(|w| w.pull_request == number) {
                    next.adopted.push(Waiting {
                        pull_request: number,
                        by_label: true,
                    });
                }
            }
            if next.adopted != self.state.adopted {
                self.save(next)?;
            }
        }
        let waiting: Vec<u64> = self.state.adopted.iter().map(|w| w.pull_request).collect();
        self.forget_unwaiting(&waiting)?;
        // A pull request whose agent's account holds waits in the queue, and the ones
        // behind it still start.
        let mut first_hold = None;
        for number in waiting {
            if let Some(held) = self.pace_dispatch()?.holds() {
                return Ok((Some(held), skipped, None));
            }
            let begin = match self.start_adoption(number) {
                Ok(Started::Held(held)) => {
                    first_hold.get_or_insert(*held);
                    continue;
                }
                Ok(Started::Opened((issue, agent))) => {
                    self.adopt_failing.remove(&number);
                    Begin::Report(StepReport::Adopted {
                        issue,
                        pull_request: number,
                        agent,
                    })
                }
                Err(AdoptError::State(e)) => return Err(e),
                Err(AdoptError::NotOpen(..)) => {
                    self.let_go(number)?;
                    continue;
                }
                // It waits for the work item on its issue to end.
                Err(AdoptError::InFlight(_)) => continue,
                Err(e) if e.settled() => match self.refuse_adoption(number, &e)? {
                    Ok(begin) => begin,
                    Err(e) => {
                        skipped.push(self.adopt_failed(number, &e)?);
                        continue;
                    }
                },
                Err(e) => {
                    skipped.push(self.adopt_failed(number, &e)?);
                    continue;
                }
            };
            return Ok((Some(begin), skipped, None));
        }
        Ok((None, skipped, first_hold))
    }

    // Notes that adopted pull request `number` could not start this pass, and
    // since when it has not. A worktree that cannot be prepared raises a
    // ruling, once for each error it reads, which starting it clears.
    fn adopt_failed(&mut self, number: u64, error: &AdoptError) -> Result<Skip, StateError> {
        let now = self.ports.clock.now();
        let failing = self.adopt_failing.entry(number).or_insert(Failing {
            since: now,
            raised: None,
        });
        let since = failing.since;
        if matches!(error, AdoptError::Worktree(_)) {
            let why = error.to_string();
            // An answered ruling is gone, so what was raised is also kept here.
            let known = failing.raised.as_ref() == Some(&why);
            let saved = self
                .adopt_ruling(number)
                .is_some_and(|r| failed_with(r, &why));
            if !known && !saved {
                let mut next = self.state.clone();
                next.rulings.retain(|r| !failed_on(r, number));
                let kind = RulingKind::Stuck(Stuck::AdoptFailed { why: why.clone() });
                let question = super::ruling::question(
                    TicketId::from_pull_request(number),
                    Some(number),
                    &kind,
                );
                next.rulings.push(Ruling {
                    pull_request: Some(number),
                    raised: Some(now),
                    ..Ruling::new(TicketId::from_pull_request(number), question, kind)
                });
                self.save(next)?;
            }
            // Only once saved, so a save that failed is tried again.
            if let Some(failing) = self.adopt_failing.get_mut(&number) {
                failing.raised = Some(why);
            }
        }
        Ok(Skip::Adopt {
            pull_request: number,
            error: error.to_string(),
            since,
        })
    }

    fn adopt_ruling(&self, number: u64) -> Option<&Ruling> {
        self.state.rulings.iter().find(|r| failed_on(r, number))
    }

    // Lets go of what a pull request that no longer waits left behind: the
    // time it began failing and the ruling that said so.
    fn forget_unwaiting(&mut self, waiting: &[u64]) -> Result<(), StateError> {
        self.adopt_failing.retain(|n, _| waiting.contains(n));
        let stale = |r: &Ruling| r.pull_request.is_some_and(|n| !waiting.contains(&n));
        if self.state.rulings.iter().any(|r| failed(r) && stale(r)) {
            let mut next = self.state.clone();
            next.rulings.retain(|r| !(failed(r) && stale(r)));
            self.save(next)?;
        }
        Ok(())
    }

    // The labelled pull requests whose branch isn't `kelpie/N` that adoption
    // queues. Someone else's, a draft included, is left to the contributions.
    // When the logins that tell one cannot be read, the queue stays as it is.
    // The local store has none: an adopted pull request closes a GitHub issue.
    fn labelled_for_adoption(&mut self, open: &[OpenPullRequest]) -> Option<Vec<u64>> {
        if self.local_store() {
            return None;
        }
        let own = match self.settings.contributions.is_off() {
            true => None,
            false => Some(self.own_logins().ok()?),
        };
        let theirs = |pr: &OpenPullRequest| {
            own.as_ref()
                .is_some_and(|own| self.by_someone_else(pr, own))
        };
        let labelled = open
            .iter()
            .filter(|pr| pr.labels.iter().any(|l| l == READY))
            .filter(|pr| {
                let kelpies = pr.head.strip_prefix("kelpie/").and_then(trigger::number);
                kelpies.is_none()
            })
            .filter(|pr| !theirs(pr))
            .map(|pr| pr.number)
            .collect();
        Some(labelled)
    }

    // The outer error is a save that failed; the inner, a label that would
    // not come off, which keeps it waiting for the next poll to refuse.
    fn refuse_adoption(
        &mut self,
        number: u64,
        refused: &AdoptError,
    ) -> Result<Result<Begin, AdoptError>, StateError> {
        let repo = &self.remote;
        let unlabelled = self.ports.forge.pull_request(repo, number).and_then(|pr| {
            if pr.labels.iter().any(|l| l == READY) {
                self.ports.forge.set_label(repo, number, READY, false)
            } else {
                Ok(())
            }
        });
        if let Err(e) = unlabelled {
            return Ok(Err(AdoptError::Unlabel(number, READY.to_owned(), e)));
        }
        self.let_go(number)?;
        let comment_failed = self.refusal_comment(number, refused);
        Ok(Ok(Begin::Report(StepReport::AdoptRefused {
            pull_request: number,
            reason: refused.to_string(),
            comment_failed,
        })))
    }

    fn let_go(&mut self, number: u64) -> Result<(), StateError> {
        let mut next = self.state.clone();
        next.adopted.retain(|w| w.pull_request != number);
        next.rulings.retain(|r| !failed_on(r, number));
        self.adopt_failing.remove(&number);
        self.save(next)
    }

    fn refusal_comment(&self, number: u64, refused: &AdoptError) -> Option<String> {
        let comment = format!("Kelpie cannot adopt this pull request: {refused}.");
        let posted = self.comment_on_issue(number, &comment);
        posted.err().map(|e| e.to_string())
    }

    // What adopting an open pull request starts from. The issue is the lowest
    // one it closes, or the one kelpie filed for it. Kelpie's own pull request
    // in the repo is adopted. A released contributor's is, from a fork too
    // (ADR 0010).
    fn adoptable(&mut self, number: u64, pr: &Reviewed) -> Result<Adoptable, AdoptError> {
        match pr.state {
            PullRequestState::Open => {}
            PullRequestState::Merged => return Err(AdoptError::NotOpen(number, "merged")),
            PullRequestState::Closed => return Err(AdoptError::NotOpen(number, "closed")),
        }
        let released = (self.state.contributions.iter())
            .find(|c| c.pull_request == number && c.released.is_some())
            .cloned();
        if pr.from_fork && released.is_none() {
            return Err(AdoptError::Fork(number));
        }
        if pr.base != self.settings.git.base() {
            return Err(AdoptError::Base(
                number,
                pr.base.clone(),
                self.settings.git.base().to_owned(),
            ));
        }
        if released.is_none() {
            let me = self.viewer().map_err(AdoptError::Viewer)?;
            if pr.author != me {
                let login = if pr.author.is_empty() {
                    "a deleted account".to_owned()
                } else {
                    pr.author.clone()
                };
                return Err(AdoptError::Author(number, login));
            }
        }
        if !pr.pushable() {
            return Err(AdoptError::NoEdits(number));
        }
        // A contribution whose pull request closes no issue takes the one
        // kelpie filed for it (`contribution-release.md`). That one holds the
        // work item even after the pull request closes another: the filed one
        // is the one the pull request's body names.
        let filed = (released.as_ref())
            .and_then(|c| c.adopting.as_ref())
            .map(|a| a.issue);
        let issue = match (filed, pr.closes.iter().min().copied()) {
            (Some(issue), _) | (None, Some(issue)) => issue,
            (None, None) => return Err(AdoptError::NoIssue(number)),
        };
        // Adoption is GitHub's alone, so the ticket is the issue's number.
        let ticket = TicketId::from_issue(issue).ok_or(AdoptError::NoIssue(number))?;
        // A fork's branch is named by its contributor, so it could be `main`:
        // the worktree's branch is kelpie's own for the issue.
        let (branch, remote) = match (&pr.head_clone_url, pr.from_fork) {
            (Some(url), true) => (
                format!("kelpie/{issue}"),
                Remote::Fork {
                    url: url.clone(),
                    branch: pr.branch.clone(),
                },
            ),
            _ => (pr.branch.clone(), Remote::Origin),
        };
        // The worktree kelpie keeps for the issue is cleared at the start, so
        // only another one holding the branch stops it.
        let own = self.paths.worktree(ticket);
        let holder = worktree::holder_of(&self.settings.git.checkout, &branch, &own);
        if let Some(holder) = holder.map_err(|e| AdoptError::Holder(e.to_string()))? {
            return Err(AdoptError::Worktree(format!(
                "`{branch}` is checked out in the worktree at {}, which kelpie did not make: \
                 remove it or switch it off the branch",
                holder.display()
            )));
        }
        Ok(Adoptable {
            issue,
            branch,
            remote,
            released,
        })
    }

    // Checks the pull request can be adopted, then prepares its worktree,
    // writes what the worker needs, takes the triage and summon labels off
    // and saves the work item, in that order.
    fn start_adoption(
        &mut self,
        number: u64,
    ) -> Result<Started<(TicketId, AgentName)>, AdoptError> {
        let repo = self.remote.clone();
        let pr = self
            .ports
            .forge
            .reviewed(&repo, number)
            .map_err(|e| AdoptError::PullRequest(number, e))?;
        let Adoptable {
            issue,
            branch,
            remote,
            released,
        } = self.adoptable(number, &pr)?;
        let ticket = TicketId::from_issue(issue).ok_or(AdoptError::NoIssue(number))?;
        if self.state.item(ticket).is_some() {
            return Err(AdoptError::InFlight(ticket));
        }
        let found = self
            .ports
            .tickets
            .issue(&repo, ticket)
            .map_err(|e| AdoptError::Issue(issue, e))?;
        let ranked = self
            .board_ranked(ticket, &found.labels)
            .map_err(AdoptError::Agent)?;
        // The first agent the board ranks that the pacer lets start, as dispatch walks it.
        let opening = match self.paced_opening(ranked).map_err(AdoptError::State)? {
            Started::Opened(opening) => opening,
            Started::Held(held) => return Ok(Started::Held(held)),
        };
        let (agent, note) = (opening.agent, opening.note);
        let session = new_session_id().map_err(|e| AdoptError::Session(e.to_string()))?;
        let mut fresh = self.fresh(ticket, found.title.clone(), agent.clone(), session);
        fresh.pinned = opening.pinned;
        // A start that failed part way leaves a worktree on a head `origin`
        // may have moved past, so each start begins from a fresh one.
        let removed = worktree::remove(
            &self.settings.git.checkout,
            &fresh.worktree,
            &branch,
            &fresh.build,
            None,
        );
        removed.map_err(|e| AdoptError::Worktree(e.to_string()))?;
        self.remove_code_graph(ticket)
            .map_err(AdoptError::Worktree)?;
        let prepared = worktree::prepare(
            &self.settings.git.checkout,
            &fresh.worktree,
            &branch,
            Start::Pushed,
            self.item_base(&fresh),
            &remote,
            &fresh.build,
        );
        prepared.map_err(|e| AdoptError::Worktree(e.to_string()))?;
        let head = worktree::origin_head(&self.settings.git.checkout, &branch, &remote)
            .map_err(|e| AdoptError::Worktree(e.to_string()))?;
        // A listed bot's `rounds` counts the reviews it gave so far. One of
        // this head counts when the bot's round finds it, as any read does.
        let listed = self.listed_bots();
        let mut bot_reads = std::collections::BTreeMap::new();
        for bot in &listed {
            let bot = self.profile(bot.bot);
            let activity = self
                .ports
                .forge
                .review_bot(&repo, number, bot.login())
                .map_err(|e| AdoptError::ReviewBot(bot.name().to_owned(), number, e))?;
            let reads = bot.reviewed_besides(&activity, &head);
            if reads > 0 {
                bot_reads.insert(bot.bot(), reads);
            }
        }
        let review = pr
            .review
            .as_ref()
            .filter(|r| r.changes_requested && !self.state.reworked.contains(&r.id))
            .cloned();
        // What the maintainer answered a contribution's merge ruling `rework`
        // with, which the worker reads before its first turn. It sits on the
        // record and not on the answer, so a contributor who pushes after the
        // answer leaves it there.
        let rework_note = (released.as_ref()).and_then(|c| c.rework.clone());
        let taken_over = (released.as_ref())
            .is_some_and(|c| c.released.as_ref().is_some_and(|r| r.how == How::Rework));
        let text = adopted_text(
            number,
            &pr,
            issue,
            &found,
            review.as_ref(),
            rework_note.as_deref(),
            taken_over,
        );
        turn::write(&fresh.build, &adopted_path(&fresh.build), &text).map_err(AdoptError::File)?;
        // With the summon labels off, no push summons a review bot outside the lease.
        let mut labels = pr.labels;
        let summons = self.ports.review_bots.iter().filter_map(|bot| bot.label());
        for label in [READY, HUMAN].into_iter().chain(summons) {
            if labels.iter().any(|l| l == label) {
                self.ports
                    .forge
                    .set_label(&repo, number, label, false)
                    .map_err(|e| AdoptError::Unlabel(number, label.to_owned(), e))?;
                labels.retain(|l| l != label);
            }
        }
        let now = self.ports.clock.now();
        let mut next = self.state.clone();
        next.adopted.retain(|w| w.pull_request != number);
        next.rulings.retain(|r| !failed_on(r, number));
        let (turn, phase, resume) = match &review {
            Some(review) => {
                next.reworked.push(review.id.clone());
                let first = Some(Phase::Review(Review::first()));
                (Turn::Due, Phase::Implement, first)
            }
            // The maintainer answered the merge ruling `rework`: a worker
            // changes the pull request first. The project's full reviewers read
            // what it pushed. A pass over the contributor's head could come back
            // clean and undo that change.
            None if taken_over => {
                let pass = Some(Phase::Review(Review::first()));
                (Turn::Due, Phase::Implement, pass)
            }
            // A released contribution is kelpie's own work item now, so the
            // project's full reviewers read it, not the bots' owed read alone.
            None if released.is_some() => {
                let pass = Phase::Review(Review::first());
                (Turn::Ended { at: now }, pass, None)
            }
            // A listed bot owes it a read of kelpie's own before CI.
            None if !listed.is_empty() => {
                let bots = Phase::Review(Review {
                    bots_only: true,
                    ..Review::first()
                });
                (Turn::Ended { at: now }, bots, None)
            }
            None => {
                let ci = Phase::Ci {
                    head: None,
                    since: now,
                };
                (Turn::Ended { at: now }, ci, None)
            }
        };
        let mut item = WorkItem {
            branch,
            remote,
            adopted: true,
            contributor: (released.as_ref()).map(|c| c.author.clone()),
            arrived: Some(head.clone()),
            pull_request: Some(number),
            turn,
            phase,
            resume,
            known: Known {
                labels,
                ready: !pr.draft,
                head: Some(head),
            },
            ..fresh
        };
        item.bot_reads = bot_reads;
        item.summons_owed = listed.iter().map(|bot| bot.bot).collect();
        next.work_items.push(item);
        self.save(next).map_err(AdoptError::State)?;
        self.mark_held(ticket, true);
        self.notes.extend(note);
        Ok(Started::Opened((ticket, agent)))
    }
}

impl Runner {
    /// The branch's head on `origin`, as the worker's own push
    ///
    /// On an adopted branch its owner may push too, so the head counts only
    /// when the worktree holds it. `None` keeps the head kelpie knew, which
    /// errs toward parking.
    pub(super) fn own_push(&self) -> Option<String> {
        let item = self.current()?;
        let head = self.origin_head().ok()?;
        if !item.adopted {
            return Some(head);
        }
        let checked_out = worktree::head(&self.settings.git.checkout, &item.worktree).ok()?;
        (checked_out == head).then_some(head)
    }

    // A push by anyone else to an adopted branch since kelpie last looked
    // goes to the gate, which parks it, before a worker's turn builds on it.
    pub(super) fn pushed_by_someone_else(&mut self) -> Result<Option<Begin>, StateError> {
        let Some(item) = self.current() else {
            return Ok(None);
        };
        let due = matches!(item.turn, Turn::Due | Turn::Next { .. });
        let Some(known) = item.known.head.clone().filter(|_| item.adopted && due) else {
            return Ok(None);
        };
        if self.origin_head().is_ok_and(|head| head == known) {
            return Ok(None);
        }
        let now = self.ports.clock.now();
        self.update(|item| {
            item.turn = Turn::Ended { at: now };
            item.resume = None;
            item.phase = Phase::Ci {
                head: None,
                since: now,
            };
        })?;
        self.check_ci().map(Some)
    }
}

// Whether `ruling` says an adopted pull request could not start.
fn failed(ruling: &Ruling) -> bool {
    matches!(ruling.kind, RulingKind::Stuck(Stuck::AdoptFailed { .. }))
}

fn failed_on(ruling: &Ruling, number: u64) -> bool {
    failed(ruling) && ruling.pull_request == Some(number)
}

// Whether `ruling` already names `why`.
fn failed_with(ruling: &Ruling, why: &str) -> bool {
    matches!(&ruling.kind, RulingKind::Stuck(Stuck::AdoptFailed { why: said }) if said == why)
}

// What an adoption starts from
struct Adoptable {
    // The issue the pull request closes, or the one kelpie filed for it
    issue: u64,
    // The work item's branch, in the worktree
    branch: String,
    // Where the pull request's branch lives
    remote: Remote,
    // The contribution's record, for a released one: what its reviewers found
    // and what the maintainer answered `rework` with
    released: Option<crate::state::Contribution>,
}

/// Whether the adopted work item's worker has yet to begin its session
pub(super) fn unborn(item: &WorkItem) -> bool {
    item.adopted
        && !item
            .calls
            .iter()
            .any(|c| c.role == Role::Worker && c.session == item.session)
}

// The worker can read its build folder, and a commit never carries it.
fn adopted_path(build: &Path) -> PathBuf {
    build.join(ADOPTED_FILE)
}

// The pull request's, the issue's and the reviewer's words go in as written.
fn adopted_text(
    number: u64,
    pr: &Reviewed,
    issue: u64,
    found: &Issue,
    review: Option<&MaintainerReview>,
    note: Option<&str>,
    taken_over: bool,
) -> String {
    let mut text = format!("# Pull request #{number}: {}\n", pr.title);
    push_block(&mut text, &pr.body);
    text.push_str(&format!("\n# Issue #{issue}: {}\n", found.title));
    push_block(&mut text, &found.body);
    if let Some(review) = review {
        text.push('\n');
        text.push_str(&review_text(number, review));
    }
    if let Some(note) = note {
        text.push_str(&format!(
            "\n# The maintainer's note on pull request #{number}\n\n{note}\n"
        ));
    }
    if taken_over && note.is_none() {
        text.push_str(
            "\n# Kelpie took this pull request over\n\nThe maintainer answered its merge \
             ruling `rework`, so a worker finishes it and the project's reviewers read what \
             the worker pushed.\n",
        );
    }
    text
}

fn push_block(text: &mut String, words: &str) {
    if words.trim().is_empty() {
        return;
    }
    text.push('\n');
    text.push_str(words);
    if !words.ends_with('\n') {
        text.push('\n');
    }
}

/// The first turn of an adopted work item, ahead of `then`, what the gate
/// sends it, or the review to fix when there is nothing else
pub(super) fn first_prompt(item: &WorkItem, then: Option<&str>) -> String {
    let number = item.pull_request.unwrap_or_default();
    let mut prompt = format!(
        "Your work item adopts pull request #{number} for issue #{}: {}\n\n\
         You did not write its code. {} holds the pull request's title and body, \
         its issue, and the review to fix, if it has one. Your branch is the pull \
         request's as `origin` holds it. The pull request is already open, so do \
         not open another.\n\n",
        item.issue,
        item.title,
        adopted_path(&item.build).display()
    );
    match then {
        Some(then) => prompt.push_str(then),
        None => {
            prompt.push_str(
                "Make the changes the review asks for, then commit and push with \
                 `git push origin HEAD`. ",
            );
            prompt.push_str(turn::KEEP_BODY_TRUE);
        }
    }
    prompt.push('\n');
    prompt
}

#[cfg(test)]
pub(in crate::runner) mod released;
#[cfg(test)]
mod tests;
