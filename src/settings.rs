//! A project's settings
//!
//! Its runner sheep's `[app.dogs.kelpie]` table, or the settings file a
//! project had before one. Unknown keys are refused, so a misspelt or
//! malformed setting stops the runner with a message naming it. Every
//! setting is required except the ones added after the first build
//! (`max_items`, `pacing.enabled`, `worker.allowed_domains`,
//! `worker.build_env`, `worker.instructions_file`, `worker.turn_timeout`,
//! `worker.guard_hooks`, `[skills]` and `[agents]`).
//! `settings.example.toml` beside this crate holds the defaults.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use shep_client::dogs::dog_config;

pub mod moving;
pub mod source;
mod table;

pub use table::table_of;
mod agents;
mod local;
mod removed;
mod reviewers;
mod skills;

pub use agents::{
    Account, AgentHarness, AgentName, Harness, Implementer, Limit, ModelServer, RoleAgentNames,
    RoleAgents, UsageReader,
};
pub use local::{ContextSize, Endpoint, EndpointUrl, LocalCommand, LocalRound};
pub use reviewers::{LeaseName, ListedReviewer, default_reviewers};
pub use skills::{SkillChoice, SkillName, StepSkills};

/// Everything kelpie reads about one project
#[dog_config]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// The project's own checkout. A leading `~/` is the home folder.
    pub repo: PathBuf,
    /// The project's repo on the forge
    pub forge: ForgeSlug,
    /// Who decides a merge
    pub merge_authority: MergeAuthority,
    /// Whether the repo runs CI on pull requests
    ///
    /// With it on, a pull request with no checks yet waits for them. With it
    /// off, kelpie reads no checks and asks for the merge once the branch
    /// has the latest `main`.
    pub ci: bool,
    /// How many work items may be open at once, each with its own branch,
    /// worktree and gates. The worker's turns still run one at a time. 1
    /// when absent.
    #[serde(default = "default_max_items")]
    pub max_items: NonZeroU32,
    /// Words that stay off the forge: kelpie refuses a post naming one, and
    /// a worker's guard refuses a commit or `gh` text that does. Whole
    /// words, whatever their case. None when absent.
    #[serde(default)]
    pub private_names: Vec<NonBlank>,
    /// The agents that build the project's work items and review its pull
    /// requests, from kelpie's agent files
    #[serde(default)]
    pub agents: RoleAgentNames,
    /// Usage pacing
    pub pacing: Pacing,
    /// What every worker is started with
    pub worker: Worker,
    /// The skill each step runs, over kelpie's vendored defaults
    #[serde(default)]
    pub skills: StepSkills,
}

pub(crate) use removed::{DELETE, Removed, refuse as refuse_removed};

const RELAY: &str = "the relay is gone";
const PLANNING: &str = "the planning call is gone";
const AUDIT: &str = "the whole-issue check is gone";
const LOOP: &str = "the review loop and its judge are gone";
const SHOTS: &str = "shots and the preview are parked";
const AGENT_FILES: &str = "agents are files in kelpie's home's `agents` folder";
const REVIEWER_FILES: &str = "reviewers are agent files listed in `agents.reviewers`";
const BOT_FILES: &str = "review bots are reviewer agent files on the `bot` harness, listed \
                         in `agents.reviewers` and run once a pass in their place";
const ONCE_A_PASS: &str = "a review bot reads once a pass, so no round cap is counted";

