//! What the batch retro reads: one section per finished work item, then the
//! batch's totals
//!
//! Built in code from the records kelpie keeps (the state file's history,
//! the journal, the answered rulings and the usage ledger), so the retro
//! spends its call reading and not collecting.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde::Serialize;

use super::gaps::Open;
use super::journal::{self, Entry, Stamped};
use crate::runner::ruling::{Answerer, Record};
use crate::state::Finished;
use crate::ticket::TicketId;
use crate::usage::{CallKind, CallLine, Ended, FinishedLine, Line, RoleLine};
use crate::work_kind::work_kind;

mod across;

/// The most wakes listed under one work item
const WAKES_LISTED: usize = 12;

/// The most findings listed under one work item
const FINDINGS_LISTED: usize = 20;

/// The number of rounds read at which a finding is called late
const LATE_ROUND: usize = 3;

/// Everything a digest is built from
pub(super) struct Records<'a> {
    pub project: &'a str,
    /// The work items finished since the last retro, oldest first
    pub batch: &'a [Finished],
    pub journal: &'a [Stamped],
    pub rulings: &'a [Record],
    pub ledger: &'a [Line],
    /// When the last retro started, or zero with none
    pub since: u64,
    /// The retro's own earlier findings still open, and what its limit left out
    pub gaps: &'a Open,
}

/// What a digest is for, as a line names it
fn shown(finished: &Finished) -> String {
    finished.issue.shown()
}

/// How far before its creation a work item's own window reaches
///
/// The wake that picked the item, and the dispatch it led to, sit seconds
/// before its first charge. The slack keeps them in the item's own section,
/// and stays far short of the gap between two runs of one issue.
const BEFORE_CREATION: u64 = 300;

// When the work item's own run began: its finish, less the wall time that
// runs from its creation, less [`BEFORE_CREATION`]
fn start_of(finished: &Finished) -> u64 {
    finished
        .at
        .0
        .saturating_sub(finished.wall.saturating_add(BEFORE_CREATION))
}

// The window the batch's totals cover: since the last retro, or with no
// earlier retro, from the batch's first work item's own start
fn window(records: &Records<'_>) -> (u64, u64) {
    let until = (records.batch.iter())
        .map(|f| f.at.0)
        .max()
        .unwrap_or_default();
    let first = (records.batch.iter())
        .map(start_of)
        .min()
        .unwrap_or_default();
    (records.since.max(first), until)
}

// Each work item's run in the batch, numbered where one issue ran twice
fn runs(batch: &[Finished]) -> Vec<(usize, usize)> {
    let mut total: BTreeMap<TicketId, usize> = BTreeMap::new();
    for finished in batch {
        *total.entry(finished.issue).or_default() += 1;
    }
    let mut seen: BTreeMap<TicketId, usize> = BTreeMap::new();
    batch
        .iter()
        .map(|finished| {
            let run = seen.entry(finished.issue).or_default();
            *run += 1;
            (*run, total[&finished.issue])
        })
        .collect()
}

/// What a set of calls cost, and what the ledger left unpriced or unread
#[derive(Debug, Clone, Default)]
struct Spend {
    calls: u32,
    units: u64,
    cost: f64,
    priced: u32,
    // Calls priced by their lease's energy, and what that cost
    electric: u32,
    electric_cost: f64,
    unpriced: u32,
    // The unpriced calls whose energy was metered, and their watt-hours
    metered: u32,
    watt_hours: f64,
    timed_out: u32,
}

impl Spend {
    fn add(&mut self, call: &CallLine) {
        self.calls += 1;
        self.units += call.units;
        if call.ended == Ended::TimedOut {
            // A turn ended at its ceiling read no usage, so what it cost is
            // unknown rather than nothing
            self.timed_out += 1;
        } else if call.cost_usd.is_some() && !call.unpriced {
            let cost = call.cost_usd.unwrap_or_default();
            // A harness that reported a session cost priced the call itself.
            match (call.energy_wh, call.session_cost_usd) {
                (Some(_), None) => {
                    self.electric += 1;
                    self.electric_cost += cost;
                }
                _ => {
                    self.priced += 1;
                    self.cost += cost;
                }
            }
        } else {
            self.unpriced += 1;
            if let Some(wh) = call.energy_wh {
                self.metered += 1;
                self.watt_hours += wh;
            }
        }
    }

