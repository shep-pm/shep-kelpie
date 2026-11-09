//! The project's repo, `[app.dogs.kelpie.git]`

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

use super::ForgeSlug;

/// The project's repo: where it is checked out and where it lives on GitHub
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "Git")]
pub struct Git {
    /// The project's own checkout. A leading `~/` is the home folder.
    pub checkout: PathBuf,
    /// The repo on GitHub as `owner/name`. Read from the checkout's
    /// `origin` when the runner starts, when absent.
    #[serde(default)]
    pub remote: Option<ForgeSlug>,
    /// The branch work items are cut from, opened pull requests against and
    /// merged into, on `origin`. `main` when absent. A change reaches the work
    /// items opened after it: each one in flight keeps the base it was cut for.
    #[serde(default)]
    pub base: Option<BaseBranch>,
    /// The GitHub login kelpie mentions on a ruling it posts through its
    /// App, so GitHub notifies them. The login `gh` acts as (`gh api user`)
    /// when absent.
    #[serde(default)]
    pub maintainer: Option<GithubLogin>,
    /// When a pull request behind the base is caught up with it: `always`, or
    /// only when it `conflicts`. `always` when absent.
    #[serde(default)]
    pub catch_up: CatchUp,
}

/// When a branch behind the base is caught up with it
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CatchUp {
    /// Whenever it lacks the latest base: it is rebased and tested again
    #[default]
    Always,
    /// Only when merging it into the base conflicts. A branch that merges
    /// cleanly goes on as it stands, and what tests the combination is the
    /// repo's merge queue or the base's own CI.
    Conflicts,
}

/// The branch work items are cut from and merged into, on `origin`, when a
/// project names none
pub const DEFAULT_BASE: &str = "main";

impl Git {
    /// The project's base branch: what work items are cut from, compared
    /// against and merged into, on `origin`. `git.base`, or [`DEFAULT_BASE`].
    #[inline]
    pub fn base(&self) -> &str {
        self.base.as_ref().map_or(DEFAULT_BASE, BaseBranch::as_str)
    }
}

/// A branch name git accepts, as `git check-ref-format --branch` reads it
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct BaseBranch(String);

impl BaseBranch {
    /// The branch as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BaseBranch {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        const REASON: &str = "must be a branch name such as `main` or `release/2.x`";
        let forbidden = |c: char| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c);
        let bad_part =
            |part: &str| part.is_empty() || part.starts_with('.') || part.ends_with(".lock");
        if value.starts_with('-')
            || value.ends_with(['/', '.'])
            || value.contains("..")
            || value.contains("@{")
            || value == "@"
            || value.chars().any(forbidden)
            || value.split('/').any(bad_part)
        {
            return Err(REASON);
        }
        Ok(Self(value))
    }
}

/// A GitHub login: letters, digits and hyphens, at most 39, not starting
/// with a hyphen
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct GithubLogin(String);

impl GithubLogin {
    /// The login as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for GithubLogin {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let login = value.strip_prefix('@').unwrap_or(&value);
        let allowed = |c: char| c.is_ascii_alphanumeric() || c == '-';
        if login.is_empty()
            || login.len() > 39
            || login.starts_with('-')
            || !login.chars().all(allowed)
        {
            return Err("must be a GitHub login");
        }
        Ok(Self(login.to_owned()))
    }
}
