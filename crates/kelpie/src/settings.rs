//! The project's settings file
//!
//! One TOML file per project, read once when the runner starts. Every
//! setting is required and unknown keys are refused, so a missing, misspelt
//! or malformed setting stops the runner with a message naming it.
//! `settings.example.toml` beside this crate holds the defaults.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Everything kelpie reads about one project
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
    /// Globs for files left out of a pull request's changed-line count
    pub generated: Vec<String>,
    /// The model and effort for each role
    pub models: Models,
    /// The qwen-review loop
    pub review: Review,
    /// The CodeRabbit gate
    pub coderabbit: CodeRabbit,
    /// Usage pacing
    pub pacing: Pacing,
    /// What every worker is started with
    pub worker: Worker,
}

/// Who decides a merge
///
/// The first build asks the maintainer every time. `auto` and
/// `ask-surface` are refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeAuthority {
    /// Kelpie asks for a ruling before every merge
    Ask,
}

/// The model and effort for each role that calls Claude
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleModel {
    /// A model id or alias, passed to `claude --model` as written
    pub model: NonBlank,
    /// Passed to `claude --effort`
    pub effort: Effort,
}

/// A Claude effort level
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// The qwen-review loop's settings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    /// Rounds after which the worker is parked for a ruling
    pub loop_guard: NonZeroU32,
}

/// The CodeRabbit gate's settings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pacing {
    /// Hours a day the daily allowance is spread over
    pub kickoff_hours: KickoffHours,
}

/// What every worker is started with
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    /// Domains the worker's sandbox may reach besides GitHub, such as a
    /// package registry
    pub allowed_domains: Vec<NonBlank>,
    /// Environment variables set to a folder inside the worker's build
    /// folder, for tool caches the sandbox would refuse elsewhere
    pub build_env: BTreeMap<EnvName, BuildDir>,
    /// Hooks copied into each worker's own settings file
    pub guard_hooks: Vec<GuardHook>,
}

/// One of the maintainer's guard hooks, run by path
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum HookEvent {
    /// Before a tool call, which the hook can refuse
    PreToolUse,
    /// After a tool call
    PostToolUse,
}

/// An environment variable's name: capitals, digits and `_`, not starting with a digit
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "i64")]
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
            Self::Invalid { setting, reason } => write!(f, "setting `{setting}`: {reason}"),
        }
    }
}

