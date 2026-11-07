//! Usage pacing
//!
//! The pacer reads an account's usage and holds a spent limit: a turn waits
//! while the 5-hour window or the week is spent, until the longer of the two
//! ends. Nothing else holds a turn or a dispatch, and usage that cannot be
//! read holds nothing. A running turn is never interrupted: the pacer is asked
//! only between turns.
//!
//! [`window`]'s 50% mark is what the agent board's waits read. `status` and
//! the plan gauges show each window as the pacer last read it.

use serde::{Deserialize, Serialize};

use crate::ports::{Timestamp, Utilization, Window};
use crate::settings::Account;

/// Percent of the 5-hour window at which no further turn starts
pub const PARK_AT_PCT: u32 = 50;

/// Percent of a window at which it is spent, where a turn fails at once
pub const SPENT_PCT: u32 = 100;

/// How long a hold is trusted before usage is read again, in seconds
///
/// A reset time is printed to the minute and a window can reset while
/// `/usage` runs, so a hold is never trusted past its reset alone.
pub const RECHECK_SECS: u64 = 600;

/// Which limit is holding
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HoldKind {
    /// The 5-hour window is past its mark or the week is spent: no turn starts
    Window,
    /// Usage could not be read, so neither limit can be checked
    Unreadable,
}

/// Why kelpie is holding, and until when
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    /// Which limit
    pub kind: HoldKind,
    /// The maintainer's reading of it
    pub reason: String,
    /// When kelpie looks again
    pub until: Timestamp,
}

/// What usage was when the pacer last read it
// wire format: changing this is a breaking change to the pacer's status
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    /// When it was read
    pub at: Timestamp,
    /// The 5-hour window
    pub session: Window,
    /// The weekly window
    pub week: Window,
}

/// What one reading of usage decides
#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    /// The reading, when usage was read
    pub reading: Option<Reading>,
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
    /// The hold on every turn and every new work item, which is `window`
    pub fn hold(&self) -> Option<&Hold> {
        self.window.as_ref()
    }

    /// Holding every decision, because usage could not be read
    pub fn unreadable(now: Timestamp, reason: String) -> Self {
        let hold = Hold {
            kind: HoldKind::Unreadable,
            reason,
            until: Timestamp(now.0 + RECHECK_SECS),
        };
        Self {
            reading: None,
            window: Some(hold),
        }
    }
}

/// Reads `account`'s `usage` at `now`
pub fn assess(account: Account, now: Timestamp, usage: &Utilization) -> Assessment {
    let reading = Reading {
        at: now,
        session: usage.session,
        week: usage.week,
    };

    Assessment {
        reading: Some(reading),
        window: window(account, usage),
    }
}

/// The hold on every turn while `account`'s 5-hour window is past [`PARK_AT_PCT`] or its
/// week is spent, until the window or the week resets, the longer of the two when both apply
///
/// The agent board's waits read this mark; a runner holds a turn only at [`SPENT_PCT`].
pub fn window(account: Account, usage: &Utilization) -> Option<Hold> {
    let session = (usage.session.used_pct >= PARK_AT_PCT).then(|| Hold {
        kind: HoldKind::Window,
        reason: format!(
            "{} 5-hour window is at {}%, past the {PARK_AT_PCT}% mark, \
             so no turn starts until it resets",
            named(account, "the", "Codex's"),
            usage.session.used_pct
        ),
        until: usage.session.resets_at,
    });
    [session, spent_week(account, &usage.week)]
        .into_iter()
        .flatten()
        .max_by_key(|hold| hold.until)
}

/// The hold on every turn while `account`'s 5-hour window or `week` is at [`SPENT_PCT`],
/// until it resets, the longer of the two when both apply
///
/// A spent window fails each turn at once, as a spent week does.
pub fn spent(account: Account, usage: &Utilization) -> Option<Hold> {
    let session = (usage.session.used_pct >= SPENT_PCT).then(|| Hold {
        kind: HoldKind::Window,
        reason: format!(
            "{} 5-hour window is at {}%, so no turn starts until it resets",
            named(account, "the", "Codex's"),
            usage.session.used_pct
        ),
        until: usage.session.resets_at,
    });
    [session, spent_week(account, &usage.week)]
        .into_iter()
        .flatten()
        .max_by_key(|hold| hold.until)
}

/// The hold on every turn while `account`'s `week` is at [`SPENT_PCT`], until it resets
///
/// A spent week fails each turn at once.
pub fn spent_week(account: Account, week: &Window) -> Option<Hold> {
    (week.used_pct >= SPENT_PCT).then(|| Hold {
        kind: HoldKind::Window,
        reason: format!(
            "{} week is at {}%, so no turn starts until it resets",
            named(account, "the", "Codex's"),
            week.used_pct
        ),
        until: week.resets_at,
    })
}

