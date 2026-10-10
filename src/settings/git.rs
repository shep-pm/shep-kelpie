//! The project's repo and who merges into it, `[app.dogs.kelpie.git]`

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ForgeSlug;

/// The project's repo: where it is checked out, where it lives on GitHub,
/// who merges into it and what becomes of findings left for later
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
    /// Who merges a green, reviewed pull request
    pub merging: Merging,
    /// What becomes of the findings a worker deferred, once its pull
    /// request merges. `ask` when absent.
    #[serde(default)]
    pub issues: Filing,
    /// The GitHub login kelpie mentions on a ruling it posts through its
    /// App, so GitHub notifies them. The repo's owner when absent and the
    /// owner is a user; nobody is mentioned for an organization's repo.
    #[serde(default)]
    pub maintainer: Option<GithubLogin>,
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

/// Who merges a green, reviewed pull request
///
/// `auto` replaces only the merge ruling. A pull request no reviewer read,
/// one with a review bot's thread above a nit open, a head no round read, a
/// fix after a `no` and a late bot round's fix still get the merge ruling,
/// which names why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Merging {
    /// Kelpie asks for a ruling before every merge
    Ask,
    /// Kelpie merges once every gate passes, then posts a notice of the merge
    Auto,
}

/// What becomes of the findings a worker deferred, once its pull request merges
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Filing {
    /// Kelpie raises a ruling, and files them as issues on a yes
    #[default]
    Ask,
    /// Kelpie files them as issues at once
    File,
    /// Kelpie files nothing, and the findings stay in the review
    Skip,
}
