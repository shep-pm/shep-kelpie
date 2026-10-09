//! Agents by name: a harness, the model and effort it runs on, and its limit
//!
//! Kelpie's agent files define each one (`crate::agents`). A project lists
//! its implementers in `agents.implementers` and its reviewers in
//! `agents.reviewers`, each by an agent file whose role is that one.
//!
//! An agent's limit is its account's usage windows, read the way its
//! `usage` says, or for a local model a lease held for each whole call.

use std::fmt;
use std::num::NonZeroU32;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::gateway::ModelHost;
use super::local::ContextSize;
use super::reviewers::{LeaseName, lowercase_name};
use super::{RoleModel, Settings, SettingsError};
use crate::agents::{Agent, Agents, FOLDER, Role, Runs};
use crate::forwarder::Upstream;

/// An agent's name: lowercase letters, digits and `-`
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct AgentName(String);

impl AgentName {
    /// One of kelpie's own agents' names, which are all valid
    pub(crate) fn kelpies(name: &'static str) -> Self {
        Self(name.to_owned())
    }

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
    /// pi, headless as `pi -p`, on a model an OpenAI-compatible server runs
    Pi,
    /// Codex, headless as `codex exec`, on the ChatGPT account it is logged in to
    Codex,
    /// The test rig's stand-in, which takes any `usage`, so the runner's
    /// pacing can be tested before a second harness exists
    #[cfg(test)]
    #[schemars(skip)]
    StandIn,
}

impl Harness {
    /// The harness's name, as settings write it
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Pi => "pi",
            Self::Codex => "codex",
            #[cfg(test)]
            Self::StandIn => "stand-in",
        }
    }

    /// The program that runs it, as an error names it
    pub fn command(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Pi => "pi",
            Self::Codex => "codex",
            #[cfg(test)]
            Self::StandIn => "stand-in",
        }
    }
}

/// The harness a call runs on, with the server its model is on where it needs one
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AgentHarness {
    /// Claude Code, on Anthropic's API
    #[default]
    ClaudeCode,
    /// pi, on the model a server runs
    Pi(ModelServer),
    /// Codex, on the ChatGPT account
    Codex,
    /// The test rig's stand-in
    #[cfg(test)]
    StandIn,
}

impl AgentHarness {
    /// The harness, as settings name it
    pub fn harness(&self) -> Harness {
        match self {
            Self::ClaudeCode => Harness::ClaudeCode,
            Self::Pi(_) => Harness::Pi,
            Self::Codex => Harness::Codex,
            #[cfg(test)]
            Self::StandIn => Harness::StandIn,
        }
    }
}

/// An OpenAI-compatible server a local model runs on
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelServer {
    /// Its base URL, or the gateway in front of it
    pub host: ModelHost,
    /// The context size it gives the model, in tokens
    pub context: ContextSize,
}

/// How an agent's usage is read
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum UsageReader {
    /// Claude's `/usage`: the Claude account's 5-hour and weekly windows
    Claude,
    /// Codex's own 5-hour and weekly windows
    Codex,
    /// None: a local model, whose limit is its lease
    None,
}

impl UsageReader {
    /// The reader's name, as settings write it
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::None => "none",
        }
    }
}

/// The account whose usage windows an agent spends
// wire format: changing this is a breaking change to the pacer's status
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Account {
    /// The account `claude` is logged in to
    Claude,
    /// The account `codex` is logged in to
    Codex,
}