// The keys removed features left behind
const REMOVED: &[Removed] = &[
    Removed {
        key: "ruling_channels",
        because: RELAY,
        fix: DELETE,
    },
    Removed {
        key: "models.relay",
        because: RELAY,
        fix: DELETE,
    },
    Removed {
        key: "planning",
        because: PLANNING,
        fix: DELETE,
    },
    Removed {
        key: "models.planner",
        because: PLANNING,
        fix: DELETE,
    },
    Removed {
        key: "agents.planner",
        because: PLANNING,
        fix: DELETE,
    },
    Removed {
        key: "skills.planning",
        because: PLANNING,
        fix: DELETE,
    },
    Removed {
        key: "models.auditor",
        because: AUDIT,
        fix: DELETE,
    },
    Removed {
        key: "agents.auditor",
        because: AUDIT,
        fix: DELETE,
    },
    Removed {
        key: "models.judge",
        because: LOOP,
        fix: DELETE,
    },
    Removed {
        key: "agents.judge",
        because: LOOP,
        fix: DELETE,
    },
    Removed {
        key: "review.loop_guard",
        because: LOOP,
        fix: DELETE,
    },
    Removed {
        key: "review.local_rounds",
        because: LOOP,
        fix: DELETE,
    },
    Removed {
        key: "preview",
        because: SHOTS,
        fix: DELETE,
    },
    Removed {
        key: "agents.worker",
        because: AGENT_FILES,
        fix: "list the agents that build in `agents.implementers`, the first being the default",
    },
    Removed {
        key: "models.worker",
        because: AGENT_FILES,
        fix: "name the agent that builds in `agents.implementers`, such as `sonnet-high`, \
              which is Sonnet 5.5 at high",
    },
    Removed {
        key: "models.labels",
        because: "an `agent:<name>` label names its agent file, which holds the model id",
        fix: "list each agent a label may name in `agents.implementers`",
    },
    Removed {
        key: "models.reviewer",
        because: REVIEWER_FILES,
        fix: "write the model and effort as a reviewer's agent file, or list kelpie's \
              `defect-hunter`, Opus 5.5 at high, in `agents.reviewers`",
    },
    Removed {
        key: "models.deep_reviewer",
        because: REVIEWER_FILES,
        fix: "list `defect-hunter` in `agents.reviewers`: it is the deep round, and its \
              file holds the model and effort",
    },
    Removed {
        key: "models",
        because: "agents are files in kelpie's home's `agents` folder, which hold each \
                  model and effort",
        fix: DELETE,
    },
    Removed {
        key: "agents.reviewer",
        because: REVIEWER_FILES,
        fix: "list the reviewer's agent file in `agents.reviewers`",
    },
    Removed {
        key: "agents.deep_reviewer",
        because: REVIEWER_FILES,
        fix: "list `defect-hunter`, or a reviewer file with `second_look: true`, in \
              `agents.reviewers`",
    },
    Removed {
        key: "review.reviewers",
        because: REVIEWER_FILES,
        fix: "list them in `agents.reviewers`: `deep` is now `defect-hunter`, and \
              `claude` and each of kelpie's `[local_reviewers]` an agent file of its own",
    },
    Removed {
        key: "review.local",
        because: REVIEWER_FILES,
        fix: "write the round as an agent file with `role: reviewer`, its `kind` as \
              `harness` and `gpu_lease = true` as `lease: gpu`, and list it in \
              `agents.reviewers`; `kind = \"off\"` is `agents.reviewers = [\"defect-hunter\"]`",
    },
    Removed {
        key: "review",
        because: REVIEWER_FILES,
        fix: DELETE,
    },
    Removed {
        key: "pull_request_reviewers",
        because: BOT_FILES,
        fix: "list each bot's file, `coderabbit`, `cubic` or `codex`, in `agents.reviewers` \
              where its read should come, which `shep kelpie add` writes out",
    },
    Removed {
        key: "coderabbit.enabled",
        because: BOT_FILES,
        fix: "for `true`, list `coderabbit` where its read should come, as in \
              LISTED_WITH_CODERABBIT, and for `false` leave it out; then delete the key",
    },
    Removed {
        key: "coderabbit.rounds",
        because: BOT_FILES,
        fix: "set `rounds` in the bot's file, such as `agents/coderabbit.md`, where it is \
              the most reads that bot makes of a work item's pull request",
    },
    Removed {
        key: "coderabbit.divisor",
        because: ONCE_A_PASS,
        fix: DELETE,
    },
    Removed {
        key: "coderabbit",
        because: BOT_FILES,
        fix: DELETE,
    },
    Removed {
        key: "generated",
        because: "it only kept files out of the review bot cap's changed-line count, and \
                  a review bot now reads once a pass",
        fix: DELETE,
    },
];

/// Who decides a merge
///
/// `auto` replaces only the merge ruling: every other ruling still asks.
/// `ask-surface` is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MergeAuthority {
    /// Kelpie asks for a ruling before every merge
    Ask,
    /// Kelpie merges once every gate passes, then posts a notice of the merge
    Auto,
}

/// One role's model and effort
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleModel {
    /// A model id or alias, passed to `claude --model` as written
    pub model: NonBlank,
    /// Passed to `claude --effort`
    pub effort: Effort,
    /// The harness it runs on: Claude Code, unless an agent names another
    #[serde(skip)]
    #[schemars(skip)]
    pub harness: AgentHarness,
}

/// A Claude effort level
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// `low`
    Low,
    /// `medium`
    Medium,
    /// `high`
    High,
    /// `xhigh`
    Xhigh,
    /// `max`
    Max,
}

impl Effort {
    /// The level as `claude --effort` takes it
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// The level `claude --effort` takes as `s`, if any
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Low, Self::Medium, Self::High, Self::Xhigh, Self::Max]
            .into_iter()
            .find(|e| e.as_str() == s)
    }
}

