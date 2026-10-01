//! Agents by name: a harness, the model and effort it runs on, and its limit
//!
//! Kelpie's `[agents]` define each one. A project names one per role in its
//! `[agents]` table, and a local reviewer of kind `session` names one too.
//! A role that names none runs on Claude Code with its `models` entry.
//!
//! An agent's limit is its account's usage windows, read the way its
//! `usage` says, or for a local model a lease held for each whole call.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::local::{ContextSize, EndpointUrl};
use super::reviewers::{LeaseName, lowercase_name};
use super::{Effort, NonBlank, RoleModel, Settings, SettingsError};
use crate::forwarder::Upstream;

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
    /// pi, headless as `pi -p`, on a model an OpenAI-compatible server runs
    Pi,
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
            #[cfg(test)]
            Self::StandIn => "stand-in",
        }
    }

    /// The program that runs it, as an error names it
    pub fn command(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Pi => "pi",
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
            #[cfg(test)]
            Self::StandIn => Harness::StandIn,
        }
    }
}

/// An OpenAI-compatible server a local model runs on
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelServer {
    /// Its base URL, up to and including its `/v1`
    pub url: EndpointUrl,
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
}

impl Limit {
    /// The lease a call holds, for an agent limited by one
    pub fn lease(&self) -> Option<&LeaseName> {
        match self {
            Self::Account(_) => None,
            Self::Lease(lease) => Some(lease),
        }
    }
}

impl Default for Limit {
    fn default() -> Self {
        Self::Account(Account::Claude)
    }
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
    /// How its usage is read, which must be its harness's own reader.
    /// That reader when absent.
    #[serde(default)]
    pub usage: Option<UsageReader>,
    /// The lease it holds for each whole call, with `usage = "none"` only.
    /// This machine's GPU lock, `gpu`, when absent.
    #[serde(default)]
    pub lease: Option<LeaseName>,
    /// The OpenAI-compatible server its model runs on, up to and including
    /// its `/v1`. pi needs one, and Claude Code takes none.
    #[serde(default)]
    pub url: Option<EndpointUrl>,
    /// The context size that server gives the model, in tokens, with `url`
    #[serde(default)]
    pub context: Option<ContextSize>,
}

impl Agent {
    /// Its model and effort, as a call on its harness takes them, or why
    /// its settings cannot reach that model
    fn role_model(&self) -> Result<RoleModel, String> {
        let harness = match (self.harness, &self.url, self.context) {
            (Harness::ClaudeCode, None, None) => AgentHarness::ClaudeCode,
            (Harness::ClaudeCode, ..) => {
                return Err("runs on claude-code, which takes no `url` or `context`".into());
            }
            (Harness::Pi, Some(url), Some(context)) => {
                if let Err(e) = Upstream::new(url) {
                    return Err(format!(
                        "runs on pi, with a `url` kelpie cannot forward to: {e}"
                    ));
                }
                AgentHarness::Pi(ModelServer {
                    url: url.clone(),
                    context,
                })
            }
            (Harness::Pi, ..) => {
                return Err("runs on pi, which needs the model's server as `url` \
                            and its context size as `context`"
                    .into());
            }
            #[cfg(test)]
            (Harness::StandIn, ..) => AgentHarness::StandIn,
        };
        Ok(RoleModel {
            model: self.model.clone(),
            effort: self.effort,
            harness,
        })
    }

