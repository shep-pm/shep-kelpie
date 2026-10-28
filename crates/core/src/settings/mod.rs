//! A project's settings
//!
//! Its runner sheep's `[app.dogs.kelpie]` table, every setting in a table of
//! its own. Unknown keys are refused, so a misspelt or malformed setting
//! stops the runner with a message naming it. `git.checkout`,
//! `git.merging`, `ci.block` and `pacing.kickoff_hours` are required, with
//! the tables that hold them and `[worker]`, and every other key has a
//! default. `settings.example.toml` beside this crate holds the defaults.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use shep_client::dogs::dog_config;

pub mod source;
mod table;

pub use table::table_of;
mod agents;
mod browser;
mod budget;
mod builds;
mod ci;
mod code_graph;
mod contributions;
mod dependencies;
mod domains;
mod e2e;
mod gateway;
mod git;
mod hooks;
mod local;
mod local_rounds;
mod removed;
mod repo_setup;
mod reviewers;
mod rulings;
mod skills;
mod tickets;
mod triage;

pub use agents::{
    Account, AgentHarness, AgentName, Allow, DEFAULT_SLACK_MINUTES, DoctorProbe, ExpertAgent,
    GatewayKeys, Gone, Harness, HarnessFacts, Implementer, Limit, ModelServer, PmAgent,
    RoleAgentNames, RoleAgents, SessionIds, UsageReader,
};
pub use browser::Browser;
pub use budget::{Budget, PerItemUsd};
pub use builds::Builds;
pub use ci::{Ci, Concurrency, FixAttempts, TestCommand};
pub use code_graph::CodeGraph;
pub use contributions::{Contributions, Pick};
pub use dependencies::{Dependencies, DependencyPick, OnRed};
pub use e2e::{E2e, RepoPath, ResultsDir, SshAlias};
pub use gateway::{
    Gateway, GatewayKey, GatewayKind, GatewayName, Gateways, KeyVar, ModelHost, Route, Timing,
};
pub use git::{BaseBranch, DEFAULT_BASE, Filing, Git, GithubLogin, Merging};
pub use hooks::{HookPoint, PointKind, ProjectHook};
pub use local::{ContextSize, Endpoint, EndpointUrl, LocalCommand, LocalRound};
pub use local_rounds::{KeepDays, LocalRounds, RoundsFolder};
pub use repo_setup::{CallRole, MarketplaceSource, Part, PluginId, RepoSetup};
pub use reviewers::{ListedReviewer, default_reviewers};
pub use rulings::{PM_ALONE, PM_DECIDES, RulingName, Rulings};
pub use skills::{SkillChoice, SkillName, StepSkills};
pub use tickets::{TicketSource, Tickets};
pub use triage::{AuthorRole, ClassifierPath, Triage, TriagePick, TriageSpam};

/// Everything kelpie reads about one project
#[dog_config]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// The project's repo: its checkout, its GitHub repo, who merges, and
    /// what becomes of deferred findings
    pub git: Git,
    /// The project's CI: whether a merge waits for it, and how many fix
    /// turns a red run gets
    pub ci: Ci,
    /// How many work items run at once, and how many may wait on rulings
    /// before the board opens no more. Its defaults when absent.
    #[serde(default)]
    pub concurrency: Concurrency,
    /// What one work item may spend before kelpie raises a ruling. Its default
    /// when absent.
    #[serde(default)]
    pub budget: Budget,
    /// The agents that review the project's pull requests and the ones it
    /// may use, from kelpie's agent files
    #[serde(default)]
    pub agents: RoleAgentNames,
    /// Usage pacing
    pub pacing: Pacing,
    /// What every worker is started with
    pub worker: Worker,
    /// Which kinds of ruling the project manager answers for the
    /// maintainer. None when absent.
    #[serde(default)]
    pub rulings: Rulings,
    /// Whether and how kelpie picks up other people's pull requests. It
    /// picks up none when absent.
    #[serde(default)]
    pub contributions: Contributions,
    /// Whether and how kelpie triages issues other people open. It triages
    /// none when absent.
    #[serde(default)]
    pub triage: Triage,
    /// Whether kelpie merges the bumps bots open when they pass, and closes
    /// those that break. It follows none when absent.
    #[serde(default)]
    pub dependencies: Dependencies,
    /// The project's own hooks, which kelpie runs at its own points for
    /// every harness and role. Each must resolve when the runner starts.
    /// Empty when absent.
    #[serde(default)]
    pub hooks: Vec<ProjectHook>,
    /// The skill each step runs, over kelpie's vendored defaults
    #[serde(default)]
    pub skills: StepSkills,
    /// Which of the repo's plugins reach which calls, and the plugins to add
    /// that the repo does not turn on. Every plugin the repo turns on, for
    /// every call, when absent.
    #[serde(default)]
    pub repo_setup: RepoSetup,
    /// Where the project's tickets live: GitHub issues, or local files in
    /// kelpie's home. GitHub when absent.
    #[serde(default)]
    pub tickets: Tickets,
    /// Whether workers and reviewers get a code graph of the worktree. Off
    /// when absent.
    #[serde(default)]
    pub code_graph: CodeGraph,
    /// Whether a worker drives a browser to see the UI it builds, which
    /// hosts it reaches, and whether it is headed. Off when absent.
    #[serde(default)]
    pub browser: Browser,
    /// Whether each local round is kept after its work item ends, for how
    /// long, and where. Kept for 30 days in `local-rounds` in the project's
    /// folder when absent.
    #[serde(default)]
    pub local_rounds: LocalRounds,
    /// How much a work item's build folder may hold, and the free space the
    /// disk keeps. 20 GB each when absent.
    #[serde(default)]
    pub builds: Builds,
    /// The host that runs the project's end-to-end tests for a worker, whose
    /// sandbox cannot. It must be disposable: the project's script runs there
    /// as the ssh alias's account, and a worker edits that script. None when
    /// absent, and a worker asking for a run is told the project has none.
    #[serde(default)]
    pub e2e: Option<E2e>,
}

