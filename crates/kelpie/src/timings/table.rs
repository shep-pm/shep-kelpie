//! The `timings` trigger's answer: the last finished work items and their totals

use std::fmt::Write as _;

use serde::Serialize;

use super::{Bucket, Finished, Split};

/// One finished work item, as the table shows it
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    /// The work item's issue
    pub issue: u64,
    /// Its pull request, if it had one
    pub pull_request: Option<u64>,
    /// Whether the pull request merged
    pub merged: bool,
    /// Seconds from its first save to its finish, which its split adds up to
    pub wall: u64,
    /// Where they went
    pub split: Split,
}

/// The split over the last finished work items
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// How many finished work items it covers
    pub items: usize,
    /// Each one, newest first
    pub rows: Vec<Row>,
    /// Their wall times and splits added together
    pub total: Row,
    /// The same, as a table to read
    pub table: String,
}

impl Report {
    /// The report over the last `last` of `history`, which runs oldest first
    pub fn of(history: &[Finished], last: usize) -> Self {
        let rows: Vec<Row> = history
            .iter()
            .rev()
            .take(last)
            .map(|f| Row {
                issue: f.issue,
                pull_request: f.pull_request,
                merged: f.merged,
                wall: f.wall(),
                split: f.split,
            })
            .collect();
        let total = Row {
            issue: 0,
            pull_request: None,
            merged: false,
            wall: rows.iter().map(|r| r.wall).sum(),
            split: rows
                .iter()
                .fold(Split::default(), |sum, r| sum.plus(&r.split)),
        };
        let table = render(&rows, &total);
        Self {
            items: rows.len(),
            rows,
            total,
            table,
        }
    }
}

const HEADS: [(Bucket, &str); 11] = [
    (Bucket::Worker, "worker"),
    (Bucket::GpuWait, "gpu wait"),
    (Bucket::LocalRound, "local"),
    (Bucket::ClaudeRound, "claude"),
    (Bucket::Judging, "judge"),
    (Bucket::Ci, "ci"),
    (Bucket::CodeRabbitWindow, "cr window"),
    (Bucket::CodeRabbitReview, "cr review"),
    (Bucket::Ruling, "ruling"),
    (Bucket::Merge, "merge"),
    (Bucket::Idle, "idle"),
];

// One line a work item, then the total and each bucket's share of it, every
// column as wide as its widest cell.
fn render(rows: &[Row], total: &Row) -> String {
    let mut lines: Vec<Vec<String>> = Vec::new();
    let mut head = vec!["item".to_owned(), "wall".to_owned()];
    head.extend(HEADS.iter().map(|(_, name)| (*name).to_owned()));
    lines.push(head);
    for row in rows {
        let name = match row.merged {
            true => format!("#{}", row.issue),
            false => format!("#{} (not merged)", row.issue),
        };
        lines.push(cells(name, row.wall, &row.split, duration));
    }
    lines.push(cells(
        "total".to_owned(),
        total.wall,
        &total.split,
        duration,
    ));
    lines.push(cells("share".to_owned(), total.wall, &total.split, |s| {
        share(s, total.wall)
    }));
    let widths: Vec<usize> = (0..lines[0].len())
        .map(|i| {
            lines
                .iter()
                .map(|l| l[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for line in &lines {
        for (i, cell) in line.iter().enumerate() {
            let sep = if i == 0 { "" } else { "  " };
            let width = widths[i];
            let _ = if i == 0 {
                write!(out, "{sep}{cell:<width$}")
            } else {
                write!(out, "{sep}{cell:>width$}")
            };
        }
        out.push('\n');
    }
    out
}

fn cells(name: String, wall: u64, split: &Split, show: impl Fn(u64) -> String) -> Vec<String> {
    let mut line = vec![name, show(wall)];
    line.extend(HEADS.iter().map(|(bucket, _)| show(split.get(*bucket))));
    line
}

fn duration(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

fn share(seconds: u64, of: u64) -> String {
    if of == 0 {
        return "-".to_owned();
    }
    format!("{}%", (seconds * 100 + of / 2) / of)
}
