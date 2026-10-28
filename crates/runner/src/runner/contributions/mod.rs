//! Following the pull requests other people open
//!
//! A contribution is an open, non-draft pull request whose author is not the
//! maintainer, kelpie's account or a bot; `contributions.pick` may also ask for
//! the `ready-for-agent` label. Its branch's name never decides.
//! Once a board poll, slot free or not, the runner records each contribution
//! with its head, reads each unread head for security ([`security`], [`ruling`]),
//! has the reviewers read a green one ([`pass`]), runs the quiet clock ([`clock`])
//! and posts a local pass ([`post`]). A call takes a slot while it runs, spends
//! against [`budget`] and goes ahead of the board ([`Runner::contribution_due`]).

mod adopt;
mod after;
mod answer;
mod bots;
mod budget;
mod ci;
pub(in crate::runner) mod clock;
mod heard;
mod merge;
mod pass;
pub(in crate::runner) mod post;
mod push;
mod release;
mod ruling;
mod security;

pub(super) use bots::waiting_on;
pub use clock::Followed;
pub(super) use push::Sent;
pub(super) use ruling::{
    ADOPT_ANYWAY, CLOSE_WITH_THANKS, KEEP_WAITING, LEAVE_OPEN, flagged_lines, keep_answer,
    raise_budget, rule_merge, settle, trust,
};

use crate::ports::{Agents, Ending, Reviewer};
use crate::state::StateError;

use super::Runner;
use super::report::Begin;
use crate::board::{OpenPullRequest, READY};
use crate::ports::ForgeError;
use crate::settings::Pick;
use crate::state::Contribution;

/// A call on a contribution's head, with everything it needs
pub(super) enum Job {
    /// The security review of the head
    Security(security::Job),
    /// The pass of the reviewers over the head
    Pass(pass::Job),
}

/// What a call on a contribution's head came to
#[derive(Debug)]
pub(super) enum Reading {
    /// A security review's
    Security(security::Reading),
    /// A pass's
    Pass(Box<pass::Walked>),
}

impl Job {
    /// The contribution it is on
    pub(super) fn pull_request(&self) -> u64 {
        match self {
            Self::Security(job) => job.pull_request,
            Self::Pass(job) => job.pull_request,
        }
    }

    /// What the thread it runs on is called
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Security(_) => "security",
            Self::Pass(_) => "pass",
        }
    }

    /// The head it reads
    pub(super) fn head(&self) -> &str {
        match self {
            Self::Security(job) => &job.head,
            Self::Pass(job) => &job.head,
        }
    }

    /// What the call leaves when it panics: the head unread, for the next poll
    pub(super) fn unread(&self) -> Reading {
        match self {
            Self::Security(_) => Reading::Security(security::Reading::default()),
            Self::Pass(_) => Reading::Pass(Box::default()),
        }
    }

    /// Runs the call to its end, or until `ending` is asked
    pub(super) fn run(
        self,
        agents: &dyn Agents,
        reviewer: &dyn Reviewer,
        ending: &Ending,
    ) -> Reading {
        match self {
            Self::Security(job) => Reading::Security(job.read(agents, ending)),
            Self::Pass(job) => Reading::Pass(Box::new(job.walk(agents, reviewer, ending))),
        }
    }
}

/// The two logins whose pull requests are not contributions
#[derive(Clone)]
pub(super) struct Logins {
    maintainer: String,
    viewer: String,
}

impl Logins {
    /// Whether `author` is the account kelpie acts as, whose pull request on another branch is
    /// the work item's own
    pub(super) fn acting(&self, author: &str) -> bool {
        author.eq_ignore_ascii_case(&self.viewer)
    }

    pub(super) fn own(&self, author: &str) -> bool {
        author.eq_ignore_ascii_case(&self.maintainer) || author.eq_ignore_ascii_case(&self.viewer)
    }
}

impl Runner {
    // The call on a contribution's head to start, if one is due: its security
    // review first, then the pass over a green head. See `security_due` and
    // `run_pass`.
    pub(super) fn contribution_due(&mut self) -> Result<Option<Begin>, StateError> {
        match self.security_due()? {
            Some(begin) => Ok(Some(begin)),
            None => self.run_pass(),
        }
    }

    // The logins whose pull requests are not contributions: the maintainer's,
    // and the account kelpie acts as
    pub(super) fn own_logins(&mut self) -> Result<Logins, ForgeError> {
        Ok(Logins {
            maintainer: self.maintainer()?,
            viewer: self.viewer()?,
        })
    }

