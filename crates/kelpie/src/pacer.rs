//! Usage pacing
//!
//! Two limits, both read from the account's usage. Today's allowance is what
//! was left of the weekly window when the day began, divided by the days
//! until it resets; once today's spend reaches it, no new work item is
//! dispatched and the current one continues. Near half of the 5-hour window,
//! no turn starts until the window resets. A turn already running is never
//! interrupted, since the pacer is asked only between turns.
//!
//! Days are 24 hours counted from the weekly reset, so the days until reset
//! are always whole and the allowance is 100 / 7 on a fresh week.

use serde::{Deserialize, Serialize};

use crate::ports::{Timestamp, Utilization, Window};

/// Percent of the 5-hour window at which no further turn starts
pub const PARK_AT_PCT: u32 = 50;

/// How long a hold is trusted before usage is read again, in seconds
///
/// A reset time is printed to the minute and a window can reset while
/// `/usage` runs, so a hold is never trusted past its reset alone.
pub const RECHECK_SECS: u64 = 600;

const DAY_SECS: u64 = 86_400;
const WEEK_DAYS: u8 = 7;

// Reset times are printed to the minute and round either way (10:59pm on one
// read, 11pm on the next), so a weekly reset that moved by less than this is
// the same week.
const SAME_WEEK_SECS: u64 = 3600;

/// What the week had spent when today began, kept across a restart
// wire format: changing this is a breaking change to the state file
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DayStart {
    /// When the week this day belongs to resets
    pub week_resets_at: Timestamp,
    /// Which day of that week, from 0 on the first
    pub day: u8,
    /// Percent of the week used when the day began
    pub week_used_pct: u32,
}

/// Which limit is holding
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HoldKind {
    /// Today's allowance is spent: nothing new is dispatched
    Allowance,
    /// The 5-hour window is past its mark: no turn starts
    Window,
    /// Usage could not be read, so neither limit can be checked
    Unreadable,
}

/// Why kelpie is holding, and until when
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hold {
    /// Which limit
    pub kind: HoldKind,
    /// The maintainer's reading of it
    pub reason: String,
    /// When kelpie looks again
    pub until: Timestamp,
}

/// What usage was when the pacer last read it, and what it makes of it
// wire format: changing this is a breaking change to the pacer's status
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Reading {
    /// When it was read
    pub at: Timestamp,
    /// The 5-hour window
    pub session: Window,
    /// The weekly window
    pub week: Window,
    /// Percent of the week today may spend
    pub allowance_pct: f64,
    /// Percent of the week spent since the day began, by anyone on the account
    pub spent_today_pct: u32,
    /// The allowance spread over the kickoff hours
    pub per_hour_pct: f64,
}

/// What one reading of usage decides
#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    /// When the day began, to keep. None when usage could not be read.
    pub day_start: Option<DayStart>,
    /// The reading, when usage was read
    pub reading: Option<Reading>,
    /// Holds a new work item, and lets the current one continue
    pub allowance: Option<Hold>,
    /// Holds every turn, new work item or not
    pub window: Option<Hold>,
}

/// Which decision the pacer is asked for
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Whether to dispatch a new work item
    Dispatch,
    /// Whether the worker starts its next turn
    Turn,
}

impl Assessment {
    /// The hold that applies to `scope`, the one lasting longer when two do
    pub fn hold(&self, scope: Scope) -> Option<&Hold> {
        let allowance = match scope {
            Scope::Dispatch => self.allowance.as_ref(),
            Scope::Turn => None,
        };
        [allowance, self.window.as_ref()]
            .into_iter()
            .flatten()
            .max_by_key(|hold| hold.until)
    }

    /// Holding every decision, because usage could not be read
    pub fn unreadable(now: Timestamp, reason: String) -> Self {
        let hold = Hold {
            kind: HoldKind::Unreadable,
            reason,
            until: Timestamp(now.0 + RECHECK_SECS),
        };
        Self {
            day_start: None,
            reading: None,
            allowance: Some(hold.clone()),
            window: Some(hold),
        }
    }
}

