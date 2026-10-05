//! The review's reviewers, by name
//!
//! Kelpie's `[local_reviewers]` define each one: a local model at an
//! endpoint, a command, a Claude session with its model and effort, or a
//! session on an agent kelpie's `[agents]` define. A project lists them in
//! `review.reviewers`, in the order the review runs them, each once.
//! `claude` is always defined: the project's own Claude round on its
//! reviewer's agent. A project that lists none runs `review.local` then
//! `claude`, or with no `review.local` the maintainer's qwen-review script,
//! when it is there, then `deep`.

use std::fmt;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::agents::{AgentHarness, Limit, find};
use super::{
    AgentName, Effort, Endpoint, LocalCommand, LocalRound, NonBlank, RoleModel, Settings,
    SettingsError,
};
use crate::webhook::KelpieSettings;

/// The name of the project's own Claude round, which kelpie always defines
pub const CLAUDE: &str = "claude";

/// The name of the project's deep round, which kelpie always defines
pub const DEEP: &str = "deep";

/// The name a project's `review.local` runs under
pub const QWEN: &str = "qwen";

/// The setting a refusal names
const SETTING: &str = "review.reviewers";

/// A reviewer's name: lowercase letters, digits and `-`
// wire format: changing this is a breaking change to the state file
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub struct ReviewerName(String);

impl ReviewerName {
    /// The project's own Claude round
    pub fn claude() -> Self {
        Self(CLAUDE.to_owned())
    }

    /// The project's deep round
    pub fn deep() -> Self {
        Self(DEEP.to_owned())
    }

    /// The name as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ReviewerName {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match lowercase_name(&value) {
            true => Ok(Self(value)),
            false => Err("must be lowercase letters, digits and `-`"),
        }
    }
}

impl From<ReviewerName> for String {
    fn from(name: ReviewerName) -> Self {
        name.0
    }
}

impl fmt::Display for ReviewerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

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

/// A reviewer kelpie's own settings define
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Definition {
    /// Kelpie's own reviewer, over an OpenAI-compatible server
    Endpoint(Endpoint),
    /// A command that keeps the README's contract
    Command(LocalCommand),
    /// A fresh Claude session on its own model and effort
    Claude(ClaudeSession),
    /// A fresh session on an agent kelpie's `[agents]` define
    Session(AgentSession),
}

/// A session on a named agent that reviews a round
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentSession {
    /// The agent, from kelpie's `[agents]`
    pub agent: AgentName,
    /// Globs of the files a pull request must change for this reviewer to
    /// run. Every pull request when absent.
    #[serde(default)]
    pub paths: Vec<NonBlank>,
}

/// A Claude session that reviews a round
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaudeSession {
    /// A model id or alias, passed to `claude --model` as written
    pub model: NonBlank,
    /// Passed to `claude --effort`
    pub effort: Effort,
    /// Globs of the files a pull request must change for this reviewer to
    /// run. Every pull request when absent.
    #[serde(default)]
    pub paths: Vec<NonBlank>,
    /// What holds its calls back: the Claude account, or a session
    /// agent's own limit
    #[serde(skip)]
    #[schemars(skip)]
    pub limit: Limit,
    /// The harness it runs on: Claude Code, or a session agent's own
    #[serde(skip)]
    #[schemars(skip)]
    pub harness: AgentHarness,
}

impl ClaudeSession {
    /// Its model and effort, as a call takes them
    pub fn model(&self) -> RoleModel {
        RoleModel {
            model: self.model.clone(),
            effort: self.effort,
            harness: self.harness.clone(),
        }
    }
}

/// One reviewer in a project's loop
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopReviewer {
    /// Its name, as the project lists it
    pub name: ReviewerName,
    /// What runs its round
    pub runs: Runs,
}

/// What runs a reviewer's round
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runs {
    /// A local model or command, never [`LocalRound::Off`]
    Local(LocalRound),
    /// A fresh Claude session
    Claude(ClaudeSession),
    /// The deep round: two readers, the confirmation of each HIGH, one fix
    /// turn and a re-check of it, on the `deep_reviewer` role
    Deep,
}

impl LoopReviewer {
    /// The project's own Claude round on `model`, held back by `limit`
    pub fn claude(model: &RoleModel, limit: &Limit) -> Self {
        Self {
            name: ReviewerName::claude(),
            runs: Runs::Claude(ClaudeSession {
                model: model.model.clone(),
                effort: model.effort,
                paths: Vec::new(),
                limit: limit.clone(),
                harness: model.harness.clone(),
            }),
        }
    }

    /// The project's deep round, which a pull request of any files gets
    pub fn deep() -> Self {
        Self {
            name: ReviewerName::deep(),
            runs: Runs::Deep,
        }
    }

