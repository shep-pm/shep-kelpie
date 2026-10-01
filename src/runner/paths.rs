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

impl fmt::Display for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<&str> for ProjectName {
    type Error = ProjectNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        if value.is_empty() || value.starts_with('.') || !value.chars().all(allowed) {
            return Err(ProjectNameError(value.to_owned()));
        }
        // A project's folder sits beside kelpie's own in kelpie's home.
        if crate::home::OWN.contains(&value) {
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
            "{:?} cannot name a project: use letters, digits, - _ . and not one of kelpie's \
             own folders ({}), as in `shep kelpie add <another name>`",
            self.0,
            crate::home::OWN.join(" ")
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
    /// The folder for the plugins that hold each step's skill
    pub skills: PathBuf,
    /// Kelpie's own tools for showing a work item's UI, which every project shares
    pub tools: Tools,
    /// Kelpie's home, which holds every folder here
    pub kelpie_home: PathBuf,
    /// The shepherd's home, which a worker may not read outside its own folders
    pub shep_home: PathBuf,
    /// The dog's door, the one socket a worker may connect to. Under
    /// kelpie's home unless the runner sets it from its own environment.
    pub door: PathBuf,
    worktrees: PathBuf,
    builds: PathBuf,
    shots: PathBuf,
    playwright: PathBuf,
}

impl ProjectPaths {
    /// `<kelpie home>/<project>/`, holding the project's state, settings and
    /// worker files, and its `worktrees`, `builds`, `shots` and `playwright`
    /// folders, beside kelpie's own `settings.toml`, `totp` and `tools`
    pub fn under(kelpie_home: &Path, shep_home: &Path, project: &ProjectName) -> Self {
        let folder = kelpie_home.join(project.as_str());
        Self {
            kelpie_settings: kelpie_home.join("settings.toml"),
            totp: kelpie_home.join("totp"),
            settings: folder.join("settings.toml"),
            state: folder.join("state.json"),
            worker: folder.join("worker"),
            skills: folder.join("skills"),
            tools: Tools::under(kelpie_home),
            kelpie_home: kelpie_home.to_owned(),
            shep_home: shep_home.to_owned(),
            door: kelpie_home.join("dog/lease.sock"),
            worktrees: folder.join("worktrees"),
            builds: folder.join("builds"),
            shots: folder.join("shots"),
            playwright: folder.join("playwright"),
        }
    }

    /// Whether the door, and the longest socket a call opens (a 128-bit
    /// random name in the worker folder), are short enough to bind
    ///
    /// # Errors
    ///
    /// A message naming the path that is too long.
    pub fn sockets_fit(&self) -> Result<(), String> {
        crate::home::socket_fits(&self.door)?;
        crate::home::socket_fits(&self.worker.join(format!("{}.sock", "0".repeat(32))))
    }

    /// The folders a dev server of this project's can work in: every
    /// worktree and every build folder
    pub fn owned(&self) -> [PathBuf; 2] {
        [self.worktrees.clone(), self.builds.clone()]
    }

    /// The worktree for the work item that resolves `issue`
    pub fn worktree(&self, issue: u64) -> PathBuf {
        self.worktrees.join(issue.to_string())
    }

    /// The detached worktree at `origin/main` a planning call reads, beside
    /// the work items' own, under a name no issue number takes
    pub fn plan(&self) -> PathBuf {
        self.worktrees.join("plan")
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
        for bad in ["", "a/b", "..", ".hidden", "sp ace", "dog", "tools"] {
            assert!(ProjectName::try_from(bad).is_err(), "{bad:?}");
        }
        assert!(ProjectName::try_from("shep-kelpie_2.0").is_ok());
    }

    #[test]
    fn a_socket_too_long_to_bind_is_named() {
        let koji = ProjectName::try_from("koji").unwrap();
        let fits = ProjectPaths::under(Path::new("/s/kelpie"), Path::new("/s"), &koji);
        assert_eq!(fits.sockets_fit(), Ok(()));
        let long = format!("/{}", "s".repeat(60));
        let home = Path::new(&long);
        let paths = ProjectPaths::under(&home.join("kelpie"), home, &koji);
        let error = paths.sockets_fit().unwrap_err();
        assert!(
            error.contains(&paths.worker.display().to_string()),
            "{error}"
        );
    }
}
