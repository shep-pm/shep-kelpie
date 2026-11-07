//! Whether kelpie merges and closes the bumps bots open,
//! `[app.dogs.kelpie.dependencies]`

use std::num::NonZeroU32;

use schemars::JsonSchema;
use serde::Deserialize;

/// Whether kelpie follows the bumps bots open
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyPick {
    /// No: kelpie leaves every bot's pull request alone
    #[default]
    Off,
    /// Yes: it brings each bump up to date, proves it and merges or closes it
    On,
}

/// Whether kelpie merges the bumps bots open
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dependencies {
    /// Whether kelpie follows the bumps. `off` when absent.
    #[serde(default)]
    pub pick: DependencyPick,
    /// The bot logins whose pull requests are bumps, compared ignoring case
    /// and a `[bot]` suffix. `["dependabot"]` when absent.
    #[serde(default = "default_bots")]
    pub bots: Vec<String>,
    /// Paths whose change makes a bump a web bump, which also needs a passing
    /// end-to-end run. A path ending in `/` is a folder. Empty when absent.
    #[serde(default)]
    pub web: Vec<String>,
    /// How many hours a bump's checks may stay pending, or never start,
    /// before it counts as red, above 0. 24 when absent.
    #[serde(default = "default_wait_hours")]
    pub wait_hours: NonZeroU32,
}

impl Default for Dependencies {
    fn default() -> Self {
        Self {
            pick: DependencyPick::default(),
            bots: default_bots(),
            web: Vec::new(),
            wait_hours: default_wait_hours(),
        }
    }
}

fn default_bots() -> Vec<String> {
    vec!["dependabot".to_owned()]
}

fn default_wait_hours() -> NonZeroU32 {
    NonZeroU32::new(24).unwrap_or(NonZeroU32::MIN)
}

// A login without its `[bot]` suffix.
fn bare(login: &str) -> &str {
    login.strip_suffix("[bot]").unwrap_or(login)
}

impl Dependencies {
    /// Whether kelpie follows no bump
    #[inline]
    pub fn is_off(&self) -> bool {
        self.pick == DependencyPick::Off
    }

    /// The name a comment mentions `login` by: the login without a `[bot]`
    /// suffix, which GitHub does not resolve in a mention
    pub fn mention(login: &str) -> &str {
        bare(login)
    }

    /// Whether `login` is one of the listed bots, ignoring case and a `[bot]`
    /// suffix on either side
    pub fn lists(&self, login: &str) -> bool {
        let login = bare(login);
        self.bots
            .iter()
            .any(|bot| bare(bot).eq_ignore_ascii_case(login))
    }

    /// Whether changing `path` makes a bump a web bump
    ///
    /// A listed path matches itself, and anything below it when it ends in
    /// `/`.
    pub fn is_web(&self, path: &str) -> bool {
        self.web.iter().any(|listed| match listed.ends_with('/') {
            true => path.starts_with(listed.as_str()),
            false => path == listed,
        })
    }
}
