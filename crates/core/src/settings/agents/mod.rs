//! Agents by name: a harness, the model and effort it runs on, and its limit
//!
//! Kelpie's agent files define each one (`crate::agents`). The agent board
//! picks a work item's implementer from the files whose role is `implementer`
//! that `agents.allow` holds. A project lists its reviewers in `agents.reviewers`.
//!
//! An agent's limit is its account's usage windows, read the way its
//! `usage` says, or for a local model a lease held for each whole call.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

mod facts;

pub use facts::{DoctorProbe, HarnessFacts, SessionIds};

use super::gateway::ModelHost;
use super::local::ContextSize;
use super::reviewers::lowercase_name;
use super::{Gateways, HookPoint, RoleModel, Settings, SettingsError};
use crate::agents::{Agent, Agents, FOLDER, Role, Runs};
use crate::forwarder::Upstream;

/// The minutes past the soonest finish an item allows when nothing names a slack
pub const DEFAULT_SLACK_MINUTES: u32 = 60;

/// An agent's name: lowercase letters, digits and `-`
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct AgentName(String);

impl AgentName {
    /// One of kelpie's own agents' names, which are all valid
    pub fn kelpies(name: &'static str) -> Self {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    /// Claude Code, headless as `claude -p`
    ClaudeCode,
    /// pi, headless as `pi -p`, on a model an OpenAI-compatible server runs
    Pi,
    /// Codex, headless as `codex exec`, on the ChatGPT account it is logged in to
    Codex,
    /// goose, headless as `goose run`, on a model an OpenAI-compatible server runs
    Goose,
    /// The test rig's stand-in, which takes any `usage`, so the runner's
    /// pacing can be tested before a second harness exists
    #[cfg(any(test, feature = "test-support"))]
    #[schemars(skip)]
    StandIn,
}

impl Harness {
    /// The tool a reviewer on this harness writes its test with, or `None` when the
    /// fence cannot hold that harness's writes to a folder
    pub fn writing_tool(self) -> Option<&'static str> {
        self.facts().writing_tool
    }

    /// The harness's name, as settings write it
    pub fn as_str(self) -> &'static str {
        self.facts().name
    }

    /// The program that runs it, as an error names it
    pub fn command(self) -> &'static str {
        self.facts().program
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
    /// goose, on the model a server runs
    Goose(ModelServer),
    /// The test rig's stand-in
    #[cfg(any(test, feature = "test-support"))]
    StandIn,
}

impl AgentHarness {
    /// Whether a call on it loads the `.claude` folder of the folder it runs in
    pub fn reads_claude_files(&self) -> bool {
        self.harness().facts().reads_claude_files
    }

    /// Whether a call on it loads the MCP servers kelpie hands it: the repo's, its plugins' and the code graph
    pub fn loads_mcp(&self) -> bool {
        true
    }

    /// Whether a pause ends a turn on it at its next tool result, through
    /// kelpie's hook or guard extension; a call on any other runs to its end
    pub fn ends_at_tool(&self) -> bool {
        self.harness().facts().ends_at_tool
    }

    /// Whether a worker's turn on it can load the browser's MCP bridge
    pub fn drives_a_browser(&self) -> bool {
        self.harness().facts().drives_a_browser
    }

    /// Whether a session keeps the MCP servers it began with, whatever a later call loads
    pub fn keeps_its_servers(&self) -> bool {
        self.harness().facts().keeps_its_servers
    }

    /// The harness, as settings name it
    pub fn harness(&self) -> Harness {
        match self {
            Self::ClaudeCode => Harness::ClaudeCode,
            Self::Pi(_) => Harness::Pi,
            Self::Codex => Harness::Codex,
            Self::Goose(_) => Harness::Goose,
            #[cfg(any(test, feature = "test-support"))]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
    /// A model server reached by `url`, which nothing of kelpie's queues. It
    /// is never paced.
    Unqueued,
    /// A gateway, which queues its calls: each holds a lease the gateway
    /// grants, and none is paced
    Gateway,
}

impl Limit {
    /// Whether it is an account's windows, which the pacer reads: an agent on
    /// any other runs on a local model
    pub fn is_account(&self) -> bool {
        matches!(self, Self::Account(_))
    }
}

impl Default for Limit {
    fn default() -> Self {
        Self::Account(Account::Claude)
    }
}

/// A key kelpie reads no longer, which a settings file may still hold
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gone;

impl<'de> Deserialize<'de> for Gone {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer).map(|_| Self)
    }
}

// Any value, as an older build may have written one.
impl JsonSchema for Gone {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Gone".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "deprecated": true })
    }
}

/// The agents a project may use, or every agent when it lists none
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(from = "Vec<AgentName>")]
pub struct Allow(Vec<AgentName>);

impl Allow {
    /// Whether the project may use `name`
    pub fn holds(&self, name: &AgentName) -> bool {
        self.0.is_empty() || self.0.contains(name)
    }
}

impl From<Vec<AgentName>> for Allow {
    fn from(names: Vec<AgentName>) -> Self {
        Self(names)
    }
}