    fn add_all(&mut self, other: &Self) {
        self.calls += other.calls;
        self.units += other.units;
        self.cost += other.cost;
        self.priced += other.priced;
        self.electric += other.electric;
        self.electric_cost += other.electric_cost;
        self.unpriced += other.unpriced;
        self.metered += other.metered;
        self.watt_hours += other.watt_hours;
        self.timed_out += other.timed_out;
    }

    // A role's own totals, from the finished line where the ledger names the
    // work item no call of it
    fn of_role(role: &RoleLine) -> Self {
        Self {
            calls: role.calls,
            units: role.units,
            cost: role.cost_usd,
            priced: role.calls.saturating_sub(role.unpriced_calls),
            unpriced: role.unpriced_calls,
            ..Self::default()
        }
    }

    // What the calls cost, naming those the ledger could not price
    fn note(&self) -> String {
        let mut parts = Vec::new();
        if self.priced > 0 {
            parts.push(format!("${:.2}", self.cost));
        }
        if self.electric > 0 {
            let electricity = format!("local, ${:.2} of electricity", self.electric_cost);
            parts.push(match self.electric == self.calls {
                true => electricity,
                false => format!("{} {electricity}", self.electric),
            });
        }
        if self.unpriced > 0 {
            let whole = match self.metered == self.unpriced {
                true => format!("local, {} Wh, unpriced", watt_hours(self.watt_hours)),
                false => "local, unpriced".to_owned(),
            };
            parts.push(each(self.unpriced, self.calls, &whole, "unpriced"));
        }
        if self.timed_out > 0 {
            parts.push(each(
                self.timed_out,
                self.calls,
                "timed out, usage not read",
                "timed out, usage not read",
            ));
        }
        match parts.is_empty() {
            true => "$0.00".to_owned(),
            false => parts.join("; "),
        }
    }
}

// Watt-hours to a tenth, with no tenth when it is a whole number
fn watt_hours(wh: f64) -> String {
    match (wh - wh.round()).abs() < 0.05 {
        true => format!("{wh:.0}"),
        false => format!("{wh:.1}"),
    }
}

// `{whole}`, where every one of the `of` calls is one, and `{n} {part}`
// where only `n` of them are
fn each(n: u32, of: u32, whole: &str, part: &str) -> String {
    match n == of {
        true => whole.to_owned(),
        false => format!("{n} {part}"),
    }
}

/// A span of seconds as `1h05m`, `5m10s` or `42s`
pub(super) fn span(seconds: u64) -> String {
    match (seconds / 3600, seconds / 60 % 60, seconds % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m{s:02}s"),
        (h, m, _) => format!("{h}h{m:02}m"),
    }
}

// A serialized name, as the ledger writes a role or a kind
fn name(value: &impl Serialize) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => "?".to_owned(),
    }
}

/// The digest of `records`, in markdown
pub(super) fn build(records: &Records<'_>) -> String {
    let issues: Vec<String> = records.batch.iter().map(shown).collect();
    let mut out = format!(
        "# Batch digest: {}, {} finished work items ({})\n",
        records.project,
        records.batch.len(),
        issues.join(", ")
    );
    for (finished, run) in records.batch.iter().zip(runs(records.batch)) {
        out.push('\n');
        item(&mut out, records, finished, run);
    }
    out.push('\n');
    across::section(&mut out, records);
    totals(&mut out, records);
    out
}

