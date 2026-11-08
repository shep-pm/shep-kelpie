//! The plan lines of the board's agents section: each plan account's week
//! against the share of it gone, the budget the project manager spends by its
//! reset, and the API credit's month beside them

use std::fmt::Write as _;

use crate::pacer::{DAY_SECS, Reading, week_end_pct, week_gone_pct};
use crate::ports::Timestamp;

use super::elapsed;

/// A plan account the listed agents spend, as the agents section shows its week
#[derive(Debug, Clone, PartialEq)]
pub struct PlanLine {
    /// The account's display name, as `Claude`
    pub name: String,
    /// What the pacer last read of it, `None` while nothing has paced it
    pub reading: Option<Reading>,
}

/// The API credit, as the agents section shows its month
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CreditLine {
    /// What the agents billed to the API key cost in the month, in US dollars
    pub spent_usd: f64,
    /// What the credit gives each month, in US dollars
    pub monthly_usd: f64,
    /// The first second of the month the spend is counted over
    pub since: Timestamp,
    /// The first second of the next month, when the grant resets
    pub resets_at: Timestamp,
}

/// One line for each of `plans`, then `credit`, as they stand at `now`
///
/// Writes nothing, the blank line above them included, when there is no plan
/// account and no credit line.
pub(super) fn lines(out: &mut String, plans: &[PlanLine], credit: Option<&str>, now: Timestamp) {
    if plans.is_empty() && credit.is_none() {
        return;
    }
    out.push('\n');
    for plan in plans {
        let _ = writeln!(out, "{} week: {}", plan.name, week(plan, now));
    }
    if let Some(line) = credit {
        let _ = writeln!(out, "{line}");
    }
}

/// The API credit's line, or `None` when its reset has passed or the month it
/// counts has no length
pub(super) fn credit_line(credit: &CreditLine, now: Timestamp) -> Option<String> {
    if credit.resets_at <= now {
        return None;
    }
    let month = credit.resets_at.0.saturating_sub(credit.since.0);
    if month == 0 {
        return None;
    }
    let gone = gone_pct(now.0.saturating_sub(credit.since.0), month);
    let used = used_pct(credit.spent_usd, credit.monthly_usd);
    let course =
        week_end_pct(used, gone).map_or_else(String::new, |end| format!(", on course for {end}%"));
    Some(format!(
        "API credit: {used}% used, {gone}% of the month gone, {} to the reset{course}",
        span(credit.resets_at.0 - now.0),
    ))
}

fn week(plan: &PlanLine, now: Timestamp) -> String {
    let Some(read) = &plan.reading else {
        return "no reading".to_owned();
    };
    let ago = span(now.0.saturating_sub(read.at.0));
    let week = read.week;
    if week.resets_at <= now {
        return format!("the reading predates the week's reset, read {ago} ago");
    }
    let gone = week_gone_pct(now, week.resets_at);
    let course = week_end_pct(week.used_pct, gone)
        .map_or_else(String::new, |end| format!(", on course for {end}%"));
    format!(
        "{}% used, {gone}% of the week gone, {} to the reset{course}, read {ago} ago",
        week.used_pct,
        span(week.resets_at.0 - now.0),
    )
}

// The share of a `month`-second month gone `gone` seconds into it, whole percent
fn gone_pct(gone: u64, month: u64) -> u32 {
    let gone = gone.min(month);
    u32::try_from((gone.saturating_mul(100) + month / 2) / month).unwrap_or(100)
}

// `spent` over `monthly` as whole percent, uncapped above 100
fn used_pct(spent: f64, monthly: f64) -> u32 {
    (spent / monthly * 100.0 + 0.5) as u32
}

// `seconds` as `6d 1h`, or `3d` when no whole hour is left over, or `3h05m` under a day
fn span(seconds: u64) -> String {
    let (days, hours) = (seconds / DAY_SECS, seconds % DAY_SECS / 3600);
    match (days, hours) {
        (0, _) => elapsed(seconds),
        (days, 0) => format!("{days}d"),
        (days, hours) => format!("{days}d {hours}h"),
    }
}
