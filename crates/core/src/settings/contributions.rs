//! Whether and how kelpie picks up other people's pull requests,
//! `[app.dogs.kelpie.contributions]`

use std::num::NonZeroU32;

use schemars::JsonSchema;
use serde::Deserialize;

use super::{AgentName, SettingsError};
use crate::agents::SECURITY_REVIEWER;

/// Which pull requests from other people kelpie picks up
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Pick {
    /// None: kelpie works only its own pull requests
    #[default]
    Off,
    /// Those the maintainer labels for it
    Label,
    /// Every open pull request
    All,
}

/// Whether and how kelpie picks up other people's pull requests
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Contributions {
    /// Which pull requests from other people kelpie picks up. `off` when
    /// absent. Anything but `off` needs the GitHub App to cover the repo,
    /// since the representative would otherwise post as the maintainer.
    #[serde(default)]
    pub pick: Pick,
    /// The reviewers' agent files for a contribution, in the order the
    /// review runs them, each once. Review bots are welcome. Kelpie's default
    /// pass when absent: `iq3-xxs-review` where kelpie holds a `paddock`
    /// gateway, then `defect-hunter-lite`.
    #[serde(default)]
    pub reviewers: Option<Vec<AgentName>>,
    /// The agent file whose role is `reviewer` that reads a contribution
    /// for security. `security-reviewer` when absent.
    #[serde(default = "default_security_reviewer")]
    pub security_reviewer: AgentName,
    /// Whole days without a word from the author before a contribution counts
    /// as quiet, above 0. 7 when absent.
    #[serde(default = "default_quiet_days")]
    pub quiet_days: NonZeroU32,
    /// Whole days without a word from the author before kelpie nudges them,
    /// above 0 and below `quiet_days`. 3 when absent.
    #[serde(default = "default_nudge_days")]
    pub nudge_days: NonZeroU32,
}

impl Default for Contributions {
    fn default() -> Self {
        Self {
            pick: Pick::default(),
            reviewers: None,
            security_reviewer: default_security_reviewer(),
            quiet_days: default_quiet_days(),
            nudge_days: default_nudge_days(),
        }
    }
}

fn default_security_reviewer() -> AgentName {
    AgentName::kelpies(SECURITY_REVIEWER)
}

fn default_quiet_days() -> NonZeroU32 {
    NonZeroU32::new(7).unwrap_or(NonZeroU32::MIN)
}

fn default_nudge_days() -> NonZeroU32 {
    NonZeroU32::new(3).unwrap_or(NonZeroU32::MIN)
}

impl Contributions {
    /// Whether kelpie picks up no one's pull request but its own
    #[inline]
    pub fn is_off(&self) -> bool {
        self.pick == Pick::Off
    }

    /// Checks the keys against each other
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming `contributions.nudge_days` when it is
    /// not below `quiet_days`.
    pub fn check(&self) -> Result<(), SettingsError> {
        match self.nudge_days < self.quiet_days {
            true => Ok(()),
            false => Err(SettingsError::Invalid {
                setting: "contributions.nudge_days",
                reason: format!(
                    "it is {} and `quiet_days` is {}: the nudge comes before a contribution \
                     counts as quiet, so set `nudge_days` below `quiet_days`",
                    self.nudge_days, self.quiet_days
                ),
            }),
        }
    }
}