/// `claude` for the Claude account, which reasons have always meant, else `other`
pub fn named(account: Account, claude: &'static str, other: &'static str) -> &'static str {
    match account {
        Account::Claude => claude,
        Account::Codex => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::EPOCH;

    const DAY_SECS: u64 = 86_400;
    const RESETS: u64 = EPOCH + 7 * DAY_SECS;

    fn usage(week: u32, session: u32) -> Utilization {
        Utilization {
            session: Window {
                used_pct: session,
                resets_at: Timestamp(EPOCH + 3 * 3600),
            },
            week: Window {
                used_pct: week,
                resets_at: Timestamp(RESETS),
            },
        }
    }

    fn at(day: u64, hour: u64) -> Timestamp {
        Timestamp(EPOCH + day * DAY_SECS + hour * 3600)
    }

    #[test]
    fn the_last_day_holds_nothing_until_the_week_is_spent() {
        let a = assess(Account::Claude, at(6, 3), &usage(88, 0));
        assert_eq!(a.window, None);

        let spent = assess(Account::Claude, at(6, 20), &usage(100, 0));
        let hold = spent.hold().unwrap();
        assert_eq!(
            (hold.kind, hold.until),
            (HoldKind::Window, Timestamp(RESETS)),
            "the week is spent too, which holds every turn"
        );
    }

    #[test]
    fn a_week_with_nothing_left_holds_at_once() {
        let a = assess(Account::Claude, at(3, 1), &usage(100, 0));
        let hold = a.hold().unwrap();
        assert_eq!(
            (hold.kind, hold.until),
            (HoldKind::Window, Timestamp(RESETS))
        );
    }

    #[test]
    fn the_window_parks_every_turn_from_half_until_it_resets() {
        let below = assess(Account::Claude, at(0, 1), &usage(0, 49));
        assert_eq!(below.hold(), None);

        let parked = assess(Account::Claude, at(0, 1), &usage(0, 50));
        let hold = parked.hold().unwrap();
        assert_eq!(hold.kind, HoldKind::Window);
        assert_eq!(hold.until, at(0, 3));
    }

    #[test]
    fn a_spent_week_holds_every_turn_until_the_week_resets_and_outlasts_the_window() {
        let a = assess(Account::Codex, at(3, 1), &usage(100, 60));
        let hold = a.hold().unwrap();
        assert_eq!(
            (hold.kind, hold.until),
            (HoldKind::Window, Timestamp(RESETS))
        );
        assert_eq!(
            hold.reason,
            "Codex's week is at 100%, so no turn starts until it resets"
        );

        let almost = assess(Account::Codex, at(3, 1), &usage(99, 0));
        assert_eq!(almost.hold(), None);
    }

    #[test]
    fn a_spent_window_holds_every_turn_until_it_resets() {
        assert_eq!(
            spent(Account::Claude, &usage(0, 99)),
            None,
            "99% holds nothing"
        );

        let hold = spent(Account::Claude, &usage(0, 100)).unwrap();
        assert_eq!((hold.kind, hold.until), (HoldKind::Window, at(0, 3)));
        assert_eq!(
            hold.reason,
            "the 5-hour window is at 100%, so no turn starts until it resets"
        );

        let codex = spent(Account::Codex, &usage(0, 100)).unwrap();
        assert_eq!(
            codex.reason,
            "Codex's 5-hour window is at 100%, so no turn starts until it resets"
        );
    }

    #[test]
    fn a_spent_week_and_a_spent_window_report_the_longer_hold() {
        let both = spent(Account::Codex, &usage(100, 100)).unwrap();
        assert_eq!(
            (both.kind, both.until),
            (HoldKind::Window, Timestamp(RESETS)),
            "the week outlasts the window"
        );
        assert_eq!(
            both.reason,
            "Codex's week is at 100%, so no turn starts until it resets"
        );

        let mut later = usage(100, 100);
        later.session.resets_at = Timestamp(RESETS + 3600);
        let later = spent(Account::Codex, &later).unwrap();
        assert_eq!(
            (later.kind, later.until),
            (HoldKind::Window, Timestamp(RESETS + 3600))
        );
        assert_eq!(
            later.reason, "Codex's 5-hour window is at 100%, so no turn starts until it resets",
            "the window outlasts the week"
        );
    }

    #[test]
    fn a_window_that_has_not_started_holds_nothing() {
        // The meter reads `Current session: 0% used` as ending five hours on.
        let now = at(0, 1);
        let mut fresh = usage(0, 0);
        fresh.session.resets_at = Timestamp(now.0 + 5 * 3600);
        let a = assess(Account::Claude, now, &fresh);
        assert_eq!(a.hold(), None);
    }

    #[test]
    fn an_unreadable_hold_covers_both_decisions_until_the_recheck() {
        let a = Assessment::unreadable(Timestamp(100), "cannot read usage".into());
        let hold = a.hold().unwrap();
        assert_eq!(
            (hold.kind, hold.until),
            (HoldKind::Unreadable, Timestamp(100 + RECHECK_SECS))
        );
    }
}
