//! A review bot the project's picks leave out, and when it comes back
//!
//! A bot a pass passed over as not installed on the repo, or shut out by its
//! visibility, is recorded here with when and why, so each pick leaves it out
//! for [`BOT_LEFT_OUT`]. A bot seen reviewing is taken back off the list, and
//! one passed over for its window, its silence or its settings is never
//! recorded.

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

/// A review bot a project's passes left out, with when and why it was last
/// left out
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotLeftOut {
    /// The bot's reviewer, as the project lists it
    pub reviewer: AgentName,
    /// When a pass last passed it over as not installed or not public
    pub at: Timestamp,
    /// Why, as that pass put it, for the ruling a pass with nobody else to
    /// read names it
    pub why: String,
}

impl BotLeftOut {
    /// Whether `now` is still within [`BOT_LEFT_OUT`] of when the bot was left out
    pub fn holds(&self, now: Timestamp) -> bool {
        now.0.saturating_sub(self.at.0) < BOT_LEFT_OUT
    }

    /// When the bot comes back into the project's picks
    pub fn back_in(&self) -> Timestamp {
        Timestamp(self.at.0.saturating_add(BOT_LEFT_OUT))
    }
}

impl ProjectState {
    /// Records that `reviewer` was passed over as a bot its repo cannot run,
    /// at `at` and for `why`, so each pick leaves it out until [`BOT_LEFT_OUT`]
    /// after
    pub fn leave_bot_out(&mut self, reviewer: AgentName, at: Timestamp, why: String) {
        self.bots_left_out.retain(|left| left.reviewer != reviewer);
        self.bots_left_out.push(BotLeftOut { reviewer, at, why });
    }

    /// The record of `reviewer` being left out, while `now` is no more than
    /// [`BOT_LEFT_OUT`] after it was last left out
    pub fn bot_left_out(&self, reviewer: &AgentName, now: Timestamp) -> Option<&BotLeftOut> {
        self.bots_left_out
            .iter()
            .find(|left| &left.reviewer == reviewer && left.holds(now))
    }

    /// Takes every left-out bot `named` names off the list, so the next pick
    /// asks it again
    pub fn bots_back_in(&mut self, named: impl Fn(&AgentName) -> bool) {
        self.bots_left_out.retain(|left| !named(&left.reviewer));
    }
}
