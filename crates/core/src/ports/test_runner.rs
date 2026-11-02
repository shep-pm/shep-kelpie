//! The test runner port: one command of the project's, run in a worktree
//!
//! A reviewer's attached test is run twice, at the head it read and at the
//! fix's head. The command is the project's own, so it runs under the worker's
//! fence and never with the maintainer's reach.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::Reach;

/// How much of a failed run's output is kept: its last 2 KiB
pub const OUTPUT_KEPT: usize = 2048;

/// How long one run may take before it is killed, the wait for the
/// `cargo-test` lease apart
pub const CEILING: Duration = Duration::from_secs(10 * 60);

/// One command to run
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRun {
    /// The command, as a shell reads it, with the test's path and name filled in
    pub command: String,
    /// The folder it runs in: the work item's worktree, or its build folder for the install
    pub cwd: PathBuf,
    /// What it may reach: the worker's own fence
    pub reach: Reach,
    /// Where the sandbox's settings for the run are written, a file the run cannot write
    pub settings: PathBuf,
    /// The dog's door, where the run takes its place in the `cargo-test` queue, or none for a
    /// run that holds no lease
    pub door: Option<PathBuf>,
    /// How long it may run once it holds the lease
    pub ceiling: Duration,
}

/// How a run ended
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TestOutcome {
    /// The command exited 0
    Passed,
    /// It exited with anything else
    Failed {
        /// The last [`OUTPUT_KEPT`] bytes of what it wrote
        output: String,
    },
    /// It ran past its ceiling and was killed
    TimedOut,
    /// It never ran to a result: the lease, the sandbox or the command itself
    /// could not start
    NotRun {
        /// Why
        reason: String,
    },
}

impl TestOutcome {
    /// Whether the run reached a result, a pass or a failure
    pub fn ran(&self) -> bool {
        matches!(self, Self::Passed | Self::Failed { .. })
    }
}

/// Runs a project's test command
pub trait TestRunner: Send + Sync {
    /// Runs `run` to its end, or to its ceiling
    ///
    /// Never fails: a run that could not happen is [`TestOutcome::NotRun`].
    fn run(&self, run: &TestRun) -> TestOutcome;
}

/// The last [`OUTPUT_KEPT`] bytes of `output`, cut at a character
pub fn tail(output: &[u8]) -> String {
    let text = String::from_utf8_lossy(output);
    let mut from = text.len().saturating_sub(OUTPUT_KEPT);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    text[from..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_output_keeps_its_last_two_kib_whole_characters_only() {
        let long = format!("{}{}", "é".repeat(2000), "end");
        let kept = tail(long.as_bytes());
        assert!(kept.len() <= OUTPUT_KEPT, "{}", kept.len());
        assert!(kept.ends_with("éend"), "{kept}");
        assert_eq!(tail(b"short"), "short");
    }

    #[test]
    fn an_outcome_is_pinned_on_the_wire() {
        let wire = |o: &TestOutcome| serde_json::to_value(o).unwrap();
        assert_eq!(
            wire(&TestOutcome::Passed),
            serde_json::json!({ "result": "passed" })
        );
        assert_eq!(
            wire(&TestOutcome::Failed {
                output: "no".into()
            }),
            serde_json::json!({ "result": "failed", "output": "no" })
        );
        assert_eq!(
            wire(&TestOutcome::TimedOut),
            serde_json::json!({ "result": "timed-out" })
        );
        assert_eq!(
            wire(&TestOutcome::NotRun { reason: "x".into() }),
            serde_json::json!({ "result": "not-run", "reason": "x" })
        );
    }
}