fn item(out: &mut String, records: &Records<'_>, finished: &Finished, run: (usize, usize)) {
    let work = work_kind(&finished.title);
    let revival = match run.1 > 1 {
        true => format!(", run {} of {}", run.0, run.1),
        false => String::new(),
    };
    let _ = writeln!(
        out,
        "## {}: {} ({work}){revival}",
        shown(finished),
        finished.title
    );
    let end = match (finished.merged, finished.closed) {
        (true, _) => "merged",
        (_, true) => "closed with no change",
        _ => "ended without a merge",
    };
    let pull_request = finished
        .pull_request
        .map_or_else(String::new, |n| format!(", pull request #{n}"));
    let _ = writeln!(
        out,
        "- Ended: {end}{pull_request}; wall time {}",
        span(finished.wall)
    );
    if let Some(line) = ledger_line(records.ledger, finished) {
        let spend = item_spend(records, finished, line);
        let _ = writeln!(
            out,
            "- Ledger: {} units, {}; worker {} calls {}; reviewer {} calls {}; \
             {} review rounds, {} fix turns, {} rulings",
            line.units,
            spend.total.note(),
            line.worker.calls,
            spend.worker.note(),
            line.reviewer.calls,
            spend.reviewer.note(),
            line.review_rounds,
            line.fix_turns,
            line.rulings
        );
    }
    let mine = mine(records, finished);
    wakes(out, &mine);
    rulings(out, records, finished);
    reviews(out, &mine);
    sent(out, &mine);
    fixes(out, &mine);
    for stamped in &mine {
        if let Entry::Friction { text, .. } = &stamped.entry {
            let _ = writeln!(out, "- Friction the worker reported: {}", one_line(text));
        }
    }
}

// The journal's entries about the work item, from its own run's start to its
// finish, so a revived issue keeps each run's entries to itself
fn mine<'a>(records: &Records<'a>, finished: &Finished) -> Vec<&'a Stamped> {
    let (from, until) = (start_of(finished), finished.at.0);
    (records.journal.iter())
        .filter(|s| s.at.0 >= from && s.at.0 <= until && s.entry.names(finished.issue))
        .collect()
}

// What the work item's own calls cost, by role, from the ledger's lines for
// them, or from its finished line where the ledger names it none
fn item_spend(records: &Records<'_>, finished: &Finished, line: &FinishedLine) -> ItemSpend {
    let (from, until) = (start_of(finished), finished.at.0);
    let mut worker = Spend::default();
    let mut reviewer = Spend::default();
    for call in records.ledger.iter().filter_map(|l| match l {
        Line::Call(c) => Some(c),
        Line::Finished(_) => None,
    }) {
        if call.issue != Some(finished.issue) || call.at.0 < from || call.at.0 > until {
            continue;
        }
        match name(&call.role).as_str() {
            "worker" => worker.add(call),
            "reviewer" => reviewer.add(call),
            _ => {}
        }
    }
    let worker = match worker.calls {
        0 => Spend::of_role(&line.worker),
        _ => worker,
    };
    let reviewer = match reviewer.calls {
        0 => Spend::of_role(&line.reviewer),
        _ => reviewer,
    };
    let mut total = worker.clone();
    total.add_all(&reviewer);
    ItemSpend {
        total,
        worker,
        reviewer,
    }
}

// What a work item's worker, reviewer and both together cost
struct ItemSpend {
    total: Spend,
    worker: Spend,
    reviewer: Spend,
}

// The item's line in the ledger: the last written at its finish
fn ledger_line<'a>(ledger: &'a [Line], finished: &Finished) -> Option<&'a FinishedLine> {
    ledger.iter().rev().find_map(|line| match line {
        Line::Finished(l) if l.issue == finished.issue && l.at == finished.at => Some(l),
        _ => None,
    })
}

fn wakes(out: &mut String, mine: &[&Stamped]) {
    let woke: Vec<&Stamped> = (mine.iter().copied())
        .filter(|s| matches!(s.entry, Entry::Wake { .. }))
        .collect();
    let _ = writeln!(out, "- Project manager wakes that named it: {}", woke.len());
    for stamped in woke.iter().take(WAKES_LISTED) {
        let Entry::Wake {
            woke_for, acted, ..
        } = &stamped.entry
        else {
            continue;
        };
        let did = match journal::changed_nothing(acted) {
            true => "changed nothing".to_owned(),
            false => acted.join("; "),
        };
        let _ = writeln!(
            out,
            "  - woke for {}: {did}",
            one_line(&woke_for.join("; "))
        );
    }
    if woke.len() > WAKES_LISTED {
        let _ = writeln!(out, "  - and {} more", woke.len() - WAKES_LISTED);
    }
}

