//! The package installs of a project's work items that failed
//!
//! A failed install does not stop its work item, and `status` shows it only while the runner is
//! up. The runner logs each install as a `packages-installed` step in `steps.jsonl`, so this
//! reads it from there.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader};

use serde::Deserialize;

use super::Line;
use crate::home::{ProjectPaths, STEPS_FILE};

/// How many of the file's last lines are read
const LAST_LINES: usize = 200;

#[derive(Deserialize)]
struct Step {
    step: String,
    #[serde(default)]
    issue: String,
    #[serde(default)]
    failed: Option<String>,
}

/// A line for each work item whose latest install in the last 200 steps failed, in the order
/// of its issue's number as text: none where the file is missing or holds no such step
pub(super) fn lines(subject: &dyn Fn(&str) -> String, paths: &ProjectPaths) -> Vec<Line> {
    let Ok(file) = std::fs::File::open(paths.folder.join(STEPS_FILE)) else {
        return Vec::new();
    };
    let mut last: VecDeque<String> = VecDeque::with_capacity(LAST_LINES);
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if last.len() == LAST_LINES {
            last.pop_front();
        }
        last.push_back(line);
    }
    let mut latest: BTreeMap<String, Option<String>> = BTreeMap::new();
    for step in last
        .iter()
        .filter_map(|line| serde_json::from_str::<Step>(line).ok())
    {
        if step.step == "packages-installed" {
            latest.insert(step.issue, step.failed);
        }
    }
    latest
        .into_iter()
        .filter_map(|(issue, failed)| Some((issue, failed?)))
        .map(|(issue, output)| {
            Line::unsure(
                subject(&format!("package install #{issue}")),
                format!(
                    "the install of its packages failed before its turn: {}",
                    error_line(&output)
                ),
                "its worker went on without them; read the cause, then fix the lockfile or \
                 the registry the install needs",
            )
        })
        .collect()
}

// The first line that names an error, else the last line
fn error_line(output: &str) -> &str {
    let lines = || output.lines().map(str::trim).filter(|l| !l.is_empty());
    lines()
        .find(|l| l.to_ascii_lowercase().contains("error"))
        .or_else(|| lines().next_back())
        .unwrap_or_default()
}
