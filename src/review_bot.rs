//! Pull request reviewers that are GitHub review bots
//!
//! A pull request reviewer is summoned on the pull request and answers there,
//! within a rate window of its own. The review bots follow one script: a label
//! or a comment summons one, a status or a comment says it heard, a review
//! lands on the head, and each finding is a thread. The runner's round runs
//! that script once for every bot. A [`Profile`] says what differs: its login,
//! its summons, how its answers read, and the window it spends.

use std::fmt;
use std::num::NonZeroU32;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::lease::LeaseKind;
use crate::ports::{Finding, Timestamp};
use crate::state::Resource;

/// A review bot kelpie has a profile for, as settings and the state file name it
// wire format: changing this is a breaking change to settings and the state file
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Bot {
    /// CodeRabbit
    #[default]
    Coderabbit,
    /// cubic
    Cubic,
    /// Codex, the ChatGPT Codex app
    Codex,
}

impl Bot {
    /// Every bot kelpie has a profile for
    pub const ALL: [Self; 3] = [Self::Coderabbit, Self::Cubic, Self::Codex];

    /// Its name as settings write it, which also names its lease
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coderabbit => "coderabbit",
            Self::Cubic => "cubic",
            Self::Codex => "codex",
        }
    }

    /// Its name in what kelpie tells the maintainer and the worker
    pub fn name(self) -> &'static str {
        match self {
            Self::Coderabbit => "CodeRabbit",
            Self::Cubic => "cubic",
            Self::Codex => "Codex",
        }
    }

    /// Whether it is CodeRabbit, which the state file names by leaving it out
    pub fn is_coderabbit(&self) -> bool {
        *self == Self::Coderabbit
    }

    /// The lease on its window
    pub fn lease(self) -> LeaseKind {
        LeaseKind::try_from(self.as_str()).expect("a bot's name is a lease kind")
    }

    /// The row the state file keeps for its lease
    pub fn resource(self) -> Resource {
        match self {
            Self::Coderabbit => Resource::Coderabbit,
            Self::Cubic => Resource::Cubic,
            Self::Codex => Resource::Codex,
        }
    }

    /// The bot whose lease a state file row keeps, if it is a bot's
    pub fn of(resource: Resource) -> Option<Self> {
        Self::ALL.into_iter().find(|bot| bot.resource() == resource)
    }
}

impl fmt::Display for Bot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The pull request reviewers kelpie's own settings define, each by its window
///
/// A project lists only reviewers defined here. CodeRabbit is defined with
/// one review an hour when absent, which its review footers raise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reviewers {
    /// CodeRabbit's window
    #[serde(default)]
    pub coderabbit: Option<ReviewWindow>,
    /// cubic's window. Its free plan gives a private repo 20 reviews a month.
    #[serde(default)]
    pub cubic: Option<ReviewWindow>,
    /// Codex's window: the weekly allowance its plan gives code reviews
    #[serde(default)]
    pub codex: Option<ReviewWindow>,
}

impl Reviewers {
    /// The window of `bot`, or `None` when it is not defined
    pub fn window(&self, bot: Bot) -> Option<ReviewWindow> {
        match bot {
            Bot::Coderabbit => Some(self.coderabbit.unwrap_or(ReviewWindow::HOURLY)),
            Bot::Cubic => self.cubic,
            Bot::Codex => self.codex,
        }
    }

    /// Every defined bot and its window
    pub fn defined(&self) -> impl Iterator<Item = (Bot, ReviewWindow)> + '_ {
        Bot::ALL
            .into_iter()
            .filter_map(|bot| Some((bot, self.window(bot)?)))
    }
}

/// A review window: so many reviews in so many hours
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewWindow {
    /// Reviews it allows at once. A quota the bot states overrides it.
    pub reviews: NonZeroU32,
    /// Hours each accepted summon holds its place
    pub hours: NonZeroU32,
}

impl ReviewWindow {
    /// One review an hour, CodeRabbit's until a footer says more
    pub const HOURLY: Self = Self {
        reviews: NonZeroU32::MIN,
        hours: NonZeroU32::MIN,
    };

    /// How long each accepted summon holds its place, in seconds
    pub fn seconds(self) -> u64 {
        u64::from(self.hours.get()) * crate::lease::window::HOUR
    }
}

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
    /// The bot refused
    Refused {
        /// When its window opens, if it said. The window's definition
        /// decides when it did not.
        opens: Option<Timestamp>,
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
    /// Which bot it is
    fn bot(&self) -> Bot;

    /// Its name in what kelpie tells the maintainer and the worker
    fn name(&self) -> &str {
        self.bot().name()
    }

    /// The login its comments, reviews, threads and statuses are posted by
    fn login(&self) -> Login<'_>;

    /// The lease on the rate window each summon spends
    fn lease(&self) -> LeaseKind {
        self.bot().lease()
    }

    /// The label whose adding summons it, if one does
    fn label(&self) -> Option<&str>;

    /// The comment that asks it to read the whole pull request again, where
    /// the label asks only for what is new. A bot with no label is summoned
    /// by this comment alone.
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

// A findings file holds one finding a line, fields split by `|`.
pub(crate) fn one_line(text: &str) -> String {
    const MOST: usize = 600;
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat.replace('|', "/");
    match flat.char_indices().nth(MOST) {
        Some((cut, _)) => format!("{}...", &flat[..cut]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_finding_is_cut_to_one_line() {
        let long = "word ".repeat(200);
        let cut = one_line(&long);
        assert_eq!(cut.chars().count(), 603);
        assert!(cut.ends_with("..."));
        assert_eq!(one_line("a |b\n c"), "a /b c");
    }

    #[test]
    fn a_bots_name_is_its_lease() {
        assert_eq!(Bot::Coderabbit.lease(), LeaseKind::coderabbit());
        assert_eq!(Bot::Cubic.lease().as_str(), "cubic");
        assert_eq!(Bot::Codex.lease().as_str(), "codex");
    }
}
