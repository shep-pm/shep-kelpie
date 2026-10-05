//! The review's reviewers, by name
//!
//! A project lists them in `agents.reviewers`, in the order the review runs
//! them, each once, by agent files whose role is `reviewer`. A project that
//! lists none runs `qwen` where the maintainer's qwen-review script is
//! installed, then `defect-hunter`.

use std::path::Path;

use schemars::JsonSchema;
use serde::Deserialize;

use super::agents::find;
use super::{AgentName, NonBlank, Settings, SettingsError};
use crate::agents::{self, Agents, DEFECT_HUNTER, QWEN, QWEN_REVIEW, Role, Runs};

/// Where a project lists its reviewers, as a refusal names it
const SETTING: &str = "agents.reviewers";

/// A lease a local reviewer holds around its round
///
/// `gpu` is this machine's GPU lock, the one the qwen scripts take. Any
/// other name is a lock of its own in the same format, so reviewers on two
/// machines' GPUs never wait on each other.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct LeaseName(String);

impl LeaseName {
    /// This machine's GPU lock
    pub fn gpu() -> Self {
        Self(crate::lease::GPU.to_owned())
    }

    /// Whether it is this machine's GPU lock
    pub fn is_gpu(&self) -> bool {
        self.0 == crate::lease::GPU
    }

    /// The name as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LeaseName {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match lowercase_name(&value) {
            true => Ok(Self(value)),
            false => Err("must be lowercase letters, digits and `-`"),
        }
    }
}

pub(super) fn lowercase_name(value: &str) -> bool {
    let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
    !value.is_empty() && value.chars().all(allowed)
}

/// One reviewer a project lists
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedReviewer {
    /// Its name, as the project lists it
    pub name: AgentName,
    /// What runs its round, a command's path taken from the home folder
    pub runs: Runs,
    /// Its prompt, for a session: its file's body
    pub prompt: Option<String>,
    /// Globs of the files a pull request must change for it to run. Every
    /// pull request when empty.
    pub paths: Vec<NonBlank>,
    /// Whether it reads twice, the second time shown what it found the first
    pub second_look: bool,
}

impl ListedReviewer {
    /// Whether it runs on its own, as a command or an endpoint, not in a session
    pub fn is_local(&self) -> bool {
        self.runs.local().is_some()
    }
}

impl Settings {
    /// The reviewers the project lists, in order, each once
    ///
    /// Agents come from `agents`, and `~/` in a command expands against `home`.
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming a reviewer that has no agent file, or
    /// whose file is an implementer's.
    pub fn lineup(
        &self,
        agents: &Agents,
        home: &Path,
    ) -> Result<Vec<ListedReviewer>, SettingsError> {
        let names = match &self.agents.reviewers {
            Some(names) => names.clone(),
            None => default_reviewers(home),
        };
        let mut lineup: Vec<ListedReviewer> = Vec::new();
        for name in names {
            if lineup.iter().any(|r| r.name == name) {
                continue;
            }
            let agent = find(agents, &name, SETTING, "agents", Role::Reviewer)?;
            let mut runs = agent.runs.clone();
            if let Runs::Command(command) = &mut runs
                && let Ok(rest) = command.command.strip_prefix("~")
            {
                command.command = home.join(rest);
            }
            lineup.push(ListedReviewer {
                name,
                runs,
                prompt: agent.prompt.clone(),
                paths: agent.paths.clone(),
                second_look: agent.second_look,
            });
        }
        Ok(lineup)
    }
}

/// `qwen` where the maintainer's qwen-review script is installed under
/// `home`, then `defect-hunter`
pub fn default_reviewers(home: &Path) -> Vec<AgentName> {
    let qwen = agents::installed(QWEN_REVIEW, home).then_some(QWEN);
    let names = qwen.into_iter().chain([DEFECT_HUNTER]);
    names.map(AgentName::kelpies).collect()
}

#[cfg(test)]
mod tests;