    // Whether `pr` is a contribution under the project's `contributions.pick`
    pub(super) fn is_contribution(&self, pr: &OpenPullRequest, own: &Logins) -> bool {
        let labelled = pr.labels.iter().any(|l| l == READY);
        let picked = match self.settings.contributions.pick {
            Pick::Off => return false,
            Pick::Label => labelled,
            Pick::All => true,
        };
        let left = self.state.left_open.contains(&pr.number);
        // A pull request kelpie adopted is its own work item now, not someone
        // else's, and its record goes with it.
        let adopted = (self.state.work_items.iter()).any(|i| i.pull_request == Some(pr.number));
        picked && !pr.draft && !left && !adopted && self.by_someone_else(pr, own)
    }

    // Whether another person opened `pr`, ready or not: the contributions
    // pick, and adoption leaves alone, a draft of theirs too, so that it is
    // not refused before it is marked ready.
    pub(super) fn by_someone_else(&self, pr: &OpenPullRequest, own: &Logins) -> bool {
        !self.settings.contributions.is_off() && !pr.author_is_bot && !own.own(&pr.author)
    }

    // Brings the contributions the state records in line with the open pull
    // requests, once a poll. A listing or a login that cannot be read leaves
    // the records as they are, for the board's own read to report.
    pub(super) fn follow_contributions(&mut self) -> Result<(), StateError> {
        self.follow_open()?;
        // A pull request that left the record takes its checkout with it.
        self.remove_checkouts();
        Ok(())
    }

    fn follow_open(&mut self) -> Result<(), StateError> {
        if self.settings.contributions.is_off() {
            if self.state.contributions.is_empty() {
                return Ok(());
            }
            let asked = self.abandon_turns(&[]);
            let mut next = self.state.clone();
            next.follow(Vec::new());
            ruling::withdraw(&mut next);
            self.save(next)?;
            self.give_up_asks(&asked)?;
            return self.release_idle_bots();
        }
        let now = self.ports.clock.now();
        if !self.looks.contributions_due(now) {
            return Ok(());
        }
        let Ok(open) = self.open_listed() else {
            return Ok(());
        };
        let Ok(own) = self.own_logins() else {
            return Ok(());
        };
        self.flights.security_retry();
        // The maintainer's choice to leave a pull request open lasts as long
        // as it is open.
        if self
            .state
            .left_open
            .iter()
            .any(|n| !open.iter().any(|pr| pr.number == *n))
        {
            let mut next = self.state.clone();
            next.left_open
                .retain(|n| open.iter().any(|pr| pr.number == *n));
            self.save(next)?;
        }
        let mut followed: Vec<Contribution> = Vec::new();
        for pr in open.iter().filter(|pr| self.is_contribution(pr, &own)) {
            let known = (self.state.contributions.iter()).find(|c| c.pull_request == pr.number);
            followed.push(match known {
                Some(known) if known.head == pr.commit => known.clone(),
                // A new head is unread, and the last head that passed stays the
                // one it is read since. The push is heard.
                Some(known) => known.pushed(pr.commit.clone(), now),
                None => Contribution::new(pr.number, pr.author.clone(), pr.commit.clone(), now)
                    .with_spent(self.state.spent_by(pr.number)),
            });
        }
        // Oldest first: the ones already followed keep their places
        let place = |c: &Contribution| {
            let held =
                (self.state.contributions.iter()).position(|k| k.pull_request == c.pull_request);
            (held.unwrap_or(usize::MAX), c.pull_request)
        };
        followed.sort_by_key(place);
        if followed != self.state.contributions {
            let asked = self.abandon_turns(&followed);
            let mut next = self.state.clone();
            next.follow(followed);
            ruling::withdraw(&mut next);
            self.save(next)?;
            self.give_up_asks(&asked)?;
        }
        self.follow_ci()?;
        self.follow_bots()?;
        self.follow_posts()?;
        // Before the answers, so a yes or a no to the ask is not answered as
        // a conversation.
        self.follow_release()?;
        self.follow_answers()?;
        self.follow_clock()?;
        self.follow_merge()?;
        self.follow_adoption()
    }
}

#[cfg(test)]
mod adopt_tests;
#[cfg(test)]
mod after_tests;
#[cfg(test)]
mod answer_tests;
#[cfg(test)]
pub(in crate::runner) mod budget_tests;
#[cfg(test)]
mod ci_tests;
#[cfg(test)]
mod clock_tests;
#[cfg(test)]
mod heard_tests;
#[cfg(test)]
mod merge_tests;
#[cfg(test)]
pub(in crate::runner) mod pass_tests;
#[cfg(test)]
mod post_tests;
#[cfg(test)]
mod push_tests;
#[cfg(test)]
mod release_tests;
#[cfg(test)]
mod ruling_tests;
#[cfg(test)]
pub(in crate::runner) mod security_tests;
#[cfg(test)]
pub(in crate::runner) mod tests;