impl Account {
    /// The account's name, as `status` shows it
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// What holds an agent's calls back
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Limit {
    /// Its account's windows, which the pacer reads
    Account(Account),
    /// A lease it holds for the whole of each call. It is never paced.
    Lease(LeaseName),
    /// A gateway, which queues its calls itself: no lease and no pacing
    Gateway,
}

impl Limit {
    /// The lease a call holds, for an agent limited by one
    pub fn lease(&self) -> Option<&LeaseName> {
        match self {
            Self::Account(_) | Self::Gateway => None,
            Self::Lease(lease) => Some(lease),
        }
    }
}

impl Default for Limit {
    fn default() -> Self {
        Self::Account(Account::Claude)
    }
}

/// The agents a project names: its implementers and its reviewers
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleAgentNames {
    /// The agents that build its work items, from kelpie's agent files. An
    /// issue labelled `agent:<name>` runs on that one. With several listed,
    /// the issue writer labels any other first; with one, it runs on that.
    /// `["sonnet-high"]` when absent.
    #[serde(default = "default_implementers")]
    pub implementers: Vec<AgentName>,
    /// The issue writer's agent file, which `shep kelpie issue` runs and
    /// which labels an unlabelled issue. `issue-writer` when absent.
    #[serde(default = "default_issue_writer")]
    pub issue_writer: AgentName,
    /// Minutes a work item's first turn may wait for its model, with no
    /// output yet, before it moves to the next implementer listed. Off when
    /// absent.
    #[serde(default)]
    pub fallback_after: Option<NonZeroU32>,
    /// The agents that review each pull request, from kelpie's agent files,
    /// each once a pass, in this order. When absent, `qwen` where the
    /// maintainer's qwen-review script is installed, then `defect-hunter`.
    #[serde(default)]
    pub reviewers: Option<Vec<AgentName>>,
    /// The project manager, from kelpie's agent files, such as `pm`: woken
    /// on the board's events to pick work, unstick items and answer the
    /// maintainer. When absent, the board's rule picks and stuck items wait
    /// on rulings alone.
    #[serde(default)]
    pub pm: Option<AgentName>,
}

impl Default for RoleAgentNames {
    fn default() -> Self {
        Self {
            implementers: default_implementers(),
            issue_writer: default_issue_writer(),
            fallback_after: None,
            reviewers: None,
            pm: None,
        }
    }
}

fn default_implementers() -> Vec<AgentName> {
    vec![AgentName::kelpies(crate::agents::DEFAULT_IMPLEMENTER)]
}

fn default_issue_writer() -> AgentName {
    AgentName::kelpies(crate::agents::ISSUE_WRITER)
}

/// One agent a project lists in `agents.implementers`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Implementer {
    /// Its name, which an `agent:<name>` label gives
    pub name: AgentName,
    /// Its harness, model and effort
    pub model: RoleModel,
    /// What holds its turns back
    pub limit: Limit,
    /// Its file's body, added to kelpie's own instructions
    pub prompt: Option<String>,
}

impl Implementer {
    /// Whether it runs on a local model, held by a lease or a gateway and never paced
    pub fn is_local(&self) -> bool {
        !matches!(self.limit, Limit::Account(_))
    }
}

/// The agent a project names in `agents.pm`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmAgent {
    /// Its name
    pub name: AgentName,
    /// Its harness, model and effort
    pub model: RoleModel,
    /// What holds its calls back
    pub limit: Limit,
    /// Its file's body, its standing prompt
    pub prompt: Option<String>,
}

/// The project's implementers and project manager, from the agents it names
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleAgents {
    /// The project's implementers, in its order, each once
    pub implementers: Vec<Implementer>,
    /// The implementer listed first, which the issue writer is told is the
    /// default and an unlabelled issue runs on when only one is listed
    pub default_implementer: Implementer,
    /// The project manager, when the project names one
    pub pm: Option<PmAgent>,
}

impl RoleAgents {
    /// The listed implementer `name`, if the project lists it
    pub fn implementer(&self, name: &AgentName) -> Option<&Implementer> {
        self.implementers.iter().find(|i| &i.name == name)
    }

    /// The listed implementers' names, in the project's order
    pub fn implementer_names(&self) -> Vec<AgentName> {
        self.implementers.iter().map(|i| i.name.clone()).collect()
    }
}

/// The setting a refusal about the implementers names
const IMPLEMENTERS: &str = "agents.implementers";

