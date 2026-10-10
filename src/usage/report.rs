//! What `shep kelpie usage` prints: per merged pull request, its units,
//! dollars, wall time, rulings and each role's share, with their medians,
//! the loaded units per merged pull request that a baseline is held
//! against, then the project manager's and the issue writer's totals

use std::collections::{BTreeMap, BTreeSet};

use super::baselines::Baseline;
use super::{CallKind, CallLine, FinishedLine, Line};
use crate::ports::{Role, Timestamp};

/// The plain-text report on `project`'s ledger `lines`, from `since` on
/// when given, with each of `baselines` held against the loaded figure and
/// the median
pub fn report(
    project: &str,
    lines: &[Line],
    since: Option<Timestamp>,
    baselines: &[Baseline],
) -> Vec<String> {
    let after = |at: Timestamp| since.is_none_or(|since| at >= since);
    let finished: Vec<&FinishedLine> = (lines.iter())
        .filter_map(|line| match line {
            Line::Finished(item) => Some(item),
            Line::Call(_) => None,
        })
        .collect();
    let calls: Vec<&CallLine> = (lines.iter())
        .filter_map(|line| match line {
            Line::Call(call) if after(call.at) => Some(call),
            _ => None,
        })
        .collect();
    let merged: Vec<&FinishedLine> = (finished.iter().copied())
        .filter(|item| item.merged && item.pull_request.is_some() && after(item.at))
        .collect();
    let mut out = vec![match merged.len() {
        1 => format!("{project}: 1 merged pull request"),
        n => format!("{project}: {n} merged pull requests"),
    }];
    if !merged.is_empty() {
        out.push(row([
            "pr", "issue", "units", "dollars", "unpriced", "wall", "rulings", "worker", "reviewer",
        ]));
        for item in &merged {
            out.push(item_row(item));
        }
        out.push(medians(&merged));
        out.push("the median is of merged items' own calls".to_owned());
        let unpriced: Vec<u32> = merged.iter().map(|item| unpriced_of(item)).collect();
        let total: u32 = unpriced.iter().sum();
        if total > 0 {
            let items = unpriced.iter().filter(|&&n| n > 0).count();
            out.push(format!(
                "dollars count priced calls only: {} of {} report no cost, the median's \
                 dollars included",
                count(total as usize, "call"),
                count(items, "merged item")
            ));
        }
        // Every call in the window over the merges, as a baseline's total over its window is.
        let count = merged.len() as f64;
        let loaded = calls.iter().map(|call| call.units).sum::<u64>() as f64 / count;
        let dollars = calls.iter().filter_map(|call| call.cost_usd).sum::<f64>() / count;
        let unpriced = calls.iter().filter(|call| call.unpriced).count();
        out.push(format!(
            "loaded: {} units, {}{} per merged pull request, counting every call: the \
             project manager's, the issue writer's, and those of work items dropped or \
             closed with no change too",
            thousands(loaded.round() as u64),
            money(dollars),
            unpriced_note(unpriced)
        ));
        let median = median(merged.iter().map(|i| i.units as f64).collect()).unwrap_or(0.0);
        out.extend(baselines.iter().map(|b| against(b, loaded, median)));
    }
    let ended = |closed: bool| -> Vec<&FinishedLine> {
        (finished.iter().copied())
            .filter(|item| !item.merged && item.closed == closed && after(item.at))
            .collect()
    };
    out.extend(not_merged("closed with no change", &ended(true)));
    out.extend(not_merged("dropped", &ended(false)));
    out.push(String::new());
    out.push(totals("project manager", &calls, Role::Pm));
    out.push(totals("issue writer", &calls, Role::IssueWriter));
    let recorded: BTreeSet<u64> = finished.iter().map(|item| item.issue).collect();
    out.extend(unrecorded(&calls, &recorded));
    out
}

// What the work items that ended unmerged as `what` spent, if any did
fn not_merged(what: &str, items: &[&FinishedLine]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let units = items.iter().map(|item| item.units).sum();
    let dollars = items.iter().map(|item| item.cost_usd).sum();
    let unpriced: u32 = items.iter().map(|item| unpriced_of(item)).sum();
    Some(format!(
        "{what}: {}, {} units, {}{}",
        count(items.len(), "work item"),
        thousands(units),
        money(dollars),
        unpriced_note(unpriced as usize)
    ))
}

fn row(cells: [&str; 9]) -> String {
    let [
        pr,
        issue,
        units,
        dollars,
        unpriced,
        wall,
        rulings,
        worker,
        reviewer,
    ] = cells;
    format!(
        "{pr:<8}{issue:<8}{units:>13}{dollars:>10}{unpriced:>10}{wall:>11}{rulings:>9}\
         {worker:>8}{reviewer:>10}"
    )
}

// The calls of `item` whose harness reported no cost
fn unpriced_of(item: &FinishedLine) -> u32 {
    (item.worker.unpriced_calls).saturating_add(item.reviewer.unpriced_calls)
}

fn item_row(item: &FinishedLine) -> String {
    let pr = item
        .pull_request
        .map_or_else(String::new, |n| format!("#{n}"));
    let (worker, reviewer) = shares(item);
    row([
        &pr,
        &format!("#{}", item.issue),
        &thousands(item.units),
        &money(item.cost_usd),
        &unpriced_of(item).to_string(),
        &duration(item.wall),
        &item.rulings.to_string(),
        &percent(worker),
        &percent(reviewer),
    ])
}