/// Usage pacing settings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pacing {
    /// Whether the daily allowance and the 5-hour window hold anything
    ///
    /// Off, usage is still read for `status`, but nothing is held. On when
    /// absent.
    #[serde(default = "default_pacing_enabled")]
    pub enabled: bool,
    /// Hours a day the daily allowance is spread over
    pub kickoff_hours: KickoffHours,
}

fn default_pacing_enabled() -> bool {
    true
}

/// What every worker is started with
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    /// Domains the worker's sandbox may reach besides GitHub, such as a
    /// package registry. Empty when absent.
    #[serde(default)]
    pub allowed_domains: Vec<NonBlank>,
    /// Environment variables set to a folder inside the worker's build
    /// folder, for tool caches the sandbox would refuse elsewhere. Empty when
    /// absent.
    #[serde(default)]
    pub build_env: BTreeMap<EnvName, BuildDir>,
    /// A file of extra instructions for every worker, appended to kelpie's
    /// own: rules the repo's docs don't carry. A leading `~/` is the home
    /// folder, and a relative path is taken from the settings file's folder.
    /// None when absent.
    #[serde(default)]
    pub instructions_file: Option<PathBuf>,
    /// The project's own hooks, copied into each worker's settings file
    /// after kelpie's guard. Each must resolve when the runner starts.
    /// Empty when absent.
    #[serde(default)]
    pub guard_hooks: Vec<GuardHook>,
    /// Minutes a worker's turn may run before kelpie stops it and parks it
    /// on a ruling, keeping its session. 60 when absent.
    #[serde(default = "default_turn_timeout")]
    pub turn_timeout: NonZeroU32,
}

/// One work item at a time, as every project ran before `max_items`
fn default_max_items() -> NonZeroU32 {
    NonZeroU32::MIN
}

/// The design log's default for `worker.turn_timeout`, in minutes
fn default_turn_timeout() -> NonZeroU32 {
    NonZeroU32::MIN.saturating_add(59)
}

/// One of a project's own guard hooks, run by path
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuardHook {
    /// The Claude Code hook event it runs on
    pub event: HookEvent,
    /// The tool names it runs for; every tool when absent
    pub matcher: Option<NonBlank>,
    /// The shell command Claude Code runs
    pub command: NonBlank,
}

/// A Claude Code hook event a guard can run on
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum HookEvent {
    /// Before a tool call, which the hook can refuse
    PreToolUse,
    /// After a tool call
    PostToolUse,
}

/// An environment variable's name: capitals, digits and `_`, not starting with a digit
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct EnvName(String);

impl EnvName {
    /// The name as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EnvName {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let mut chars = value.chars();
        let first = chars
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_');
        if !first || !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
            return Err("must be an environment variable name like `BUN_INSTALL_CACHE_DIR`");
        }
        Ok(Self(value))
    }
}

/// A folder inside the build folder: relative, with no `..`
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct BuildDir(PathBuf);

impl BuildDir {
    /// The folder, relative to the build folder
    #[inline]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl TryFrom<String> for BuildDir {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let path = PathBuf::from(value);
        let inside = path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
        if path.as_os_str().is_empty() || !inside {
            return Err("must be a relative folder inside the build folder, with no `..`");
        }
        Ok(Self(path))
    }
}

/// A string with something other than whitespace in it
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct NonBlank(String);

impl NonBlank {
    /// The string as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for NonBlank {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty() {
            return Err("must not be blank");
        }
        Ok(Self(value))
    }
}

/// A forge repo as `owner/name`
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct ForgeSlug(String);

impl ForgeSlug {
    /// The slug as `owner/name`
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The repo's name, without its owner
    pub fn name(&self) -> &str {
        self.0.split_once('/').map_or(&self.0, |(_, name)| name)
    }
}

impl TryFrom<String> for ForgeSlug {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let part = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        match value.split_once('/') {
            Some((owner, name)) if part(owner) && part(name) => Ok(Self(value)),
            _ => Err("must be `owner/name`"),
        }
    }
}

/// Hours in a working day, from 1 to 24
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "i64")]
// schemars describes a `try_from` type by its source, so the range goes here.
#[schemars(extend("minimum" = 1, "maximum" = 24))]
pub struct KickoffHours(u8);

impl KickoffHours {
    /// The number of hours
    #[inline]
    pub fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<i64> for KickoffHours {
    type Error = &'static str;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match u8::try_from(value) {
            Ok(hours @ 1..=24) => Ok(Self(hours)),
            _ => Err("must be from 1 to 24"),
        }
    }
}

