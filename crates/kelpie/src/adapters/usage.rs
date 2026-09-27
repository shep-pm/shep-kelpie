//! The account's usage, read with headless `claude -p "/usage"`
//!
//! `/usage` is a local command: no model turn and no tokens. It prints each
//! window as `Current session: 2% used · resets Sep 27 at 3:40am (America/New_York)`,
//! with the zone named at the end and the year left out.

use std::process::Command;
use std::time::Duration;

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use serde::Deserialize;

use super::claude::ClaudeCli;
use super::process::{Processes, RunError};
use crate::ports::{Meter, MeterError, Timestamp, Utilization, Window};

/// How long `/usage` gets to answer. It takes about 1.5 seconds.
const LIMIT: Duration = Duration::from_secs(30);

// A reset time this far in the past still counts as this year's or today's,
// so a window that reset while `/usage` ran is not moved a year or a day on.
const STALE_SECS: u64 = 3600;

/// Reads the account's usage through `claude`, one process per read
#[derive(Debug, Clone)]
pub struct UsageMeter {
    processes: Processes,
}

impl ClaudeCli {
    /// A meter whose reads end with this one's calls when the runner stops
    pub fn meter(&self) -> UsageMeter {
        UsageMeter {
            processes: self.processes.clone(),
        }
    }
}

