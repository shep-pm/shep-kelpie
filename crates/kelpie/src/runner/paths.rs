//! A project's name, and where its files live under kelpie's home

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::preview::Tools;

/// A project's name, which is also its sheep's name
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ProjectName(String);

impl ProjectName {
    /// The name as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for ProjectName {
    type Error = ProjectNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        if value.is_empty() || value.starts_with('.') || !value.chars().all(allowed) {
            return Err(ProjectNameError(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }
}

// A name read back from the dog's book file passes the same check.
impl<'de> Deserialize<'de> for ProjectName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::try_from(name.as_str()).map_err(serde::de::Error::custom)
    }
}

/// A name that is not one plain path component, carrying the name
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectNameError(pub String);

impl fmt::Display for ProjectNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} is not a project name: use letters, digits, - _ .",
            self.0
        )
    }
}

impl core::error::Error for ProjectNameError {}

/// Where a project's files live under kelpie's home
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPaths {
    /// Kelpie's own settings file, which every project shares
    pub kelpie_settings: PathBuf,
    /// The authenticator secret and the codes used, which every project shares
    pub totp: PathBuf,
    /// The settings file
    pub settings: PathBuf,
    /// The state file
    pub state: PathBuf,
    /// The folder for the worker's settings file and instructions
    pub worker: PathBuf,
    /// Kelpie's own tools for showing a work item's UI, which every project shares
    pub tools: Tools,
    /// The shepherd's home, which a worker may not read. Kelpie's own shepherd,
    /// `<kelpie home>/shep`, unless the runner's `SHEP_HOME` names another.
    pub shep_home: PathBuf,
    worktrees: PathBuf,
    builds: PathBuf,
    shots: PathBuf,
    playwright: PathBuf,
}

impl ProjectPaths {
    /// `<kelpie home>/projects/<project>/`, beside kelpie's own
    /// `<kelpie home>/settings.toml`, with worktrees under
    /// `<kelpie home>/wt/<project>/`, build folders under
    /// `<kelpie home>/targets/<project>/`, shots under
    /// `<kelpie home>/shots/<project>/`, the Playwright server's files under
    /// `<kelpie home>/playwright/<project>/`, and the shared `<kelpie home>/tools/`
    pub fn under(kelpie_home: &Path, project: &ProjectName) -> Self {
        let folder = kelpie_home.join("projects").join(project.as_str());
        Self {
            kelpie_settings: kelpie_home.join("settings.toml"),
            totp: kelpie_home.join("totp"),
            settings: folder.join("settings.toml"),
            state: folder.join("state.json"),
            worker: folder.join("worker"),
            tools: Tools::under(kelpie_home),
            shep_home: kelpie_home.join("shep"),
            worktrees: kelpie_home.join("wt").join(project.as_str()),
            builds: kelpie_home.join("targets").join(project.as_str()),
            shots: kelpie_home.join("shots").join(project.as_str()),
            playwright: kelpie_home.join("playwright").join(project.as_str()),
        }
    }

    /// The worktree for the work item that resolves `issue`
    pub fn worktree(&self, issue: u64) -> PathBuf {
        self.worktrees.join(issue.to_string())
    }

    /// The build folder for the work item that resolves `issue`
    pub fn build(&self, issue: u64) -> PathBuf {
        self.builds.join(issue.to_string())
    }

    /// Issue `issue`'s shots: out of its worker's reach for writes, not for
    /// reads. Only kelpie's own processes write there.
    pub fn shots(&self, issue: u64) -> PathBuf {
        self.shots.join(issue.to_string())
    }

    /// Issue `issue`'s Playwright MCP output folder, kept apart from its shots:
    /// a page the worker drives can download a file into it
    pub fn playwright(&self, issue: u64) -> PathBuf {
        self.playwright.join(issue.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_name_is_one_path_component() {
        for bad in ["", "a/b", "..", ".hidden", "sp ace"] {
            assert!(ProjectName::try_from(bad).is_err(), "{bad:?}");
        }
        assert!(ProjectName::try_from("shep-kelpie_2.0").is_ok());
    }
}