impl std::error::Error for SettingsError {}

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
        Self::parse(&text, home).map_err(|message| SettingsError::Parse {
            path: path.to_owned(),
            message,
        })
    }

    fn parse(text: &str, home: &Path) -> Result<Self, String> {
        let mut settings: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if let Ok(rest) = settings.repo.strip_prefix("~") {
            settings.repo = home.join(rest);
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../settings.example.toml");

    fn parse(text: &str) -> Result<Settings, String> {
        Settings::parse(text, Path::new("/home/maintainer"))
    }

    fn parse_err(text: &str) -> String {
        parse(text).expect_err("the settings should be refused")
    }

    #[test]
    fn the_example_holds_the_first_build_defaults() {
        let s = parse(EXAMPLE).unwrap();
        assert_eq!(s.repo, Path::new("/home/maintainer/.kelpie/repos/shep"));
        assert_eq!(s.forge.as_str(), "shep-pm/shep");
        assert_eq!(s.merge_authority, MergeAuthority::Ask);
        assert!(s.ci);
        let role = |r: &RoleModel| (r.model.as_str().to_owned(), r.effort);
        assert_eq!(
            role(&s.models.worker),
            ("claude-sonnet-5".into(), Effort::Medium)
        );
        assert_eq!(
            role(&s.models.reviewer),
            ("claude-sonnet-5".into(), Effort::Medium)
        );
        assert_eq!(
            role(&s.models.judge),
            ("claude-opus-5-5".into(), Effort::Low)
        );
        assert_eq!(
            role(&s.models.relay),
            ("claude-haiku-4-5-20251001".into(), Effort::Low)
        );
        assert_eq!(s.review.loop_guard.get(), 8);
        assert!(s.coderabbit.enabled);
        assert_eq!(s.coderabbit.divisor.get(), 1000);
        assert_eq!(s.pacing.kickoff_hours.get(), 8);
        assert!(s.generated.iter().any(|g| g == "Cargo.lock"));
        assert_eq!(s.worker.guard_hooks[0].event, HookEvent::PreToolUse);
        let domains: Vec<&str> = s
            .worker
            .allowed_domains
            .iter()
            .map(NonBlank::as_str)
            .collect();
        assert_eq!(
            domains,
            ["crates.io", "index.crates.io", "static.crates.io"]
        );
    }

    #[test]
    fn a_missing_setting_is_named() {
        let text = EXAMPLE.replace("forge = \"shep-pm/shep\"\n", "");
        assert!(parse_err(&text).contains("missing field `forge`"));
    }

    #[test]
    fn a_missing_nested_setting_is_named_with_its_table() {
        let text = EXAMPLE.replace("divisor = 1000\n", "");
        let err = parse_err(&text);
        assert!(err.contains("missing field `divisor`"), "{err}");
        assert!(err.contains("[coderabbit]"), "{err}");
    }

    #[test]
    fn ci_must_be_said_either_way() {
        let err = parse_err(&EXAMPLE.replace("ci = true\n", ""));
        assert!(err.contains("missing field `ci`"), "{err}");
        assert!(
            !parse(&EXAMPLE.replace("ci = true", "ci = false"))
                .unwrap()
                .ci
        );
    }

    #[test]
    fn a_misspelt_setting_is_named() {
        let text = EXAMPLE.replace("loop_guard", "loop_gaurd");
        assert!(parse_err(&text).contains("unknown field `loop_gaurd`"));
    }

    #[test]
    fn merge_authority_other_than_ask_is_refused() {
        let text = EXAMPLE.replace("merge_authority = \"ask\"", "merge_authority = \"auto\"");
        let err = parse_err(&text);
        assert!(err.contains("merge_authority"), "{err}");
        assert!(err.contains("unknown variant `auto`"), "{err}");
    }

    #[test]
    fn a_zero_loop_guard_is_refused() {
        let text = EXAMPLE.replace("loop_guard = 8", "loop_guard = 0");
        assert!(parse_err(&text).contains("loop_guard = 0"));
    }

    #[test]
    fn kickoff_hours_outside_a_day_are_refused() {
        for hours in ["0", "25", "300", "-1"] {
            let line = format!("kickoff_hours = {hours}");
            let err = parse_err(&EXAMPLE.replace("kickoff_hours = 8", &line));
            assert!(err.contains(&line), "{err}");
            assert!(err.contains("must be from 1 to 24"), "{err}");
        }
    }

    #[test]
    fn every_effort_parses_from_what_claude_takes() {
        for effort in [
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ] {
            assert_eq!(Effort::parse(effort.as_str()), Some(effort));
        }
        assert_eq!(Effort::parse("Medium"), None);
    }

    #[test]
    fn an_unknown_effort_is_refused() {
        let text = EXAMPLE.replacen("effort = \"medium\"", "effort = \"huge\"", 1);
        assert!(parse_err(&text).contains("unknown variant `huge`"));
    }

    #[test]
    fn a_forge_slug_needs_an_owner_and_a_name() {
        for slug in [
            "shep",
            "/shep",
            "shep-pm/",
            "shep-pm/shep/x",
            "shep pm/shep",
        ] {
            let text = EXAMPLE.replace("\"shep-pm/shep\"", &format!("{slug:?}"));
            assert!(parse_err(&text).contains("must be `owner/name`"), "{slug}");
        }
    }

    #[test]
    fn a_blank_model_is_refused() {
        let text = EXAMPLE.replacen("model = \"claude-sonnet-5\"", "model = \" \"", 1);
        assert!(parse_err(&text).contains("must not be blank"));
    }

    #[test]
    fn build_env_names_folders_inside_the_build_folder() {
        let text = EXAMPLE.replace(
            "build_env = {}",
            r#"build_env = { BUN_INSTALL_CACHE_DIR = "bun/cache" }"#,
        );
        let s = parse(&text).unwrap();
        let [(name, dir)] = s
            .worker
            .build_env
            .iter()
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        assert_eq!(
            (name.as_str(), dir.as_path()),
            ("BUN_INSTALL_CACHE_DIR", Path::new("bun/cache"))
        );
        for bad in [
            "\"\"",
            "\"/tmp/bun\"",
            "\"../bun\"",
            "\"bun/../..\"",
            "\"./bun\"",
        ] {
            let line = format!("build_env = {{ BUN = {bad} }}");
            let err = parse_err(&EXAMPLE.replace("build_env = {}", &line));
            assert!(err.contains("inside the build folder"), "{bad}: {err}");
        }
        for bad in ["bun", "1BUN", "BUN-DIR"] {
            let line = format!("build_env = {{ {bad} = \"bun\" }}");
            let err = parse_err(&EXAMPLE.replace("build_env = {}", &line));
            assert!(err.contains("environment variable name"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_repo_path_without_a_tilde_is_kept() {
        let text = EXAMPLE.replace("\"~/.kelpie/repos/shep\"", "\"/srv/shep\"");
        assert_eq!(parse(&text).unwrap().repo, Path::new("/srv/shep"));
    }

    #[test]
    fn an_unreadable_file_names_its_path() {
        let err = Settings::load(Path::new("/nonexistent/settings.toml"), Path::new("/"));
        let err = err.unwrap_err();
        assert_eq!(
            err,
            SettingsError::Read {
                path: "/nonexistent/settings.toml".into(),
                kind: io::ErrorKind::NotFound,
            }
        );
        assert!(err.to_string().contains("/nonexistent/settings.toml"));
    }
}