fn rulings(out: &mut String, records: &Records<'_>, finished: &Finished) {
    let asked: Vec<&Record> = (records.rulings.iter())
        .filter(|r| r.issue == Some(finished.issue) && r.at <= finished.at)
        .collect();
    let _ = writeln!(out, "- Rulings: {}", asked.len());
    for ruling in asked {
        let kind = ruling.kind.map_or("?", |k| k.as_str());
        let waited = (ruling.raised).map_or_else(String::new, |raised| {
            format!(", waited {}", span(ruling.at.0.saturating_sub(raised.0)))
        });
        let by = match ruling.by() {
            Answerer::Maintainer => "maintainer",
            Answerer::Pm => "project manager",
        };
        let _ = writeln!(
            out,
            "  - ruling {} ({kind}){waited}; answered by the {by}: {}",
            ruling.id,
            one_line(&ruling.answer)
        );
    }
}

fn rounds_read(mine: &[&Stamped]) -> impl Iterator<Item = u32> {
    mine.iter().filter_map(|s| match s.entry {
        Entry::Round { round, .. } => Some(round),
        _ => None,
    })
}

fn reviews(out: &mut String, mine: &[&Stamped]) {
    let rounds: Vec<&Entry> = (mine.iter())
        .map(|s| &s.entry)
        .filter(|e| matches!(e, Entry::Round { .. } | Entry::ReviewerSkipped { .. }))
        .collect();
    let read = rounds_read(mine).count();
    let passed_over = rounds.len() - read;
    let latest = (rounds.iter())
        .filter_map(|e| match e {
            Entry::Round { round, .. } | Entry::ReviewerSkipped { round, .. } => Some(*round),
            _ => None,
        })
        .max();
    let latest = latest.map_or(String::new(), |n| format!(", the latest numbered {n}"));
    let _ = writeln!(
        out,
        "- Review rounds: {read} read, {passed_over} passed over{latest}"
    );
    for entry in rounds {
        match entry {
            Entry::Round {
                issue,
                round,
                reviewer,
                findings,
            } => {
                let sent = mine.iter().find_map(|s| match &s.entry {
                    Entry::FindingsSent { round: r, held, .. } if r == round => Some(*held),
                    _ => None,
                });
                // A round that found nothing sent nothing, which its own line
                // already says
                let sent = match (*findings, sent) {
                    (_, None) | (0, Some(0)) => String::new(),
                    (_, Some(held)) => format!(", {held} sent to the worker"),
                };
                let _ = writeln!(
                    out,
                    "  - {} round {round} by {reviewer}: {findings} findings{sent}",
                    issue.shown()
                );
            }
            Entry::ReviewerSkipped {
                issue,
                round,
                reviewer,
                reason,
            } => {
                let _ = writeln!(
                    out,
                    "  - {} round {round} by {reviewer}: passed over, {}",
                    issue.shown(),
                    one_line(reason)
                );
            }
            _ => {}
        }
    }
}

// Each finding sent to the worker, numbered by rounds read; a late finding is
// marked, since a defect found that late was missed in the reads before, and
// so is one an earlier round already sent, since its first fix did not fix it
fn sent(out: &mut String, mine: &[&Stamped]) {
    let mut lines = Vec::new();
    for (index, stamped) in mine.iter().enumerate() {
        let Entry::Findings {
            round, findings, ..
        } = &stamped.entry
        else {
            continue;
        };
        let preceding = &mine[..index];
        let reviewer = preceding.iter().rev().find_map(|s| match &s.entry {
            Entry::Round {
                round: r, reviewer, ..
            } if r == round => Some(reviewer.as_str()),
            _ => None,
        });
        let round = rounds_read(preceding).count();
        let late = if round >= LATE_ROUND { " (late)" } else { "" };
        for found in findings {
            let line = match found.line {
                0 => String::new(),
                n => format!(":{n}"),
            };
            let again = if found.again { " (again)" } else { "" };
            lines.push(format!(
                "  - round {round}{late}{again} by {}, {}, {}{line}: {}",
                reviewer.unwrap_or("?"),
                name(&found.severity),
                found.file,
                one_line(&found.what)
            ));
        }
    }
    if lines.is_empty() {
        return;
    }
    let _ = writeln!(out, "- Findings sent to the worker: {}", lines.len());
    for line in lines.iter().take(FINDINGS_LISTED) {
        let _ = writeln!(out, "{line}");
    }
    if lines.len() > FINDINGS_LISTED {
        let _ = writeln!(out, "  - and {} more", lines.len() - FINDINGS_LISTED);
    }
}