/// The agents a project names, and the ones it may use
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleAgentNames {
    /// The only agents the project may use, by the names of kelpie's agent
    /// files: any other is refused wherever the project names it, the agent
    /// board never picks it, and no `agent:` label may pin it. Not checked
    /// against the agents folder, since the files are reread each wake.
    /// Every agent with a file when absent or empty.
    #[serde(default)]
    pub allow: Option<Allow>,
    /// Read by nothing: the agent board picks each work item's implementer,
    /// and `shep kelpie upgrade` deletes it
    #[serde(default)]
    #[schemars(extend("deprecated" = true))]
    pub implementers: Option<Gone>,
    /// Read by nothing: the agent board picks each work item's implementer,
    /// and `shep kelpie upgrade` deletes it
    #[serde(default)]
    #[schemars(extend("deprecated" = true))]
    pub implementer_choice: Option<Gone>,
    /// Read by nothing: the agent board's handoff moves a work item, and
    /// `shep kelpie upgrade` deletes it
    #[serde(default)]
    #[schemars(extend("deprecated" = true))]
    pub fallback_after: Option<Gone>,
    /// Minutes past the soonest finish the agent board lets a work item's agent
    /// take: a new item opens on the cheapest agent finishing within it, and a
    /// later turn moves on when its own agent finishes past it. The project
    /// manager's pick may name its own. Whole minutes, `0` allowed. 60 when absent.
    #[serde(default = "default_slack")]
    pub slack: u32,
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
    /// The representative, from kelpie's agent files: the one voice that
    /// words every post to a contributor. A project that picks contributions
    /// needs it, so unlike the project manager it has a default. `representative`
    /// when absent.
    #[serde(default = "default_representative")]
    pub representative: AgentName,
}

impl Default for RoleAgentNames {
    fn default() -> Self {
        Self {
            allow: None,
            implementers: None,
            implementer_choice: None,
            fallback_after: None,
            slack: DEFAULT_SLACK_MINUTES,
            reviewers: None,
            pm: None,
            representative: default_representative(),
        }
    }
}

impl RoleAgentNames {
    /// Each key kelpie reads no longer that the table still holds, by its setting's name
    pub fn gone_keys(&self) -> Vec<&'static str> {
        let held = [
            ("agents.implementers", self.implementers),
            ("agents.implementer_choice", self.implementer_choice),
            ("agents.fallback_after", self.fallback_after),
        ];
        (held.into_iter())
            .filter_map(|(key, gone)| gone.map(|_| key))
            .collect()
    }
}

fn default_slack() -> u32 {
    DEFAULT_SLACK_MINUTES
}

fn default_representative() -> AgentName {
    AgentName::kelpies(crate::agents::REPRESENTATIVE)
}

/// An implementer the agent board may pick for a project
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
    /// Whether it runs on a local model, behind a gateway or not, and never paced
    pub fn is_local(&self) -> bool {
        !self.limit.is_account()
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

/// The agent a project names in `agents.representative` or `triage.agent`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpertAgent {
    /// Its name
    pub name: AgentName,
    /// Its harness, model and effort
    pub model: RoleModel,
    /// What holds its calls back
    pub limit: Limit,
    /// Its file's body, its standing prompt
    pub prompt: Option<String>,
}

/// The project's project manager, from the agent it names
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleAgents {
    /// The project manager, when the project names one
    pub pm: Option<PmAgent>,
}

impl Settings {
    /// The implementers the agent board may pick for this project, by name:
    /// each agent file whose role is `implementer` that `agents.allow` holds
    pub fn allowed_implementers(&self, agents: &Agents) -> Vec<Implementer> {
        let allow = self.agents.allow.as_ref();
        let allowed = (agents.iter()).filter(|(name, agent)| {
            agent.role == Role::Implementer && allow.is_none_or(|a| a.holds(name))
        });
        let implementer = |(name, agent): (&AgentName, &Agent)| {
            let (model, limit) = agent.runs.session()?;
            Some(Implementer {
                name: name.clone(),
                model: model.clone(),
                limit: limit.clone(),
                prompt: agent.prompt.clone(),
            })
        };
        allowed.filter_map(implementer).collect()
    }

    /// The implementers [`Settings::allowed_implementers`] holds that no turn can
    /// run on, by name, each with why: the worker's fence would not hold on it, or
    /// kelpie's `gateways` cannot route to it. The agent board is never asked
    /// about them.
    pub fn refused_implementers(
        &self,
        agents: &Agents,
        gateways: &Gateways,
    ) -> Vec<(AgentName, String)> {
        let why = |i: &Implementer| {
            let fenced = self.under_worker_fence(&i.name, &i.model);
            let routed = || gateways.check_listed(agents, [&i.name]);
            let refused = fenced.and_then(|()| routed()).err()?;
            Some(match refused {
                SettingsError::Invalid { reason, .. } => {
                    let named = format!("{}: ", i.name);
                    reason
                        .strip_prefix(&named)
                        .map_or(reason.clone(), str::to_owned)
                }
                other => other.to_string(),
            })
        };
        let allowed = self.allowed_implementers(agents);
        (allowed.iter())
            .filter_map(|i| Some((i.name.clone(), why(i)?)))
            .collect()
    }