impl Meter for UsageMeter {
    fn read(&self, now: Timestamp) -> Result<Utilization, MeterError> {
        // No settings and no transcript: this call is not a session of the
        // maintainer's, so none of their hooks should run for it.
        let output = self
            .processes
            .output_within(
                Command::new("claude").args([
                    "-p",
                    "/usage",
                    "--output-format",
                    "json",
                    "--setting-sources",
                    "",
                    "--no-session-persistence",
                ]),
                LIMIT,
            )
            .map_err(|e| match e {
                RunError::Io(e) => MeterError::Spawn(e.to_string()),
                RunError::Stopped => MeterError::Stopped,
                RunError::TimedOut => MeterError::TimedOut,
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Err(MeterError::Unreadable(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        parse(&stdout, now)
    }
}

/// Reads the utilization out of what `claude -p "/usage" --output-format json` printed
pub(super) fn parse(stdout: &str, now: Timestamp) -> Result<Utilization, MeterError> {
    #[derive(Deserialize)]
    struct Message {
        is_error: bool,
        #[serde(default)]
        result: String,
    }
    let unreadable = |what: &str| MeterError::Unreadable(what.to_owned());
    let message: Message = serde_json::from_str(stdout).map_err(|_| unreadable(stdout))?;
    if message.is_error {
        return Err(unreadable(&message.result));
    }
    Ok(Utilization {
        session: window(&message.result, "Current session:", now)?,
        week: window(&message.result, "Current week (all models):", now)?,
    })
}

fn window(text: &str, label: &str, now: Timestamp) -> Result<Window, MeterError> {
    let unreadable = |what: String| MeterError::Unreadable(what);
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix(label))
        .ok_or_else(|| unreadable(format!("no `{label}` line")))?;
    let read = || -> Option<Window> {
        let (used, resets) = line.split_once("% used")?;
        let (_, when) = resets.split_once("resets ")?;
        Some(Window {
            used_pct: used.trim().parse().ok()?,
            resets_at: reset_time(when.trim(), now)?,
        })
    };
    read().ok_or_else(|| unreadable(format!("cannot read `{label}{line}`")))
}

// `Sep 27 at 3:40am (America/New_York)`, or `3:40am (America/New_York)` for a
// reset later today or tomorrow. The minutes are left out on the hour.
fn reset_time(when: &str, now: Timestamp) -> Option<Timestamp> {
    let (when, zone) = when.strip_suffix(')')?.rsplit_once(" (")?;
    let zone = TimeZone::get(zone).ok()?;
    let (day, time) = match when.split_once(" at ") {
        Some((day, time)) => (Some(day), time),
        None => (None, when),
    };
    let (hour, minute) = clock(time)?;
    let floor = i64::try_from(now.0.saturating_sub(STALE_SECS)).ok()?;
    let today = jiff::Timestamp::from_second(i64::try_from(now.0).ok()?)
        .ok()?
        .to_zoned(zone.clone())
        .date();
    let at = |date: Date| -> Option<i64> {
        let zoned = DateTime::from_parts(date, jiff::civil::time(hour, minute, 0, 0))
            .to_zoned(zone.clone())
            .ok()?;
        Some(zoned.timestamp().as_second())
    };
    let seconds = match day {
        Some(day) => {
            let (month, day) = month_day(day)?;
            let this_year = at(Date::new(today.year(), month, day).ok()?)?;
            if this_year >= floor {
                this_year
            } else {
                at(Date::new(today.year().checked_add(1)?, month, day).ok()?)?
            }
        }
        None => {
            let this_day = at(today)?;
            if this_day >= floor {
                this_day
            } else {
                at(today.tomorrow().ok()?)?
            }
        }
    };
    u64::try_from(seconds).ok().map(Timestamp)
}

// `3:40am`, `11pm` and `12am` as an hour and minute of the 24-hour clock
fn clock(time: &str) -> Option<(i8, i8)> {
    let (digits, pm) = match time.strip_suffix("am") {
        Some(digits) => (digits, false),
        None => (time.strip_suffix("pm")?, true),
    };
    let (hour, minute) = digits.split_once(':').unwrap_or((digits, "0"));
    let (hour, minute) = (hour.parse::<i8>().ok()?, minute.parse::<i8>().ok()?);
    ((1..=12).contains(&hour) && (0..60).contains(&minute))
        .then_some((hour % 12 + if pm { 12 } else { 0 }, minute))
}

// `Sep 27` as a month and a day of the month
fn month_day(day: &str) -> Option<(i8, i8)> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (month, day) = day.split_once(' ')?;
    let month = MONTHS.iter().position(|m| *m == month)?;
    Some((i8::try_from(month + 1).ok()?, day.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::Clock;

    const RECORDED: &str = include_str!("../../fixtures/usage-result.json");

    fn ts(rfc3339: &str) -> Timestamp {
        let at: jiff::Timestamp = rfc3339.parse().unwrap();
        Timestamp(u64::try_from(at.as_second()).unwrap())
    }

    #[test]
    fn the_recorded_output_reads_as_both_windows() {
        let utilization = parse(RECORDED, ts("2026-09-26T12:00:00-04:00")).unwrap();
        assert_eq!(
            utilization,
            Utilization {
                session: Window {
                    used_pct: 2,
                    resets_at: ts("2026-09-27T03:40:00-04:00"),
                },
                week: Window {
                    used_pct: 21,
                    resets_at: ts("2026-10-02T23:00:00-04:00"),
                },
            }
        );
    }

    #[test]
    fn the_weekly_line_is_the_one_for_all_models() {
        // The recorded output also has a `Current week (Fable)` line, at 8%.
        let utilization = parse(RECORDED, ts("2026-09-26T12:00:00-04:00")).unwrap();
        assert_ne!(utilization.week.used_pct, 8);
    }

    #[test]
    fn a_reset_time_is_read_as_the_next_one_after_now() {
        // The first two are printed in the experiments repo's transport results
        // (`transport/results/t1.jsonl`), the rest are forms `/usage` can print.
        let cases = [
            (
                "Sep 25 at 10:59pm",
                "2026-09-25T12:00:00-04:00",
                "2026-09-25T22:59:00-04:00",
            ),
            (
                "Sep 25 at 11pm",
                "2026-09-25T12:00:00-04:00",
                "2026-09-25T23:00:00-04:00",
            ),
            (
                "3:40am",
                "2026-09-26T12:00:00-04:00",
                "2026-09-27T03:40:00-04:00",
            ),
            (
                "3:40am",
                "2026-09-26T01:00:00-04:00",
                "2026-09-26T03:40:00-04:00",
            ),
            (
                "12am",
                "2026-09-26T12:00:00-04:00",
                "2026-09-27T00:00:00-04:00",
            ),
            (
                "12pm",
                "2026-09-26T08:00:00-04:00",
                "2026-09-26T12:00:00-04:00",
            ),
            // January is standard time
            (
                "Jan 2 at 1am",
                "2026-12-31T12:00:00-05:00",
                "2027-01-02T01:00:00-05:00",
            ),
        ];
        for (printed, now, expected) in cases {
            assert_eq!(
                reset_time(&format!("{printed} (America/New_York)"), ts(now)),
                Some(ts(expected)),
                "{printed} at {now}"
            );
        }
    }

    #[test]
    fn a_reset_that_passed_minutes_ago_is_not_moved_on() {
        let now = ts("2026-09-27T03:50:00-04:00");
        assert_eq!(
            reset_time("3:40am (America/New_York)", now),
            Some(ts("2026-09-27T03:40:00-04:00"))
        );
        assert_eq!(
            reset_time("Sep 27 at 3:40am (America/New_York)", now),
            Some(ts("2026-09-27T03:40:00-04:00"))
        );
    }

    #[test]
    fn a_reset_time_that_cannot_be_read_is_refused() {
        let now = ts("2026-09-26T12:00:00-04:00");
        for printed in [
            "",
            "3:40am",
            "3:40am (Nowhere/Land)",
            "3:40 (America/New_York)",
            "13pm (America/New_York)",
            "0am (America/New_York)",
            "3:60am (America/New_York)",
            "Foo 2 at 3am (America/New_York)",
            "Feb 30 at 3am (America/New_York)",
        ] {
            assert_eq!(reset_time(printed, now), None, "{printed:?}");
        }
    }

    #[test]
    fn output_without_a_window_names_the_line_it_lacks() {
        let text = "Current week (all models): 21% used · resets Oct 2 at 11pm (America/New_York)";
        let json = serde_json::json!({ "is_error": false, "result": text }).to_string();
        assert_eq!(
            parse(&json, ts("2026-09-26T12:00:00-04:00")),
            Err(MeterError::Unreadable("no `Current session:` line".into()))
        );
    }

    #[test]
    fn a_window_line_that_does_not_parse_is_quoted() {
        let text = "Current session: soon\nCurrent week (all models): 1% used · resets 1am (UTC)";
        let json = serde_json::json!({ "is_error": false, "result": text }).to_string();
        assert_eq!(
            parse(&json, ts("2026-09-26T12:00:00-04:00")),
            Err(MeterError::Unreadable(
                "cannot read `Current session: soon`".into()
            ))
        );
    }

    #[test]
    #[ignore = "runs the real `claude -p /usage` against the maintainer's account, about 2 s"]
    fn the_real_claude_answers_with_both_windows() {
        let now = crate::adapters::SystemClock.now();
        let usage = ClaudeCli::default().meter().read(now).unwrap();
        assert!(usage.week.resets_at > now && usage.session.resets_at > now);
        assert!(usage.week.used_pct <= 100 && usage.session.used_pct <= 100);
    }

    #[test]
    fn an_error_result_and_plain_text_are_both_unreadable() {
        let now = ts("2026-09-26T12:00:00-04:00");
        let error = r#"{"is_error":true,"result":"Not logged in"}"#;
        assert_eq!(
            parse(error, now),
            Err(MeterError::Unreadable("Not logged in".into()))
        );
        assert_eq!(
            parse("nope", now),
            Err(MeterError::Unreadable("nope".into()))
        );
    }
}
