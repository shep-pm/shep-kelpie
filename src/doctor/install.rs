//! The package installs of a project's work items that failed
//!
//! A failed install does not stop its work item, and `status` shows it only while the runner is
//! up. The runner logs each install as a `packages-installed` step in `steps.jsonl`, so this
//! reads it from there.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use serde::Deserialize;

use super::Line;
use crate::home::{ProjectPaths, STEPS_FILE};

/// How many of the file's last lines are read
const LAST_LINES: usize = 200;

/// How many bytes are read back from the end at a time
const CHUNK: u64 = 64 * 1024;

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
    let Ok(mut file) = std::fs::File::open(paths.folder.join(STEPS_FILE)) else {
        return Vec::new();
    };
    let last = last_lines(&mut file);
    let mut latest: BTreeMap<String, Option<String>> = BTreeMap::new();
    for step in last
        .iter()
        .filter_map(|line| serde_json::from_slice::<Step>(line).ok())
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

// The last `LAST_LINES` lines of `file`, read backward from its end so a long history costs
// only the bytes of those lines. A file that cannot be read gives none.
fn last_lines(file: &mut File) -> Vec<Vec<u8>> {
    let Ok(mut start) = file.seek(SeekFrom::End(0)) else {
        return Vec::new();
    };
    let mut bytes: Vec<u8> = Vec::new();
    // One newline more than the lines wanted proves the first line read is whole.
    while start > 0 && bytes.iter().filter(|b| **b == b'\n').count() <= LAST_LINES {
        let take = start.min(CHUNK);
        start -= take;
        let mut chunk = vec![0; take as usize];
        if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut chunk).is_err() {
            return Vec::new();
        }
        chunk.extend_from_slice(&bytes);
        bytes = chunk;
    }
    let mut lines: Vec<Vec<u8>> = bytes.split(|b| *b == b'\n').map(<[u8]>::to_vec).collect();
    // The text after the last newline is empty, or a line still being written.
    lines.pop();
    if start > 0 {
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(LAST_LINES);
    lines.split_off(skip)
}
