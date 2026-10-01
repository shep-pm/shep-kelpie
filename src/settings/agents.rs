//! Agents by name: a harness, and the model and effort it runs on
//!
//! Kelpie's `[agents]` define each one. A project names one per role in its
//! `[agents]` table, and a local reviewer of kind `session` names one too.
//! A role that names none runs on Claude Code with its `models` entry.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::reviewers::lowercase_name;
use super::{Effort, NonBlank, RoleModel, Settings, SettingsError};

/// An agent's name: lowercase letters, digits and `-`
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct AgentName(String);

impl AgentName {
    /// The name as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AgentName {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match lowercase_name(&value) {
            true => Ok(Self(value)),
            false => Err("must be lowercase letters, digits and `-`"),
        }
    }
}

impl From<AgentName> for String {
    fn from(name: AgentName) -> Self {
        name.0
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The program that runs an agent's sessions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    /// Claude Code, headless as `claude -p`
    ClaudeCode,
}

/// One agent kelpie's own settings define
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    /// What runs its sessions
    pub harness: Harness,
    /// A model id or alias, as the harness takes it
    pub model: NonBlank,
    /// How hard the model thinks
    pub effort: Effort,
}

impl Agent {
    /// Its model and effort, as a call on its harness takes them
    pub fn model(&self) -> RoleModel {
        match self.harness {
            Harness::ClaudeCode => RoleModel {
                model: self.model.clone(),
                effort: self.effort,
            },
        }
    }
}

/// The agent a project names for each role, over its `models` entry
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleAgentNames {
    /// The worker's sessions
    #[serde(default)]
    pub worker: Option<AgentName>,
    /// The project's own Claude round, `claude` in `review.reviewers`
    #[serde(default)]
    pub reviewer: Option<AgentName>,
    /// The one-shot that judges every finding
    #[serde(default)]
    pub judge: Option<AgentName>,
    /// The one-shot that plans a ready issue before it opens a work item
    #[serde(default)]
    pub planner: Option<AgentName>,
}

/// The model and effort each role runs on, from the agent it names
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleAgents {
    /// The worker's sessions
    pub worker: RoleModel,
    /// The project's own Claude round
    pub reviewer: RoleModel,
    /// The judge's one-shots
    pub judge: RoleModel,
    /// The planning call's one-shots
    pub planner: RoleModel,
}

impl Settings {
    /// Each role's agent: the one the project names, or `models`' entry on Claude Code
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming a role whose agent `defined` lacks.
    pub fn role_agents(
        &self,
        defined: &BTreeMap<AgentName, Agent>,
    ) -> Result<RoleAgents, SettingsError> {
        let pick = |role: &str, named: &Option<AgentName>, model: &RoleModel| match named {
            None => Ok(model.clone()),
            Some(name) => find(defined, name, &format!("agents.{role}"), "agents"),
        };
        Ok(RoleAgents {
            worker: pick("worker", &self.agents.worker, &self.models.worker)?,
            reviewer: pick("reviewer", &self.agents.reviewer, &self.models.reviewer)?,
            judge: pick("judge", &self.agents.judge, &self.models.judge)?,
            planner: pick("planner", &self.agents.planner, &self.models.planner)?,
        })
    }
}

/// The agent `name`, which the setting `at` names, as a call takes it
///
/// # Errors
///
/// [`SettingsError::Invalid`] on `setting` when `defined` lacks it.
pub(super) fn find(
    defined: &BTreeMap<AgentName, Agent>,
    name: &AgentName,
    at: &str,
    setting: &'static str,
) -> Result<RoleModel, SettingsError> {
    match defined.get(name) {
        Some(agent) => Ok(agent.model()),
        None => Err(SettingsError::Invalid {
            setting,
            reason: format!(
                "`{at}` names {name}, which is not defined: kelpie's own settings \
                 need an [agents.{name}] table"
            ),
        }),
    }
}

#[cfg(test)]
mod tests;
