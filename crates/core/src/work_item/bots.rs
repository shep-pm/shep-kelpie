//! What a work item keeps of the review bots its passes summon

use serde::{Deserialize, Serialize};

use crate::ports::Timestamp;
use crate::settings::AgentName;

/// A listed review bot a pass went on without, and why
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BotSkipped {
    /// Its window is closed, so waiting would hold the pass
    Window {
        /// The bot's reviewer, as the project lists it
        reviewer: AgentName,
        /// When the window opens
        opens: Timestamp,
    },
    /// It never reviewed the head in the wait its file allows after its summon
    Silent {
        /// The bot's reviewer, as the project lists it
        reviewer: AgentName,
        /// The head it was summoned for
        head: String,
    },
    /// Its round could not summon it in the wait its file allows, with its
    /// lease never granted or its draft never read ready
    Waited {
        /// The bot's reviewer, as the project lists it
        reviewer: AgentName,
        /// When its round began
        since: Timestamp,
    },
    /// Its review failed when it was summoned, and again when the summon was
    /// sent once more
    Failed {
        /// The bot's reviewer, as the project lists it
        reviewer: AgentName,
        /// The head it was summoned for
        head: String,
    },
    /// The project no longer lists it, so nothing summons it
    Unlisted {
        /// The bot's reviewer, as the round named it
        reviewer: AgentName,
    },
    /// It gave no sign of its summon, and it has never posted a review on
    /// the repo, so it may not be installed there
    NotInstalled {
        /// The bot's reviewer, as the project lists it
        reviewer: AgentName,
    },
    /// The repo is not public, and CodeRabbit's free plan reviews public
    /// repos only
    NotPublic {
        /// The bot's reviewer, as the project lists it
        reviewer: AgentName,
    },
    /// Its settings do not let the summon start a review, and it said so on
    /// the head's commit status
    Declined {
        /// The bot's reviewer, as the project lists it
        reviewer: AgentName,
        /// The status it set, such as `Review skipped: automatic reviews are disabled`
        status: String,
    },
}

impl BotSkipped {
    /// The bot's reviewer, as the project lists it
    pub fn reviewer(&self) -> &AgentName {
        match self {
            Self::Window { reviewer, .. }
            | Self::Silent { reviewer, .. }
            | Self::Waited { reviewer, .. }
            | Self::Failed { reviewer, .. }
            | Self::Unlisted { reviewer }
            | Self::NotInstalled { reviewer }
            | Self::NotPublic { reviewer }
            | Self::Declined { reviewer, .. } => reviewer,
        }
    }

    /// Why, plainly
    pub fn why(&self) -> String {
        match self {
            Self::Window { .. } => "its window is closed".into(),
            Self::Silent { head, .. } => format!(
                "it never reviewed {} in the wait its agent file allows after its summon",
                head.get(..7).unwrap_or(head)
            ),
            Self::Waited { .. } => {
                "it could not be summoned in the wait its agent file allows after its round began"
                    .into()
            }
            Self::Failed { head, .. } => format!(
                "its review of {} failed twice, once when summoned and once when summoned again",
                head.get(..7).unwrap_or(head)
            ),
            Self::Unlisted { .. } => "the project no longer lists it".into(),
            Self::NotInstalled { .. } => {
                "it has never posted a review on this repo, so it may not be installed there".into()
            }
            Self::NotPublic { .. } => {
                "the repo is not public, and CodeRabbit's free plan reviews public repos only"
                    .into()
            }
            Self::Declined { status, .. } => format!("CodeRabbit declined: {status}"),
        }
    }
}
