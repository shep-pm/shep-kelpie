//! `[app.dogs.kelpie.local_rounds]`: how long a project keeps its local rounds, and where

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;

const DAYS: u32 = 30;

/// How long a project keeps its local rounds, and where. Every round is kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "Local rounds")]
pub struct LocalRounds {
    /// Kept rounds older than this many days are removed; 0 keeps them all.
    /// 30 when absent.
    #[serde(default)]
    pub keep_days: KeepDays,
    /// Where kept rounds go: an absolute path, or `~/` and a path. `local-rounds`
    /// in the project's kelpie folder when absent.
    #[serde(default)]
    pub folder: Option<RoundsFolder>,
}

/// Days a kept round stays, from 0 (for ever)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "i64")]
// schemars describes a `try_from` type by its source, so the bound goes here.
#[schemars(extend("minimum" = 0, "maximum" = u32::MAX))]
pub struct KeepDays(u32);

impl KeepDays {
    /// The number of days, 0 for no limit
    #[inline]
    pub fn get(self) -> u32 {
        self.0
    }
}

impl Default for KeepDays {
    fn default() -> Self {
        Self(DAYS)
    }
}

impl TryFrom<i64> for KeepDays {
    type Error = &'static str;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        u32::try_from(value)
            .map(Self)
            .map_err(|_| "must be 0 or more days, and 0 keeps every round")
    }
}

/// An absolute folder, or `~/` and a path
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "String")]
pub struct RoundsFolder(PathBuf);

impl RoundsFolder {
    /// The folder
    #[inline]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    // Its leading `~/` is the home folder.
    pub(super) fn path_mut(&mut self) -> &mut PathBuf {
        &mut self.0
    }
}

impl TryFrom<String> for RoundsFolder {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let path = PathBuf::from(value);
        let home = path
            .strip_prefix("~")
            .is_ok_and(|rest| !rest.as_os_str().is_empty());
        if path.is_absolute() || home {
            return Ok(Self(path));
        }
        Err(
            "must be an absolute path, or `~/` and a path: a relative one would sit in a different place for each work item",
        )
    }
}
