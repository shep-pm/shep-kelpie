//! How much review a work item's change gets, which `reviews.light` sets

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::agents::Tier;

/// How much review a work item's pull request gets: the light count of
/// reviewers, or the full `reviews.need`
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewWeight {
    /// `reviews.light` reviewers read it
    Light(Lighter),
    /// `reviews.need` reviewers read it
    Full(Heavier),
}

/// Why a change gets the light count
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lighter {
    /// Every changed file matches `reviews.light_paths`
    Docs,
    /// The project manager tagged it light and it changes this many lines,
    /// within `reviews.light_max_lines`
    Pm {
        /// The lines it adds plus the lines it deletes
        lines: u32,
    },
}

/// Why a change gets the full count
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
    /// It changes this file, which `reviews.full_paths` names
    FullPaths(String),
    /// It changes this file, the first outside `reviews.light_paths`
    Touched(String),
    /// A reviewer's HIGH finding was sent to the worker
    High,
}

impl ReviewWeight {
    /// Whether it gets the light count
    pub fn is_light(&self) -> bool {
        matches!(self, Self::Light(_))
    }

    /// The tier the agent board's pick for the review asks for: `light` for a
    /// light change, and `own`, the work item's, otherwise
    pub fn tier(&self, own: Option<Tier>) -> Option<Tier> {
        match self.is_light() {
            true => Some(Tier::Light),
            false => own,
        }
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
    fn only_a_light_weight_lowers_the_tier_to_light() {
        let light = ReviewWeight::Light(Lighter::Docs);
        let full = ReviewWeight::Full(Heavier::Empty);
        assert!(light.is_light() && !full.is_light());
        assert_eq!(light.tier(Some(Tier::Heavy)), Some(Tier::Light));
        assert_eq!(light.tier(None), Some(Tier::Light));
        assert_eq!(full.tier(Some(Tier::Heavy)), Some(Tier::Heavy));
        assert_eq!(full.tier(None), None);
    }
}
