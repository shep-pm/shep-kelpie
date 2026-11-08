//! The plan lines of the board's agents section: each plan account's week
//! against the share of it gone, the budget the project manager spends by its reset

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

/// One line for each of `plans`, as the account's week stands at `now`
pub(super) fn lines(out: &mut String, plans: &[PlanLine], now: Timestamp) {
    for plan in plans {
        let _ = writeln!(out, "{} week: {}", plan.name, standing(plan, now));
    }
}

fn standing(plan: &PlanLine, now: Timestamp) -> String {
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

// `seconds` as `6d 1h`, or `3d` when no whole hour is left over, or `3h05m` under a day
fn span(seconds: u64) -> String {
    let (days, hours) = (seconds / DAY_SECS, seconds % DAY_SECS / 3600);
    match (days, hours) {
        (0, _) => elapsed(seconds),
        (days, 0) => format!("{days}d"),
        (days, hours) => format!("{days}d {hours}h"),
    }
}
