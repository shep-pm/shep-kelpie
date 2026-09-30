//! A review bot round as the state file keeps it: where it stands, and
//! the rounds a work item has had

use serde::{Deserialize, Serialize};

use crate::ports::{Finding, Timestamp, Verdict};
use crate::review_bot::Bot;

/// A work item's pull request reviewer rounds so far, from every bot
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeRabbitTally {
    /// Rounds whose review covered the head
    pub rounds: u32,
    /// Whether the maintainer let the rounds past their cap
    pub cap_cleared: bool,
    /// Whether the reviewers are satisfied with the code as it stands. A
    /// worker's turn changes the code, so it clears this.
    pub satisfied: bool,
}

/// Where one review bot round stands
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CodeRabbitStage {
    /// Waiting for a listed bot's lease, to summon a review of `head`
    Lease {
        /// The head CI passed on
        head: String,
        /// When kelpie marked the draft ready, until the forge reads it so
        #[serde(default, skip_serializing_if = "Option::is_none")]
        readied: Option<Timestamp>,
        /// Whether the summon asks for a full review whatever the bot read
        /// before, because the last one was answered with nothing new
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        full: bool,
    },
    /// The label went on, or the comment asking for a full review was
    /// posted, at `at`. The lease goes back once the bot answers.
    Summoned {
        /// The bot summoned, which holds the round. CodeRabbit when absent.
        #[serde(default, skip_serializing_if = "Bot::is_coderabbit")]
        bot: Bot,
        /// The head the summon is for
        head: String,
        /// When the summon was made
        at: Timestamp,
        /// Whether it asked for a full review
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        full: bool,
        /// Whether the bot gave no sign of it and it went out once more
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        resent: bool,
    },
    /// The open threads of a review of `head`, judged in order
    Judging {
        /// The bot whose review it is. CodeRabbit when absent.
        #[serde(default, skip_serializing_if = "Bot::is_coderabbit")]
        bot: Bot,
        /// The head the review covered
        head: String,
        /// Every thread still open, as a finding
        threads: Vec<OpenThread>,
        /// The judge's verdict on each thread judged so far, same order
        verdicts: Vec<Verdict>,
    },
    /// The findings the judge held were sent to the worker; waiting for its fix
    Fixing {
        /// The head the findings are on, which a fix moves
        head: String,
    },
}

/// A review bot's thread still open, as the judge reads it
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenThread {
    /// The forge's id, which resolving it takes
    pub id: String,
    /// What it says
    pub finding: Finding,
}