    /// The project manager the project names, from `agents`
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming `agents.pm` when it has no agent
    /// file, its file is another role's, or `agents.allow` leaves it out.
    pub fn role_agents(&self, agents: &Agents) -> Result<RoleAgents, SettingsError> {
        let allow = self.agents.allow.as_ref();
        let pm = match &self.agents.pm {
            Some(name) => {
                let agent = find(agents, name, "agents.pm", "agents", Role::Pm, allow)?;
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
        Ok(RoleAgents { pm })
    }

    /// The representative the project names in `agents.representative`
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming `agents.representative` when it has
    /// no agent file, its file's role is not `representative`, or
    /// `agents.allow` leaves it out.
    pub fn representative(&self, agents: &Agents) -> Result<ExpertAgent, SettingsError> {
        let at = "agents.representative";
        let name = &self.agents.representative;
        let agent = find(
            agents,
            name,
            at,
            at,
            Role::Representative,
            self.agents.allow.as_ref(),
        )?;
        let Runs::Session { model, limit } = &agent.runs else {
            unreachable!("a representative's file only parses to a session")
        };
        Ok(ExpertAgent {
            name: name.clone(),
            model: model.clone(),
            limit: limit.clone(),
            prompt: agent.prompt.clone(),
        })
    }

    /// Refuses a worker on `agent`, named `name`, where the worker's fence
    /// would not hold: only Claude Code runs the project's hooks, and a pi
    /// or goose worker must not be given its model server's host past the forwarder
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming the setting to change, and why.
    pub fn under_worker_fence(
        &self,
        name: &AgentName,
        agent: &RoleModel,
    ) -> Result<(), SettingsError> {
        let facts = agent.harness.harness().facts();
        // parity: harness.guard-hooks
        if !facts.runs_guard_hooks && !self.worker.guard_hooks.is_empty() {
            let what = "`worker.guard_hooks`, which are Claude Code hooks";
            return Err(SettingsError::Invalid {
                setting: "worker.guard_hooks",
                reason: format!(
                    "implementer {name} runs on {}, which cannot run {what}: \
                     turn those off, move the check to a `command` or `write` hook in \
                     `hooks`, which runs on every harness, or build on Claude Code",
                    facts.name
                ),
            });
        }
        // parity: harness.after-command
        if let Some(why) = facts.no_after_command
            && self
                .hooks
                .iter()
                .any(|h| h.point == HookPoint::AfterCommand)
        {
            return Err(SettingsError::Invalid {
                setting: "hooks",
                reason: format!(
                    "implementer {name} runs on {}, which cannot run an `after_command` hook: \
                     {why}. Move the check to a `command` or `write` \
                     hook, or build on another harness",
                    facts.name
                ),
            });
        }
        // parity: harness.local-model
        if let Some(server) = agent.harness.server()
            && let ModelHost::Url(url) = &server.host
            && let Ok(upstream) = Upstream::new(url)
            && let Some(domain) =
                (self.worker.allowed_domains.iter()).find(|d| upstream.is_reached_by(d.as_str()))
        {
            return Err(SettingsError::Invalid {
                setting: "worker.allowed_domains",
                reason: format!(
                    "`{}` would give implementer {name}, on {}, the model server's host, \
                     or every local port, past the forwarder: remove it",
                    domain.as_str(),
                    agent.harness.harness().as_str()
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
/// [`SettingsError::Invalid`] on `setting` when `allow` leaves it out,
/// `agents` lacks it, its file is left out, or its file is another role's.
pub(super) fn find<'a>(
    agents: &'a Agents,
    name: &AgentName,
    at: &str,
    setting: &'static str,
    role: Role,
    allow: Option<&Allow>,
) -> Result<&'a Agent, SettingsError> {
    let invalid = |reason: String| SettingsError::Invalid { setting, reason };
    if let Some(left_out) = left_out(allow, name, at, "name another") {
        return Err(invalid(left_out));
    }
    let Some(agent) = agents.get(name) else {
        if let Some(error) = agents.broken(name) {
            return Err(invalid(format!("`{at}` names {name}, whose {error}")));
        }
        if let Some(command) = agents.unbound_command(name) {
            return Err(invalid(format!(
                "`{at}` names {name}, which is unbound: run `{command}`"
            )));
        }
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

/// The refusal of `name` when `allow` leaves it out, with `instead` as the
/// other fix beside adding it to `agents.allow`
pub(super) fn left_out(
    allow: Option<&Allow>,
    name: &AgentName,
    at: &str,
    instead: &str,
) -> Option<String> {
    let left_out = allow.is_some_and(|allow| !allow.holds(name));
    left_out.then(|| {
        format!(
            "`{at}` names {name}, which `agents.allow` leaves out: add it to `agents.allow`, \
             or {instead}"
        )
    })
}

#[cfg(test)]
mod tests;