fn fixes(out: &mut String, mine: &[&Stamped]) {
    let mut ci = Vec::new();
    let (mut rebased, mut conflicts) = (0, Vec::new());
    for stamped in mine {
        match &stamped.entry {
            Entry::CiFailed { checks, .. } => ci.push(checks.join(", ")),
            Entry::Rebased { .. } => rebased += 1,
            Entry::Conflicted { files, .. } => conflicts.push(files.join(", ")),
            _ => {}
        }
    }
    let _ = writeln!(out, "- Fix turns after a failed check: {}", ci.len());
    for checks in ci {
        let _ = writeln!(out, "  - failed: {checks}");
    }
    let _ = writeln!(
        out,
        "- Merges of main: {rebased} rebased, {} conflicted",
        conflicts.len()
    );
    for files in conflicts {
        let _ = writeln!(out, "  - conflict in {files}");
    }
}

fn totals(out: &mut String, records: &Records<'_>) {
    let _ = writeln!(out, "## Batch totals");
    let batch: Vec<TicketId> = records.batch.iter().map(|f| f.issue).collect();
    let (from, until) = window(records);
    // The batch's own calls, and the project manager's over the batch's own
    // span; the retro's calls are not the batch's.
    let calls: Vec<&CallLine> = (records.ledger.iter())
        .filter_map(|line| match line {
            Line::Call(call) => Some(call),
            Line::Finished(_) => None,
        })
        .filter(|call| call.kind != CallKind::Retro)
        .filter(|call| match call.issue {
            Some(issue) => batch.contains(&issue),
            None => call.at.0 > from && call.at.0 <= until,
        })
        .collect();
    let mut spend: BTreeMap<(String, String), Spend> = BTreeMap::new();
    for call in &calls {
        spend
            .entry((name(&call.role), name(&call.kind)))
            .or_default()
            .add(call);
    }
    let _ = writeln!(out, "- Spend by role and kind:");
    for ((role, kind), group) in &spend {
        let _ = writeln!(
            out,
            "  - {role} {kind}: {} calls, {} units, {}",
            group.calls,
            group.units,
            group.note()
        );
    }
    let wakes: Vec<&Stamped> = (records.journal.iter())
        .filter(|s| s.at.0 > from && s.at.0 <= until)
        .filter(|s| matches!(s.entry, Entry::Wake { .. }))
        .collect();
    let idle = (wakes.iter())
        .filter(
            |s| matches!(&s.entry, Entry::Wake { acted, .. } if journal::changed_nothing(acted)),
        )
        .count();
    let _ = writeln!(
        out,
        "- Project manager wakes: {}, of which {idle} changed nothing",
        wakes.len()
    );
    let waits: Vec<u64> = (records.rulings.iter())
        .filter(|r| r.issue.is_some_and(|ticket| batch.contains(&ticket)))
        .filter_map(|r| r.raised.map(|raised| r.at.0.saturating_sub(raised.0)))
        .collect();
    let asked = (records.rulings.iter())
        .filter(|r| r.issue.is_some_and(|ticket| batch.contains(&ticket)))
        .count();
    let _ = writeln!(
        out,
        "- Rulings: {asked}, waiting {} in all and {} at the longest",
        span(waits.iter().sum()),
        span(waits.iter().copied().max().unwrap_or_default())
    );
}

// Whitespace folded, so a multi-line text sits on one line
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests;