impl Settings {
    /// The implementers and project manager the project names, from `agents`
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming an agent `agents` lacks or whose
    /// file is another role's, an implementer the worker's fence cannot hold,
    /// or an empty list.
    pub fn role_agents(&self, agents: &Agents) -> Result<RoleAgents, SettingsError> {
        let mut implementers: Vec<Implementer> = Vec::new();
        for name in &self.agents.implementers {
            if implementers.iter().any(|i| &i.name == name) {
                continue;
            }
            let agent = find(agents, name, IMPLEMENTERS, "agents", Role::Implementer)?;
            let Runs::Session { model, limit } = &agent.runs else {
                unreachable!("an implementer's file only parses to a session")
            };
            self.under_worker_fence(name, model)?;
            implementers.push(Implementer {
                name: name.clone(),
                model: model.clone(),
                limit: limit.clone(),
                prompt: agent.prompt.clone(),
            });
        }
        let Some(default_implementer) = implementers.first().cloned() else {
            return Err(SettingsError::Invalid {
                setting: IMPLEMENTERS,
                reason: "lists no implementer: list one, such as `sonnet-high`".into(),
            });
        };
        let writer = &self.agents.issue_writer;
        find(
            agents,
            writer,
            "agents.issue_writer",
            "agents",
            Role::IssueWriter,
        )?;
        let pm = match &self.agents.pm {
            Some(name) => {
                let agent = find(agents, name, "agents.pm", "agents", Role::Pm)?;
                let Runs::Session { model, limit } = &agent.runs else {
                    unreachable!("a project manager's file only parses to a session")
                };
                Some(PmAgent {
                    name: name.clone(),
                    model: model.clone(),
                    limit: limit.clone(),
                    prompt: agent.prompt.clone(),
                })
            }
            None => None,
        };
        Ok(RoleAgents {
            implementers,
            default_implementer,
            pm,
        })
    }

    /// Refuses a worker on `agent`, named `name`, where the worker's fence
    /// would not hold: only Claude Code runs the project's hooks, and a pi
    /// worker must not be given its model server's host past the forwarder
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming the setting to change, and why.
    pub fn under_worker_fence(
        &self,
        name: &AgentName,
        agent: &RoleModel,
    ) -> Result<(), SettingsError> {
        let other = match &agent.harness {
            AgentHarness::Pi(_) => Some("pi"),
            AgentHarness::Codex => Some("codex"),
            _ => None,
        };
        if let Some(harness) = other
            && !self.worker.guard_hooks.is_empty()
        {
            let what = "`worker.guard_hooks`, which are Claude Code hooks";
            return Err(SettingsError::Invalid {
                setting: IMPLEMENTERS,
                reason: format!(
                    "implementer {name} runs on {harness}, which cannot run {what}: \
                     turn those off or build on Claude Code"
                ),
            });
        }
        if let AgentHarness::Pi(server) = &agent.harness
            && let ModelHost::Url(url) = &server.host
            && let Ok(upstream) = Upstream::new(url)
            && let Some(domain) =
                (self.worker.allowed_domains.iter()).find(|d| upstream.is_reached_by(d.as_str()))
        {
            return Err(SettingsError::Invalid {
                setting: "worker.allowed_domains",
                reason: format!(
                    "`{}` would give implementer {name}, on pi, the model server's host, \
                     or every local port, past the forwarder: remove it",
                    domain.as_str()
                ),
            });
        }
        Ok(())
    }
}

/// The agent `name` from `agents`, which the setting `at` names for `role`
///
/// # Errors
///
/// [`SettingsError::Invalid`] on `setting` when `agents` lacks it, or its
/// file is another role's.
pub(super) fn find<'a>(
    agents: &'a Agents,
    name: &AgentName,
    at: &str,
    setting: &'static str,
    role: Role,
) -> Result<&'a Agent, SettingsError> {
    let invalid = |reason: String| SettingsError::Invalid { setting, reason };
    let Some(agent) = agents.get(name) else {
        return Err(invalid(format!(
            "`{at}` names {name}, which has no agent file: write `{FOLDER}/{name}.md` \
             in kelpie's home"
        )));
    };
    if agent.role != role {
        return Err(invalid(format!(
            "`{at}` names {name}, whose agent file's role is `{}`: list it where that \
             role goes, or name an agent whose role is `{}`",
            agent.role.as_str(),
            role.as_str()
        )));
    }
    Ok(agent)
}

#[cfg(test)]
mod tests;