pub use removed::removed_command;
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
const GROUPED: &str = "every project setting sits in a table now";
const PANEL: &str = "the ruling panel is gone: the project manager answers the kinds \
                     `rulings.pm_decides` sets `true`, and the maintainer the rest";
const WRITER: &str = "the issue writer is gone, and an issue is written with kelpie's \
                      `issue` skill, which every harness has";
const MIGRATE: &str = "delete it, or run `shep kelpie settings migrate`, which does";

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
        key: "skills.review",
        because: "no step drives `code-review` now: a reviewer's prompt is the body of its \
                  agent file",
        fix: "write the prompt as the body of a reviewer's agent file, listed in \
              `agents.reviewers`, and delete the key",
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
        fix: "a worker looks at the UI through the `[app.dogs.kelpie.browser]` table instead",
    },
    Removed {
        key: "agents.worker",
        because: AGENT_FILES,
        fix: "delete it: the agent board picks each work item's implementer from the agent \
              files whose role is `implementer`",
    },
    Removed {
        key: "models.worker",
        because: AGENT_FILES,
        fix: "write an agent file whose role is `implementer`, such as kelpie's own \
              `sonnet-high`, which is Sonnet 5.5 at high",
    },
    Removed {
        key: "models.labels",
        because: "an `agent:<name>` label names its agent file, which holds the model id",
        fix: "write an agent file whose role is `implementer` for each agent a label names",
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
              `harness`, and list it in `agents.reviewers`; `kind = \"off\"` is \
              `agents.reviewers = [\"defect-hunter\"]`",
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
        key: "rulings.decide",
        because: PANEL,
        fix: MIGRATE,
    },
    Removed {
        key: "rulings.full_panel_paths",
        because: PANEL,
        fix: MIGRATE,
    },
    Removed {
        key: "agents.advisor",
        because: PANEL,
        fix: MIGRATE,
    },
    Removed {
        key: "agents.researcher",
        because: PANEL,
        fix: MIGRATE,
    },
    Removed {
        key: "researcher",
        because: PANEL,
        fix: MIGRATE,
    },
    Removed {
        key: "agents.issue_writer",
        because: WRITER,
        fix: MIGRATE,
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
    Removed {
        key: "repo",
        because: GROUPED,
        fix: "move its value to `git.checkout`",
    },
    Removed {
        key: "forge",
        because: GROUPED,
        fix: "move its value to `git.remote`, or delete it to read the repo from the \
              checkout's `origin`",
    },
    Removed {
        key: "merge_authority",
        because: "it set both who merges and whether deferred findings are asked about",
        fix: "move its value to `git.merging`, and set `git.issues`: `ask` asks before \
              filing deferred findings as `ask` did, and `file` files them as `auto` did",
    },
    Removed {
        key: "ci",
        because: GROUPED,
        fix: "move its value to `block` in a `[ci]` table",
    },
    Removed {
        key: "max_items",
        because: GROUPED,
        fix: "move its value to `concurrency.active_items`",
    },
    Removed {
        key: "max_parked",
        because: GROUPED,
        fix: "move its value to `concurrency.pending_rulings`",
    },
    Removed {
        key: "private_names",
        because: "kelpie no longer checks text for the project's own words",
        fix: "check text for the project's own words with a hook in `hooks`, which \
              kelpie runs for every harness and role, and delete it",
    },
    Removed {
        key: "skills.ci",
        because: "the step is named for what it does, apart from the `[ci]` table",
        fix: "move its value to `skills.ci_fix`",
    },
];