/// Why a project's settings cannot be used
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    /// The settings file could not be read
    Read {
        /// The file
        path: PathBuf,
        /// What reading it failed with
        kind: io::ErrorKind,
    },
    /// The file is not valid TOML, or a setting is missing, unknown or malformed
    Parse {
        /// The file
        path: PathBuf,
        /// The parser's message, which names the setting and its line
        message: String,
    },
    /// Neither the table nor the file it stands in for is there
    Unset {
        /// The table, as `[app.dogs.kelpie] table on the <sheep> sheep`
        table: String,
        /// The file
        path: PathBuf,
    },
    /// A sheep's table has a setting missing, unknown or malformed
    Table {
        /// The runner sheep carrying the table
        sheep: String,
        /// Names the setting as a dotted key, and what is wrong with it
        message: String,
    },
    /// Kelpie's `[kelpie]` section of `dogs.toml` is malformed
    Section {
        /// Names the line that is wrong, never its text
        message: String,
    },
    /// A well-formed setting that does not hold on this machine
    Invalid {
        /// The setting's dotted name
        setting: &'static str,
        /// What is wrong with it
        reason: String,
    },
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, kind } => {
                write!(f, "cannot read settings file {}: {kind}", path.display())
            }
            Self::Parse { path, message } => {
                write!(f, "settings file {}: {message}", path.display())
            }
            Self::Unset { table, path } => {
                write!(f, "there is no {table}, and no {}", path.display())
            }
            Self::Table { sheep, message } => {
                write!(f, "the [app.dogs.kelpie] table on {sheep}: {message}")
            }
            Self::Section { message } => {
                write!(f, "the [kelpie] section of dogs.toml: {message}")
            }
            Self::Invalid { setting, reason } => write!(f, "setting `{setting}`: {reason}"),
        }
    }
}

impl core::error::Error for SettingsError {}

impl Settings {
    /// Reads and checks a settings file, expanding `~/` in `repo` against `home`
    ///
    /// # Errors
    ///
    /// - [`SettingsError::Read`] when the file cannot be read.
    /// - [`SettingsError::Parse`] when a setting is missing, unknown or malformed.
    pub fn load(path: &Path, home: &Path) -> Result<Self, SettingsError> {
        let text = std::fs::read_to_string(path).map_err(|e| SettingsError::Read {
            path: path.to_owned(),
            kind: e.kind(),
        })?;
        let mut settings = Self::parse(&text, home).map_err(|message| SettingsError::Parse {
            path: path.to_owned(),
            message,
        })?;
        if let Some(folder) = path.parent() {
            settings.relative_to(folder);
        }
        Ok(settings)
    }

    pub(crate) fn parse(text: &str, home: &Path) -> Result<Self, String> {
        refuse_removed_settings(text, home)?;
        let mut settings: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        settings.expand(home);
        Ok(settings)
    }

    // `~/` in a path setting is the home folder.
    fn expand(&mut self, home: &Path) {
        if let Ok(rest) = self.repo.strip_prefix("~") {
            self.repo = home.join(rest);
        }
        for file in self.files_mut() {
            if let Ok(rest) = file.strip_prefix("~") {
                *file = home.join(rest);
            }
        }
    }

    // A relative path setting is taken from the project's folder.
    fn relative_to(&mut self, folder: &Path) {
        for file in self.files_mut().filter(|file| file.is_relative()) {
            *file = folder.join(&*file);
        }
    }

    // The paths that expand `~/` and are taken from the project's folder.
    fn files_mut(&mut self) -> impl Iterator<Item = &mut PathBuf> {
        [self.worker.instructions_file.as_mut()]
            .into_iter()
            .flatten()
            .chain(self.skills.paths_mut())
    }
}

/// Refuses `text` when it sets a key a removed feature left behind
///
/// The fix for `coderabbit.enabled` shows the project's reviewers, as its
/// table lists them or as `add` would, with `coderabbit` after them.
///
/// # Errors
///
/// A message naming every removed key `text` sets, each with what replaces it.
pub(crate) fn refuse_removed_settings(text: &str, home: &Path) -> Result<(), String> {
    removed::refuse(text, REMOVED).map_err(|message| {
        let table = text.parse::<toml::Table>().unwrap_or_default();
        let listed = table
            .get("agents")
            .and_then(|agents| agents.get("reviewers"))
            .and_then(toml::Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(|n| n.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_else(|| {
                let names = default_reviewers(home).into_iter();
                names.map(|n| n.as_str().to_owned()).collect::<Vec<_>>()
            });
        let all: Vec<String> = listed
            .into_iter()
            .chain(["coderabbit".to_owned()])
            .collect();
        message.replace(
            "LISTED_WITH_CODERABBIT",
            &format!("`agents.reviewers = {all:?}`"),
        )
    })
}

#[cfg(test)]
mod tests;
