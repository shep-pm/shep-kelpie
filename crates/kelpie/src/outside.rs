//! The outside reviewers kelpie summons: CodeRabbit and Gemini Code Assist
//!
//! Both run the same round between green CI and the merge ruling, under a
//! lease on their own review window. Gemini's round comes first, so the
//! scarce CodeRabbit review sees code Gemini's findings already cleaned up.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::lease::LeaseKind;
use crate::lease::window::Terms;
use crate::ports::Timestamp;

/// An outside reviewer
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outside {
    /// Gemini Code Assist, summoned by a `/gemini review` comment
    Gemini,
    /// CodeRabbit, summoned by the `review please` label. The default, since
    /// a state file written before Gemini names no reviewer.
    #[default]
    #[serde(rename = "coderabbit")]
    CodeRabbit,
}

impl Outside {
    /// Every outside reviewer, in the order their rounds run
    pub const ALL: [Self; 2] = [Self::Gemini, Self::CodeRabbit];

    /// The lease on its review window
    pub fn lease_kind(self) -> LeaseKind {
        match self {
            Self::Gemini => LeaseKind::gemini(),
            Self::CodeRabbit => LeaseKind::coderabbit(),
        }
    }

    /// Its review window's terms, until a review says otherwise
    pub fn terms(self) -> Terms {
        match self {
            Self::Gemini => Terms::GEMINI,
            Self::CodeRabbit => Terms::CODERABBIT,
        }
    }
}

impl fmt::Display for Outside {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gemini => "Gemini",
            Self::CodeRabbit => "CodeRabbit",
        })
    }
}

/// One of a reviewer's conversation comments, as last edited
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Its text
    pub body: String,
    /// When it was last edited: CodeRabbit's walkthrough is edited in place
    pub at: Timestamp,
}

/// A review thread a reviewer opened
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// The forge's id for it, which resolving it takes
    pub id: String,
    /// Whether it is resolved
    pub resolved: bool,
    /// The file it is on
    pub path: String,
    /// The line it is on, if it still maps to one
    pub line: Option<u32>,
    /// Its first comment: the finding
    pub body: String,
    /// The forge's id for the review its first comment is part of
    pub review: Option<u64>,
}

/// What became of a summon, read at one moment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    /// A review covers the head
    Reviewed,
    /// A review is running
    Processing,
    /// The reviewer refused, quoting when its window opens
    Refused {
        /// When the window opens
        opens: Timestamp,
    },
    /// Nothing yet
    Silent,
}

/// A reviewer's answer stamped this long before the summon still answers
/// it: GitHub's clock is not this machine's
pub const CLOCK_SLACK: u64 = 60;

// A findings file holds one finding a line, fields split by `|`.
pub(crate) fn one_line(text: &str) -> String {
    const MOST: usize = 600;
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat.replace('|', "/");
    match flat.char_indices().nth(MOST) {
        Some((cut, _)) => format!("{}...", &flat[..cut]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reviewer_is_named_as_the_state_file_and_the_lease_name_it() {
        for (reviewer, name) in [
            (Outside::Gemini, "gemini"),
            (Outside::CodeRabbit, "coderabbit"),
        ] {
            assert_eq!(serde_json::to_value(reviewer).unwrap(), name);
            assert_eq!(reviewer.lease_kind().as_str(), name);
        }
    }

    #[test]
    fn a_long_finding_is_cut_to_one_line() {
        let long = "word ".repeat(200);
        let cut = one_line(&long);
        assert_eq!(cut.chars().count(), 603);
        assert!(cut.ends_with("..."));
        assert_eq!(one_line("a |b\n c"), "a /b c");
    }
}
