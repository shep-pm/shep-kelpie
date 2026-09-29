//! Pull request reviewers that are GitHub review bots
//!
//! A pull request reviewer is summoned on the pull request and answers there,
//! within a rate window of its own. The review bots follow one script: a label
//! or a comment summons one, a status or a comment says it heard, a review
//! lands on the head, and each finding is a thread. The runner's round runs
//! that script once for every bot. A [`Profile`] says what differs: its login,
//! its summons, how its answers read, and the window it spends.

use std::fmt;

use crate::lease::LeaseKind;
use crate::ports::{Finding, Timestamp};

/// Everything one review bot has posted on one pull request
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// Its comments on the conversation, oldest first
    pub comments: Vec<Comment>,
    /// Its reviews, oldest first. A clean review may post none.
    pub reviews: Vec<Review>,
    /// The review threads it opened
    pub threads: Vec<Thread>,
    /// The commit statuses it set on the pull request's head, newest first
    pub statuses: Vec<Status>,
}

/// One of a bot's conversation comments, as last edited
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Its text
    pub body: String,
    /// When it was last edited: a bot may edit one comment in place
    pub at: Timestamp,
}

/// One review a bot posted
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// The commit it reviewed
    pub commit: String,
    /// Its text
    pub body: String,
    /// When it was posted
    pub at: Timestamp,
}

/// A review thread a bot opened
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// The forge's id for it, which resolving it takes
    pub id: String,
    /// Whether it is resolved
    pub resolved: bool,
    /// The file it is on
    pub path: String,
    /// The line it is on, if it still maps to one
    pub line: Option<u32>,
    /// Its first comment: the finding
    pub body: String,
}

/// A commit status a bot set
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The commit it is on
    pub commit: String,
    /// What it says, such as "Review completed"
    pub description: String,
    /// When it was set
    pub at: Timestamp,
}

/// What became of a summon, read at one moment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    /// A review covers the head
    Reviewed,
    /// A review is running
    Processing,
    /// The bot refused, quoting when its window opens
    Refused {
        /// When the window opens
        opens: Timestamp,
    },
    /// The bot marked the head done and posted nothing that covers it:
    /// it found nothing new to read
    Completed {
        /// When it marked the head done
        at: Timestamp,
    },
    /// Nothing yet
    Silent,
}

/// A bot's login, which the forge's two APIs spell differently
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Login<'a> {
    /// On the REST API, such as `coderabbitai[bot]`
    pub rest: &'a str,
    /// On the GraphQL API, which drops the `[bot]` suffix
    pub graphql: &'a str,
}

/// What one review bot does its own way
///
/// Parsing lives here, in code, tested against the bot's recorded output.
pub trait Profile: Send + Sync + fmt::Debug {
    /// Its name in what kelpie tells the maintainer and the worker
    fn name(&self) -> &str;

    /// The login its comments, reviews, threads and statuses are posted by
    fn login(&self) -> Login<'_>;

    /// The lease on the rate window each summon spends
    fn lease(&self) -> LeaseKind;

    /// The label whose adding summons it
    fn label(&self) -> &str;

    /// The comment that asks it to read the whole pull request again, where
    /// the label asks only for what is new
    fn full_review(&self) -> Option<&str>;

    /// What became of a summon made at `since`, for commit `head`
    fn read(&self, activity: &Activity, head: &str, since: Timestamp) -> Reading;

    /// Whether it gave any sign of a summon made at `since`, for `head`
    fn heard(&self, activity: &Activity, head: &str, since: Timestamp) -> bool;

    /// Whether a review covers commit `head`
    fn covers(&self, activity: &Activity, head: &str) -> bool;

    /// How many commits other than `head` it reviewed
    fn reviewed_besides(&self, activity: &Activity, head: &str) -> u32;

    /// The quota it last stated, reviews an hour, and when it stated it
    fn quota(&self, activity: &Activity) -> Option<(u32, Timestamp)>;

    /// A thread as a finding the judge can rule on
    fn finding(&self, thread: &Thread) -> Finding;
}

// A bot's answer stamped this long before the summon is still its answer:
// GitHub's clock is not this machine's.
pub(crate) const CLOCK_SLACK: u64 = 60;

impl Activity {
    /// Only what the bot posted or edited from `at` on, threads aside
    pub fn since(&self, at: Timestamp) -> Self {
        let from = at.0.saturating_sub(CLOCK_SLACK);
        Self {
            comments: self
                .comments
                .iter()
                .filter(|c| c.at.0 >= from)
                .cloned()
                .collect(),
            reviews: self
                .reviews
                .iter()
                .filter(|r| r.at.0 >= from)
                .cloned()
                .collect(),
            threads: self.threads.clone(),
            statuses: self
                .statuses
                .iter()
                .filter(|s| s.at.0 >= from)
                .cloned()
                .collect(),
        }
    }

    /// Its threads not yet resolved
    pub fn open_threads(&self) -> impl Iterator<Item = &Thread> {
        self.threads.iter().filter(|t| !t.resolved)
    }
}