// The worker's and the reviewers' shares of the units, or none with no units
fn shares(item: &FinishedLine) -> (Option<f64>, Option<f64>) {
    let total = item.worker.units.saturating_add(item.reviewer.units);
    let share = |units: u64| (total > 0).then(|| units as f64 / total as f64);
    (share(item.worker.units), share(item.reviewer.units))
}

fn medians(items: &[&FinishedLine]) -> String {
    let of = |values: Vec<f64>| median(values);
    let units = of(items.iter().map(|i| i.units as f64).collect());
    let dollars = of(items.iter().map(|i| i.cost_usd).collect());
    let unpriced = of(items.iter().map(|i| f64::from(unpriced_of(i))).collect());
    let wall = of(items.iter().map(|i| i.wall as f64).collect());
    let rulings = of(items.iter().map(|i| f64::from(i.rulings)).collect());
    let worker = of(items.iter().filter_map(|i| shares(i).0).collect());
    let reviewer = of(items.iter().filter_map(|i| shares(i).1).collect());
    let whole = |v: Option<f64>| v.map_or(0, |v| v.round() as u64);
    row([
        "median",
        "",
        &thousands(whole(units)),
        &money(dollars.unwrap_or(0.0)),
        &unpriced.map_or_else(String::new, |n| format!("{n}")),
        &duration(whole(wall)),
        &rulings.map_or_else(String::new, |r| format!("{r}")),
        &percent(worker),
        &percent(reviewer),
    ])
}

// `baseline`'s units per merged pull request, and kelpie's `loaded` and
// `median` units over it
fn against(baseline: &Baseline, loaded: f64, median: f64) -> String {
    let name = match &baseline.window {
        Some(window) => format!("{} ({window})", baseline.name),
        None => baseline.name.clone(),
    };
    let per_pr = baseline.units_per_merged_pr;
    let ratio = |units: f64| match per_pr > 0.0 {
        true => format!("{:.2}", units / per_pr),
        false => "-".to_owned(),
    };
    format!(
        "baseline {name}: {} units per merged pull request; kelpie / baseline, below 1 is \
         cheaper: {} loaded, {} median",
        thousands(per_pr.round() as u64),
        ratio(loaded),
        ratio(median)
    )
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    match n {
        0 => None,
        n if n % 2 == 1 => Some(values[n / 2]),
        n => Some((values[n / 2 - 1] + values[n / 2]) / 2.0),
    }
}

// A role's calls in `calls`, totalled on one line
fn totals(name: &str, calls: &[&CallLine], role: Role) -> String {
    let mine: Vec<&&CallLine> = calls.iter().filter(|call| call.role == role).collect();
    let compactions = (mine.iter())
        .filter(|call| call.kind == CallKind::Compact)
        .count();
    let units = mine.iter().map(|call| call.units).sum();
    let dollars = mine.iter().filter_map(|call| call.cost_usd).sum();
    let calls = match compactions {
        0 => count(mine.len(), "call"),
        c => format!("{} ({})", count(mine.len(), "call"), count(c, "compaction")),
    };
    let unpriced = mine.iter().filter(|call| call.unpriced).count();
    format!(
        "{name}: {calls}, {} units, {}{}",
        thousands(units),
        money(dollars),
        unpriced_note(unpriced)
    )
}

// Work items' calls with no finished line for them: open still, or run
// before the ledger kept finished lines
fn unrecorded(calls: &[&CallLine], recorded: &BTreeSet<u64>) -> Vec<String> {
    let mut by_issue: BTreeMap<u64, Vec<&CallLine>> = BTreeMap::new();
    for call in calls {
        if let Some(issue) = call.issue.filter(|issue| !recorded.contains(issue)) {
            by_issue.entry(issue).or_default().push(call);
        }
    }
    if by_issue.is_empty() {
        return Vec::new();
    }
    let mut out = vec!["no finished record, open or from before the ledger:".to_owned()];
    for (issue, calls) in by_issue {
        let pr = (calls.iter().rev().find_map(|call| call.pull_request))
            .map_or_else(String::new, |n| format!(" (#{n})"));
        let units = calls.iter().map(|call| call.units).sum();
        let dollars = calls.iter().filter_map(|call| call.cost_usd).sum();
        let unpriced = calls.iter().filter(|call| call.unpriced).count();
        out.push(format!(
            "  #{issue}{pr}: {}, {} units, {}{}",
            count(calls.len(), "call"),
            thousands(units),
            money(dollars),
            unpriced_note(unpriced)
        ));
    }
    out
}

fn count(n: usize, thing: &str) -> String {
    match n {
        1 => format!("1 {thing}"),
        n => format!("{n} {thing}s"),
    }
}

// What a dollar figure leaves out, so it never reads as complete
fn unpriced_note(unpriced: usize) -> String {
    match unpriced {
        0 => String::new(),
        n => format!(" + {}", count(n, "unpriced call")),
    }
}

fn money(dollars: f64) -> String {
    // An empty sum is -0.0, which prints its sign.
    format!("${:.2}", dollars + 0.0)
}

fn percent(share: Option<f64>) -> String {
    share.map_or_else(|| "-".to_owned(), |s| format!("{:.0}%", s * 100.0))
}

fn duration(seconds: u64) -> String {
    format!(
        "{}h{:02}m{:02}s",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
