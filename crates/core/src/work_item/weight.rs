//! What a work item's change weighs, which the review pass reports and the
//! project manager decides from
//!
//! Kelpie weighs a diff that touches only docs light and any other full.
//! `Lighter::Pm`, `Heavier::FullPaths` and `Heavier::High` are what a state file
//! saved before the project manager took the review plan can hold: kelpie calls
//! no change light for a tag, no file full for a glob and no finding a step-up.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What a work item's change weighs: light for a diff that touches only docs,
/// full for one that touches anything else
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewWeight {
    /// A change the weighing called light
    Light(Lighter),
    /// A change the weighing called full
    Full(Heavier),
}

/// Why a change weighs light
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lighter {
    /// Every changed file is one a docs-only change touches
    Docs,
    /// What a state file saved when the project manager's tag could lighten a
    /// change, which changes this many lines
    Pm {
        /// The lines it adds plus the lines it deletes
        lines: u32,
    },
}

/// Why a change weighs full
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Heavier {
    /// It adopts a pull request kelpie did not open
    Adopted,
    /// Its diff names no file, so nothing is called light
    Empty,
    /// It changes a workflow file
    Ci,
    /// It changes a lockfile or a manifest
    Dependencies,
    /// It changes a harness's own file
    AgentFiles,
    /// What a state file saved when a project setting could name a file full:
    /// it changes this file
    FullPaths(String),
    /// It changes this file, the first outside what a docs-only change touches
    Touched(String),
    /// What a state file saved when a HIGH finding stepped a light review up
    High,
}

impl ReviewWeight {
    /// Whether it weighs light
    pub fn is_light(&self) -> bool {
        matches!(self, Self::Light(_))
    }
}

impl fmt::Display for ReviewWeight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Light(Lighter::Docs) => f.write_str("light (docs only)"),
            Self::Light(Lighter::Pm { lines: 1 }) => f.write_str("light (PM, 1 line)"),
            Self::Light(Lighter::Pm { lines }) => write!(f, "light (PM, {lines} lines)"),
            Self::Full(heavier) => write!(f, "full ({heavier})"),
        }
    }
}

impl fmt::Display for Heavier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Adopted => f.write_str("adopted pull request"),
            Self::Empty => f.write_str("no changed file"),
            Self::Ci => f.write_str("CI file"),
            Self::Dependencies => f.write_str("dependency file"),
            Self::AgentFiles => f.write_str("agent files"),
            Self::FullPaths(file) => write!(f, "full_paths: {file}"),
            Self::High => f.write_str("stepped up: HIGH finding"),
            Self::Touched(file) => match file.split_once('/') {
                Some((folder, _)) => write!(f, "diff touched {folder}/"),
                None => write!(f, "diff touched {file}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(weight: ReviewWeight) -> String {
        weight.to_string()
    }

    #[test]
    fn each_weight_reads_as_status_will_show_it() {
        let full = ReviewWeight::Full;
        assert_eq!(
            shown(ReviewWeight::Light(Lighter::Docs)),
            "light (docs only)"
        );
        assert_eq!(shown(full(Heavier::Adopted)), "full (adopted pull request)");
        assert_eq!(shown(full(Heavier::Empty)), "full (no changed file)");
        assert_eq!(shown(full(Heavier::Ci)), "full (CI file)");
        assert_eq!(shown(full(Heavier::Dependencies)), "full (dependency file)");
        assert_eq!(shown(full(Heavier::AgentFiles)), "full (agent files)");
        let forced = full(Heavier::FullPaths("docs/adr/0001.md".into()));
        assert_eq!(shown(forced), "full (full_paths: docs/adr/0001.md)");
        let deep = full(Heavier::Touched("src/runner/mod.rs".into()));
        assert_eq!(shown(deep), "full (diff touched src/)");
        let root = full(Heavier::Touched("Makefile".into()));
        assert_eq!(shown(root), "full (diff touched Makefile)");
    }

    #[test]
    fn a_project_manager_light_weight_reads_with_its_line_count() {
        let pm = |lines| shown(ReviewWeight::Light(Lighter::Pm { lines }));
        assert_eq!(pm(6), "light (PM, 6 lines)");
        assert_eq!(pm(1), "light (PM, 1 line)");
        assert_eq!(pm(0), "light (PM, 0 lines)");
        assert!(ReviewWeight::Light(Lighter::Pm { lines: 6 }).is_light());
    }

    #[test]
    fn a_stepped_up_review_reads_full_with_its_reason() {
        let stepped = ReviewWeight::Full(Heavier::High);
        assert_eq!(shown(stepped), "full (stepped up: HIGH finding)");
    }

    #[test]
    fn only_a_light_weight_reads_as_light() {
        let light = ReviewWeight::Light(Lighter::Docs);
        let full = ReviewWeight::Full(Heavier::Empty);
        assert!(light.is_light() && !full.is_light());
    }
}