    /// The globs a pull request must change a file under for it to run
    pub fn paths(&self) -> &[NonBlank] {
        match &self.runs {
            Runs::Local(local) => local.paths(),
            Runs::Claude(session) => &session.paths,
            Runs::Deep => &[],
        }
    }

    /// Whether it runs on a local model or command
    pub fn is_local(&self) -> bool {
        matches!(self.runs, Runs::Local(_))
    }
}

impl Settings {
    /// The reviewers the project's loop runs, in order, each once
    ///
    /// Reviewers and agents are `kelpie`'s, and `~/` in a command expands
    /// against `home`.
    ///
    /// # Errors
    ///
    /// [`SettingsError::Invalid`] naming a reviewer or agent that is not
    /// defined, or a reviewer whose definition cannot work.
    pub fn lineup(
        &self,
        kelpie: &KelpieSettings,
        home: &Path,
    ) -> Result<Vec<LoopReviewer>, SettingsError> {
        let defined = &kelpie.local_reviewers;
        let invalid = |reason: String| SettingsError::Invalid {
            setting: SETTING,
            reason,
        };
        if defined.contains_key(&ReviewerName::deep()) {
            return Err(invalid(format!(
                "kelpie's `[local_reviewers.{DEEP}]` is taken: `{DEEP}` is each \
                 project's own deep round on its `deep_reviewer` role, so name yours otherwise"
            )));
        }
        if defined.contains_key(&ReviewerName::claude()) {
            return Err(invalid(format!(
                "kelpie's `[local_reviewers.{CLAUDE}]` is taken: `{CLAUDE}` is each \
                 project's own Claude round on its reviewer's agent, so name yours otherwise"
            )));
        }
        let agents = self.role_agents(&kelpie.agents)?;
        let claude = LoopReviewer::claude(&agents.reviewer, &agents.limits.reviewer);
        if self.review.reviewers.is_empty() {
            // The older form of the local round runs before the project's
            // Claude round, not before the deep round.
            let older = self.review.local.is_some();
            let last = if older { claude } else { LoopReviewer::deep() };
            let local = (self.review.local.clone()).unwrap_or_else(|| LocalRound::default_at(home));
            if !local.is_on() {
                return Ok(vec![last]);
            }
            let name = ReviewerName(QWEN.to_owned());
            let qwen = LoopReviewer {
                name,
                runs: Runs::Local(local),
            };
            return Ok(vec![qwen, last]);
        }
        if self.review.local.is_some() {
            return Err(invalid(
                "`review.local` and `review.reviewers` cannot both be set: \
                 define the local round in kelpie's `[local_reviewers]` and list it"
                    .into(),
            ));
        }
        let mut lineup: Vec<LoopReviewer> = Vec::new();
        for name in &self.review.reviewers {
            if lineup.iter().any(|r| &r.name == name) {
                continue;
            }
            if name.as_str() == CLAUDE {
                lineup.push(claude.clone());
                continue;
            }
            if name.as_str() == DEEP {
                lineup.push(LoopReviewer::deep());
                continue;
            }
            let Some(definition) = defined.get(name) else {
                return Err(invalid(format!(
                    "{name} is not defined: kelpie's own settings need a \
                     [local_reviewers.{name}] table"
                )));
            };
            let at = format!("local_reviewers.{name}");
            let runs = match definition.clone() {
                Definition::Endpoint(endpoint) => Runs::Local(LocalRound::Endpoint(endpoint)),
                Definition::Command(mut command) => {
                    command.command = absolute(&command.command, home).ok_or_else(|| {
                        invalid(format!("`{at}.command` must start with `/` or `~/`"))
                    })?;
                    Runs::Local(LocalRound::Command(command))
                }
                Definition::Claude(session) => Runs::Claude(session),
                Definition::Session(AgentSession { agent, paths }) => {
                    let at = format!("{at}.agent");
                    let (model, limit) = find(&kelpie.agents, &agent, &at, SETTING)?;
                    Runs::Claude(ClaudeSession {
                        model: model.model,
                        effort: model.effort,
                        paths,
                        limit,
                        harness: model.harness,
                    })
                }
            };
            if let Runs::Local(local) = &runs {
                local.check(&at).map_err(invalid)?;
            }
            lineup.push(LoopReviewer {
                name: name.clone(),
                runs,
            });
        }
        Ok(lineup)
    }
}

// Kelpie's own settings have no folder to take a relative path from.
fn absolute(path: &Path, home: &Path) -> Option<std::path::PathBuf> {
    match path.strip_prefix("~") {
        Ok(rest) => Some(home.join(rest)),
        Err(_) => path.is_absolute().then(|| path.to_owned()),
    }
}

#[cfg(test)]
mod tests;
