//! A review bot the project's picks leave out, and when it comes back
//!
//! A bot a pass passed over as not installed on the repo, or shut out by its
//! visibility, is recorded here with when, so each pick leaves it out for
//! [`BOT_LEFT_OUT`]. A bot seen reviewing is taken back off the list, and one
//! passed over for its window, its silence or its settings is never recorded.

use serde::{Deserialize, Serialize};

use super::ProjectState;
use crate::ports::Timestamp;
use crate::settings::AgentName;

/// How long a review bot a pass left out stays out of the project's picks
///
/// Seven days: a bot installed on the repo some days later is asked again
/// within the week, and one that is not costs the project no more than one
/// round of waiting a week.
pub const BOT_LEFT_OUT: u64 = 7 * 24 * 3600;

/// A review bot a project's passes left out, and when it was last left out
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotLeftOut {
    /// The bot's reviewer, as the project lists it
    pub reviewer: AgentName,
    /// When a pass last passed it over as not installed or not public
    pub at: Timestamp,
}

impl ProjectState {
    /// Records that `reviewer` was passed over as a bot its repo cannot run,
    /// at `at`, so each pick leaves it out until [`BOT_LEFT_OUT`] after
    pub fn leave_bot_out(&mut self, reviewer: AgentName, at: Timestamp) {
        self.bots_left_out.retain(|left| left.reviewer != reviewer);
        self.bots_left_out.push(BotLeftOut { reviewer, at });
    }

    /// Whether `reviewer` was left out no more than [`BOT_LEFT_OUT`] before `now`
    pub fn bot_left_out(&self, reviewer: &AgentName, now: Timestamp) -> bool {
        self.bots_left_out.iter().any(|left| {
            &left.reviewer == reviewer && now.0.saturating_sub(left.at.0) < BOT_LEFT_OUT
        })
    }

    /// Takes `reviewer` off the bots left out, so the next pick asks it again
    pub fn bot_back_in(&mut self, reviewer: &AgentName) {
        self.bots_left_out.retain(|left| &left.reviewer != reviewer);
    }
}