    /// What holds its calls back, or why its settings do not say
    ///
    /// The reader must be the harness's own: an agent on Claude Code spends
    /// the Claude account whatever its `usage` says.
    fn limit(&self) -> Result<Limit, String> {
        let own = match self.harness {
            Harness::ClaudeCode => UsageReader::Claude,
            Harness::Pi => UsageReader::None,
            #[cfg(test)]
            Harness::StandIn => self.usage.unwrap_or(UsageReader::Claude),
        };
        let usage = self.usage.unwrap_or(own);
        if usage != own {
            return Err(format!(
                "runs on {}, whose usage is read with `{}`, so it cannot set \
                 `usage = \"{}\"`: leave `usage` out",
                self.harness.as_str(),
                own.as_str(),
                usage.as_str()
            ));
        }
        match (usage, &self.lease) {
            (UsageReader::None, lease) => {
                Ok(Limit::Lease(lease.clone().unwrap_or_else(LeaseName::gpu)))
            }
            (_, Some(_)) => {
                Err("sets `lease`, which only an agent with `usage = \"none\"` takes".into())
            }
            (UsageReader::Claude, None) => Ok(Limit::Account(Account::Claude)),
            (UsageReader::Codex, None) => Ok(Limit::Account(Account::Codex)),
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
    /// What holds each role's calls back
    pub limits: RoleLimits,
}

/// What holds each role's calls back, from the agent it names
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleLimits {
    /// The worker's turns
    pub worker: Limit,
    /// The project's own Claude round
    pub reviewer: Limit,
    /// The judge's one-shots
    pub judge: Limit,
    /// The planning call's one-shots
    pub planner: Limit,
}

impl Settings {
    /// Each role's agent: the one the project names, or `models`' entry on Claude Code
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming a role whose agent `defined` lacks,
    /// or an agent whose limit is unclear.
    pub fn role_agents(
        &self,
        defined: &BTreeMap<AgentName, Agent>,
    ) -> Result<RoleAgents, SettingsError> {
        let pick = |role: &str, named: &Option<AgentName>, model: &RoleModel| match named {
            None => Ok((model.clone(), Limit::default())),
            Some(name) => find(defined, name, &format!("agents.{role}"), "agents"),
        };
        let (worker, worker_limit) = pick("worker", &self.agents.worker, &self.models.worker)?;
        if let (AgentHarness::Pi(_), Some(name)) = (&worker.harness, &self.agents.worker) {
            let what = match (self.preview.enabled, self.worker.guard_hooks.is_empty()) {
                (true, _) => Some("`preview.enabled`, since pi runs no MCP servers"),
                (false, false) => Some("`worker.guard_hooks`, which are Claude Code hooks"),
                (false, true) => None,
            };
            if let Some(what) = what {
                return Err(SettingsError::Invalid {
                    setting: "agents",
                    reason: format!(
                        "`agents.worker` names {name}, on pi, which cannot run {what}: \
                         turn that off or put the worker on Claude Code"
                    ),
                });
            }
        }
        let (reviewer, reviewer_limit) =
            pick("reviewer", &self.agents.reviewer, &self.models.reviewer)?;
        let (judge, judge_limit) = pick("judge", &self.agents.judge, &self.models.judge)?;
        let (planner, planner_limit) = pick("planner", &self.agents.planner, &self.models.planner)?;
        Ok(RoleAgents {
            worker,
            reviewer,
            judge,
            planner,
            limits: RoleLimits {
                worker: worker_limit,
                reviewer: reviewer_limit,
                judge: judge_limit,
                planner: planner_limit,
            },
        })
    }
}

/// The agent `name`, which the setting `at` names, as a call takes it, and
/// what holds its calls back
///
/// # Errors
///
/// [`SettingsError::Invalid`] on `setting` when `defined` lacks it, and on
/// `agents` when its limit is unclear.
pub(super) fn find(
    defined: &BTreeMap<AgentName, Agent>,
    name: &AgentName,
    at: &str,
    setting: &'static str,
) -> Result<(RoleModel, Limit), SettingsError> {
    let Some(agent) = defined.get(name) else {
        return Err(SettingsError::Invalid {
            setting,
            reason: format!(
                "`{at}` names {name}, which is not defined: kelpie's own settings \
                 need an [agents.{name}] table"
            ),
        });
    };
    match agent
        .role_model()
        .and_then(|model| Ok((model, agent.limit()?)))
    {
        Ok(found) => Ok(found),
        Err(reason) => Err(SettingsError::Invalid {
            setting: "agents",
            reason: format!("`agents.{name}` {reason}"),
        }),
    }
}

#[cfg(test)]
mod tests;
