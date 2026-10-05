//! What a work item keeps of the review bots its passes summon

use serde::{Deserialize, Serialize};

use crate::ports::Timestamp;
use crate::settings::AgentName;

/// A listed review bot a pass went on without, and why
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BotSkipped {
    /// Its window opens more than an hour on, so waiting would hold the pass
    Window {
        /// The bot's reviewer, as the project lists it
        reviewer: AgentName,
        /// When the window opens
        opens: Timestamp,
    },
    /// It never reviewed the head in the two hours after its summon
    Silent {
        /// The bot's reviewer, as the project lists it
        reviewer: AgentName,
        /// The head it was summoned for
        head: String,
    },
    /// Its round could not summon it in two hours, with its lease never
    /// granted or its draft never read ready
    Waited {
        /// The bot's reviewer, as the project lists it
        reviewer: AgentName,
        /// When its round began
        since: Timestamp,
    },
    /// The project no longer lists it, so nothing summons it
    Unlisted {
        /// The bot's reviewer, as the round named it
        reviewer: AgentName,
    },
    /// The repo is not public, and CodeRabbit's free plan reviews public
    /// repos only
    NotPublic {
        /// The bot's reviewer, as the project lists it
        reviewer: AgentName,
    },
}

impl BotSkipped {
    /// The bot's reviewer, as the project lists it
    pub fn reviewer(&self) -> &AgentName {
        match self {
            Self::Window { reviewer, .. }
            | Self::Silent { reviewer, .. }
            | Self::Waited { reviewer, .. }
            | Self::Unlisted { reviewer }
            | Self::NotPublic { reviewer } => reviewer,
        }
    }

    /// Why, plainly
    pub fn why(&self) -> String {
        match self {
            Self::Window { .. } => "its window opens more than an hour on".into(),
            Self::Silent { head, .. } => format!(
                "it never reviewed {} in the two hours after its summon",
                head.get(..7).unwrap_or(head)
            ),
            Self::Waited { .. } => {
                "it could not be summoned in the two hours after its round began".into()
            }
            Self::Unlisted { .. } => "the project no longer lists it".into(),
            Self::NotPublic { .. } => {
                "the repo is not public, and CodeRabbit's free plan reviews public repos only"
                    .into()
            }
        }
    }
}