/// Reads `usage` at `now` against the day's start and the kickoff hours
///
/// A new day or a new week starts the day over from `usage`.
pub fn assess(
    now: Timestamp,
    usage: &Utilization,
    prior: Option<DayStart>,
    kickoff_hours: u8,
) -> Assessment {
    let week_start = usage
        .week
        .resets_at
        .0
        .saturating_sub(u64::from(WEEK_DAYS) * DAY_SECS);
    let day = (now.0.saturating_sub(week_start) / DAY_SECS).min(u64::from(WEEK_DAYS) - 1) as u8;
    let day_start = prior
        .filter(|p| {
            p.day == day && p.week_resets_at.0.abs_diff(usage.week.resets_at.0) < SAME_WEEK_SECS
        })
        .unwrap_or(DayStart {
            week_resets_at: usage.week.resets_at,
            day,
            week_used_pct: usage.week.used_pct,
        });

    let days_left = f64::from(WEEK_DAYS - day);
    let left = f64::from(100u32.saturating_sub(day_start.week_used_pct));
    let allowance_pct = left / days_left;
    let spent_today_pct = usage.week.used_pct.saturating_sub(day_start.week_used_pct);
    let reading = Reading {
        at: now,
        session: usage.session,
        week: usage.week,
        allowance_pct: tenth(allowance_pct),
        spent_today_pct,
        per_hour_pct: tenth(allowance_pct / f64::from(kickoff_hours)),
    };

    let allowance = (f64::from(spent_today_pct) >= allowance_pct).then(|| Hold {
        kind: HoldKind::Allowance,
        reason: format!(
            "today's allowance of {:.1}% of the week is spent ({spent_today_pct}% since the day began), \
             so no new work item is dispatched until the next day; the current one continues",
            reading.allowance_pct
        ),
        until: Timestamp(week_start + (u64::from(day) + 1) * DAY_SECS),
    });
    let window = (usage.session.used_pct >= PARK_AT_PCT).then(|| Hold {
        kind: HoldKind::Window,
        reason: format!(
            "the 5-hour window is at {}%, past the {PARK_AT_PCT}% mark, \
             so no turn starts until it resets",
            usage.session.used_pct
        ),
        until: usage.session.resets_at,
    });
    Assessment {
        day_start: Some(day_start),
        reading: Some(reading),
        allowance,
        window,
    }
}