// The keys removed above that name a table now, refused only when set to a value
const NOW_TABLES: &[&str] = &["ci"];

/// The only tools a Claude Code reviewer's session starts with, as `--tools`
/// takes them: it reads the worktree and runs no command
pub const CLAUDE_REVIEW_TOOLS: &str = "Read,Glob,Grep";

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
    /// The tools its agent file names for a Claude Code session, as
    /// `claude --tools` takes them, in place of the role's own
    #[serde(skip)]
    #[schemars(skip)]
    pub tools: Option<NonBlank>,
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
    /// package registry. Each is a host such as `crates.io`, with one
    /// optional leading `*.` label (`*.crates.io`) and an optional `:port`.
    /// A `*` anywhere else, a scheme or a path is refused when the settings
    /// load, since the sandbox would refuse to start. Empty when absent.
    #[serde(default, deserialize_with = "domains::allowed_domains")]
    pub allowed_domains: Vec<NonBlank>,
    /// Whether a call that works an item also reaches the registries of the
    /// package managers its worktree keeps a lockfile or manifest for, such as
    /// `registry.npmjs.org` for a `bun.lock` and `crates.io` for a `Cargo.lock`,
    /// with no entry in `allowed_domains`. Off, only `allowed_domains` adds
    /// hosts. On when absent.
    #[serde(default = "default_package_registries")]
    pub package_registries: bool,
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
    /// The project's own Claude Code hooks, copied into each worker's
    /// settings file after kelpie's guard. Only a worker on Claude Code runs
    /// them; `hooks` runs for every harness and role. Each must resolve when
    /// the runner starts. Empty when absent.
    #[serde(default)]
    pub guard_hooks: Vec<GuardHook>,
    /// Minutes a worker's turn may run before kelpie stops it and parks it
    /// on a ruling, keeping its session. 60 when absent.
    #[serde(default = "default_turn_timeout")]
    pub turn_timeout: NonZeroU32,
    /// Tokens of context at which Claude Code compacts a worker's session in
    /// the middle of a turn. 200000 when absent; 0 leaves Claude Code to
    /// compact near the model's own window.
    #[serde(default = "default_compact_window")]
    pub compact_window: u32,
}

/// The design's default for `worker.package_registries`
fn default_package_registries() -> bool {
    true
}

/// The design's default for `worker.compact_window`, in tokens
fn default_compact_window() -> u32 {
    200_000
}

/// The design's default for `worker.turn_timeout`, in minutes
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

    /// The repo's owner, a user or an organization
    pub fn owner(&self) -> &str {
        self.0.split_once('/').map_or(&self.0, |(owner, _)| owner)
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

/// The GitHub repo a remote's URL names, over HTTPS or SSH
pub fn forge_of(url: &str) -> Option<ForgeSlug> {
    let path = [
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
        "git@github.com:",
    ]
    .iter()
    .find_map(|prefix| url.strip_prefix(prefix))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    ForgeSlug::try_from(path.to_owned()).ok()
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
    /// The table is not there
    Unset {
        /// The table, as `[app.dogs.kelpie] table on the <sheep> sheep`
        table: String,
        /// The settings file an older kelpie read in its place, where one is still there
        old_file: Option<PathBuf>,
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
            Self::Unset {
                table,
                old_file: None,
            } => write!(f, "there is no {table}"),
            Self::Unset {
                table,
                old_file: Some(file),
            } => write!(
                f,
                "there is no {table}, and kelpie no longer reads {}: move its keys into the table",
                file.display()
            ),
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
    // `~/` in a path setting is the home folder.
    fn expand(&mut self, home: &Path) {
        if let Ok(rest) = self.git.checkout.strip_prefix("~") {
            self.git.checkout = home.join(rest);
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
            .chain(
                self.local_rounds
                    .folder
                    .as_mut()
                    .map(RoundsFolder::path_mut),
            )
            .chain(self.skills.paths_mut())
    }
}

/// Refuses `text` when it sets a key a removed feature left behind
///
/// The fix for `coderabbit.enabled` shows the project's reviewers, as its
/// table lists them or as the default gives them with no paddock gateway, with
/// `coderabbit` after them.
///
/// # Errors
///
/// A message naming every removed key `text` sets, each with what replaces it.
pub(crate) fn refuse_removed_settings(text: &str) -> Result<(), String> {
    removed::refuse(text, REMOVED, NOW_TABLES).map_err(|message| {
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
                let names = default_reviewers(&Gateways::default()).into_iter();
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
