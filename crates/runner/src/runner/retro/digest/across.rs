//! What only shows across the batch's work items: the files they conflict
//! on, their review rounds, the flaky tests and the known gaps that came back

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use super::{Records, mine, rounds_read};
use crate::runner::retro::flakes;
use crate::runner::retro::journal::{Entry, Stamped};
use crate::ticket::TicketId;

/// The section, with a line for each of the four
pub(super) fn section(out: &mut String, records: &Records<'_>) {
    let items: Vec<(TicketId, Vec<&Stamped>)> = (records.batch.iter())
        .map(|f| (f.issue, mine(records, f)))
        .collect();
    let _ = writeln!(out, "## Across the batch");
    conflicts(out, &items);
    rounds(out, &items);
    flaky(out, &items);
    gaps(out, records, &items);
    out.push('\n');
}

// How often each file conflicted, and on which work items, most first
fn conflicts(out: &mut String, items: &[(TicketId, Vec<&Stamped>)]) {
    let mut files: BTreeMap<&str, (usize, BTreeSet<TicketId>)> = BTreeMap::new();
    for (issue, mine) in items {
        for stamped in mine {
            if let Entry::Conflicted { files: named, .. } = &stamped.entry {
                let unique: BTreeSet<&str> = named.iter().map(String::as_str).collect();
                for file in unique {
                    let seen = files.entry(file).or_default();
                    seen.0 += 1;
                    seen.1.insert(*issue);
                }
            }
        }
    }
    if files.is_empty() {
        let _ = writeln!(out, "- Merge conflicts per file: none");
        return;
    }
    let mut ranked: Vec<_> = files.into_iter().collect();
    ranked.sort_by_key(|(_, (count, _))| std::cmp::Reverse(*count));
    let _ = writeln!(out, "- Merge conflicts per file, most first:");
    for (file, (count, issues)) in ranked {
        let _ = writeln!(
            out,
            "  - {file}: {count} conflicts on {}",
            issue_list(&issues)
        );
    }
}

fn rounds(out: &mut String, items: &[(TicketId, Vec<&Stamped>)]) {
    let _ = writeln!(out, "- Review rounds per work item:");
    for (issue, mine) in items {
        let read = rounds_read(mine).count();
        let passed_over = (mine.iter())
            .filter(|s| matches!(s.entry, Entry::ReviewerSkipped { .. }))
            .count();
        let _ = writeln!(
            out,
            "  - {}: {read} read, {passed_over} passed over",
            issue.shown()
        );
    }
}

fn flaky(out: &mut String, items: &[(TicketId, Vec<&Stamped>)]) {
    let entries = items.iter().flat_map(|(_, m)| m.iter().map(|s| &s.entry));
    let known = flakes::known(entries);
    if known.is_empty() {
        let _ = writeln!(out, "- Known flakes: none");
        return;
    }
    let _ = writeln!(out, "- Known flakes:");
    for flake in known {
        let issues: BTreeSet<TicketId> = flake.issues.into_iter().collect();
        let _ = writeln!(out, "  - {}: {}", flake.test, issue_list(&issues));
    }
}

// Each open known gap, with the work items whose friction or wakes name it
fn gaps(out: &mut String, records: &Records<'_>, items: &[(TicketId, Vec<&Stamped>)]) {
    if records.gaps.is_empty() {
        return;
    }
    let texts: Vec<(TicketId, String)> = (items.iter())
        .map(|(issue, mine)| (*issue, text_of(mine).to_lowercase()))
        .collect();
    let mut lines = String::new();
    for gap in records.gaps {
        let gap_lower = gap.to_lowercase();
        let named: BTreeSet<TicketId> = (texts.iter())
            .filter(|(_, text)| text.contains(&gap_lower))
            .map(|(issue, _)| *issue)
            .collect();
        if !named.is_empty() {
            let _ = writeln!(lines, "  - {gap}: {}", issue_list(&named));
        }
    }
    match lines.is_empty() {
        true => {
            let _ = writeln!(out, "- Known gaps named again in this batch: none");
        }
        false => {
            let _ = writeln!(out, "- Known gaps named again in this batch:");
            out.push_str(&lines);
        }
    }
}

// What the work item's friction and wakes say, in one string
fn text_of(mine: &[&Stamped]) -> String {
    let mut text = String::new();
    for stamped in mine {
        match &stamped.entry {
            Entry::Friction { text: said, .. } => text.push_str(said),
            Entry::Wake {
                woke_for, acted, ..
            } => text.push_str(
                &woke_for
                    .iter()
                    .chain(acted)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => continue,
        }
        text.push('\n');
    }
    // A line break inside a sentence must not hide a theme from the match.
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn issue_list(issues: &BTreeSet<TicketId>) -> String {
    let named: Vec<String> = issues.iter().map(TicketId::shown).collect();
    named.join(", ")
}
