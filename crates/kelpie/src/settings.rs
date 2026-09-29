//! A project's settings
//!
//! Its runner sheep's `[app.dogs.kelpie]` table, or the settings file a
//! project had before one. Unknown keys are refused, so a misspelt or
//! malformed setting stops the runner with a message naming it. Every
//! setting is required except the ones added after the first build
//! (`max_items`, `review.local`, `pacing.enabled`, `worker.allowed_domains`,
//! `worker.build_env`, `worker.instructions_file`, `worker.turn_timeout`,
//! `ruling_channels` and `[preview]`).
//! `settings.example.toml` beside this crate holds the defaults.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use shep_client::dogs::dog_config;

use crate::channels::Channels;
use crate::preview::Preview;

pub mod moving;
pub mod source;
mod table;

pub use table::table_of;
mod local;

pub use local::{ContextSize, Endpoint, EndpointUrl, LocalCommand, LocalRound};

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
    /// Globs for files left out of a pull request's changed-line count
    pub generated: Vec<String>,
    /// The model and effort for each role
    pub models: Models,
    /// The review loop
    pub review: Review,
    /// The CodeRabbit gate
    pub coderabbit: CodeRabbit,
    /// Usage pacing
    pub pacing: Pacing,
    /// What every worker is started with
    pub worker: Worker,
    /// Showing a work item's UI, off unless `enabled` and a launch file on `main`
    #[serde(default)]
    pub preview: Preview,
    /// How rulings reach the maintainer, over what kelpie's own settings say.
    /// Kelpie's settings decide when absent.
    #[serde(default)]
    pub ruling_channels: Option<Channels>,
}

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

/// The model and effort for each role that calls Claude
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Models {
    /// The worker's sessions
    pub worker: RoleModel,
    /// Each Claude review round, a fresh session every time
    pub reviewer: RoleModel,
    /// The one-shot that judges every finding
    pub judge: RoleModel,
    /// The session that carries rulings to the maintainer
    pub relay: RoleModel,
}

/// One role's model and effort
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleModel {
    /// A model id or alias, passed to `claude --model` as written
    pub model: NonBlank,
    /// Passed to `claude --effort`
    pub effort: Effort,
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

/// The review loop's settings
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Review {
    /// Rounds after which the worker is parked for a ruling
    pub loop_guard: NonZeroU32,
    /// The local round, `[review.local]`. The maintainer's qwen-review
    /// script when absent, as every file before this table ran it.
    #[serde(default)]
    pub local: LocalRound,
}

/// The CodeRabbit gate's settings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeRabbit {
    /// Whether pull requests go through CodeRabbit rounds at all
    ///
    /// CodeRabbit's free plan reviews public repos only, so the runner
    /// refuses to start with this on for a private one.
    pub enabled: bool,
    /// Changed lines per extra round: the cap is `ceil(changed / divisor) + 1`
    pub divisor: NonZeroU32,
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
    /// Hooks copied into each worker's own settings file
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

/// One of the maintainer's guard hooks, run by path
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
        let local = match &mut self.review.local {
            LocalRound::Command(local) => Some(&mut local.command),
            LocalRound::Off {} | LocalRound::Endpoint(_) => None,
        };
        [self.worker.instructions_file.as_mut(), local]
            .into_iter()
            .flatten()
    }
}

#[cfg(test)]
mod tests;