fn tenth(pct: f64) -> f64 {
    (pct * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEEK_START: u64 = 1_790_000_000;
    const RESETS: u64 = WEEK_START + 7 * DAY_SECS;

    fn usage(week: u32, session: u32) -> Utilization {
        Utilization {
            session: Window {
                used_pct: session,
                resets_at: Timestamp(WEEK_START + 3 * 3600),
            },
            week: Window {
                used_pct: week,
                resets_at: Timestamp(RESETS),
            },
        }
    }

    fn at(day: u64, hour: u64) -> Timestamp {
        Timestamp(WEEK_START + day * DAY_SECS + hour * 3600)
    }

    #[test]
    fn a_fresh_week_allows_a_seventh_of_it_spread_over_the_kickoff_hours() {
        let a = assess(at(0, 1), &usage(0, 0), None, 8);
        let reading = a.reading.unwrap();
        assert_eq!((reading.allowance_pct, reading.per_hour_pct), (14.3, 1.8));
        assert_eq!(
            a.day_start.unwrap(),
            DayStart {
                week_resets_at: Timestamp(RESETS),
                day: 0,
                week_used_pct: 0
            }
        );
        assert_eq!((a.allowance, a.window), (None, None));
    }

    #[test]
    fn the_allowance_is_what_was_left_when_the_day_began_over_the_days_left() {
        // 20% used when day 2 began: 80% over 5 days
        let start = DayStart {
            week_resets_at: Timestamp(RESETS),
            day: 2,
            week_used_pct: 20,
        };
        let a = assess(at(2, 9), &usage(25, 0), Some(start), 8);
        let reading = a.reading.unwrap();
        assert_eq!(reading.allowance_pct, 16.0);
        assert_eq!(reading.spent_today_pct, 5);
        assert_eq!(a.day_start, Some(start));
    }

    #[test]
    fn the_allowance_holds_dispatch_once_spent_and_not_before() {
        let start = DayStart {
            week_resets_at: Timestamp(RESETS),
            day: 0,
            week_used_pct: 0,
        };
        // 100 / 7 is 14.29%
        let under = assess(at(0, 5), &usage(14, 0), Some(start), 8);
        assert_eq!(under.hold(Scope::Dispatch), None);

        let over = assess(at(0, 5), &usage(15, 0), Some(start), 8);
        let hold = over.hold(Scope::Dispatch).unwrap();
        assert_eq!(hold.kind, HoldKind::Allowance);
        assert_eq!(hold.until, at(1, 0));
        assert_eq!(over.hold(Scope::Turn), None, "the current item continues");
    }

    #[test]
    fn a_new_day_starts_over_from_what_the_week_has_used() {
        let start = DayStart {
            week_resets_at: Timestamp(RESETS),
            day: 0,
            week_used_pct: 0,
        };
        let a = assess(at(1, 0), &usage(15, 0), Some(start), 8);
        assert_eq!(a.day_start.unwrap().day, 1);
        assert_eq!(a.day_start.unwrap().week_used_pct, 15);
        let reading = a.reading.unwrap();
        // 85% over the 6 days left
        assert_eq!((reading.allowance_pct, reading.spent_today_pct), (14.2, 0));
        assert_eq!(a.allowance, None);
    }

    #[test]
    fn a_new_week_starts_over_even_when_the_day_number_repeats() {
        let start = DayStart {
            week_resets_at: Timestamp(RESETS),
            day: 0,
            week_used_pct: 60,
        };
        let next =
            |usage: &mut Utilization| usage.week.resets_at = Timestamp(RESETS + 7 * DAY_SECS);
        let mut after = usage(2, 0);
        next(&mut after);
        let a = assess(Timestamp(RESETS + 3600), &after, Some(start), 8);
        assert_eq!(
            a.day_start.unwrap(),
            DayStart {
                week_resets_at: Timestamp(RESETS + 7 * DAY_SECS),
                day: 0,
                week_used_pct: 2
            }
        );
        assert_eq!(a.reading.unwrap().allowance_pct, 14.0);
    }

    #[test]
    fn a_reset_time_that_rounds_the_other_way_is_the_same_week() {
        let start = DayStart {
            week_resets_at: Timestamp(RESETS - 60),
            day: 0,
            week_used_pct: 10,
        };
        let a = assess(at(0, 2), &usage(12, 0), Some(start), 8);
        assert_eq!(a.day_start, Some(start));
    }

    #[test]
    fn the_last_day_may_spend_what_is_left() {
        let a = assess(at(6, 3), &usage(88, 0), None, 8);
        let reading = a.reading.unwrap();
        assert_eq!(reading.allowance_pct, 12.0);
        assert_eq!(a.allowance, None);

        let start = a.day_start.unwrap();
        let spent = assess(at(6, 20), &usage(100, 0), Some(start), 8);
        let hold = spent.hold(Scope::Dispatch).unwrap();
        assert_eq!(
            (hold.kind, hold.until),
            (HoldKind::Allowance, Timestamp(RESETS))
        );
    }

    #[test]
    fn a_week_with_nothing_left_holds_at_once() {
        let a = assess(at(3, 1), &usage(100, 0), None, 8);
        assert_eq!(a.reading.unwrap().allowance_pct, 0.0);
        assert_eq!(a.hold(Scope::Dispatch).unwrap().kind, HoldKind::Allowance);
    }

    #[test]
    fn the_window_parks_every_turn_from_half_until_it_resets() {
        let below = assess(at(0, 1), &usage(0, 49), None, 8);
        assert_eq!(below.hold(Scope::Turn), None);

        let parked = assess(at(0, 1), &usage(0, 50), None, 8);
        let hold = parked.hold(Scope::Turn).unwrap();
        assert_eq!(hold.kind, HoldKind::Window);
        assert_eq!(hold.until, at(0, 3));
        let dispatch = parked.hold(Scope::Dispatch).unwrap();
        assert_eq!(
            (dispatch.kind, dispatch.until),
            (HoldKind::Window, at(0, 3))
        );
    }

    #[test]
    fn two_holds_report_the_one_that_lasts_longer() {
        let start = DayStart {
            week_resets_at: Timestamp(RESETS),
            day: 0,
            week_used_pct: 0,
        };
        let a = assess(at(0, 1), &usage(20, 60), Some(start), 8);
        assert_eq!(a.hold(Scope::Dispatch).unwrap().kind, HoldKind::Allowance);
        assert_eq!(a.hold(Scope::Turn).unwrap().kind, HoldKind::Window);
    }

    #[test]
    fn an_unreadable_hold_covers_both_decisions_until_the_recheck() {
        let a = Assessment::unreadable(Timestamp(100), "cannot read usage".into());
        assert_eq!(a.day_start, None);
        for scope in [Scope::Dispatch, Scope::Turn] {
            let hold = a.hold(scope).unwrap();
            assert_eq!(
                (hold.kind, hold.until),
                (HoldKind::Unreadable, Timestamp(100 + RECHECK_SECS))
            );
        }
    }
}
