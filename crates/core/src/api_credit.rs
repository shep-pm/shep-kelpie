//! One grant of the maintainer's API credit: its amount, the day it resets on, and the window a moment falls in

use jiff::civil::Date;
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::ports::Timestamp;

/// One grant of API credit and the day of the month it resets on
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiCredit {
    /// What the grant is worth in US dollars, rounded to a cent. More than 0.
    usd: GrantUsd,
    /// The day of the month, in UTC, the grant resets on: 1 to 31. A month
    /// with fewer days resets on its last.
    day: GrantDay,
}

/// A grant's worth in whole cents
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "f64")]
// schemars describes a `try_from` type by its source, so the bounds go here.
#[schemars(extend("exclusiveMinimum" = 0))]
struct GrantUsd(u64);

impl TryFrom<f64> for GrantUsd {
    type Error = &'static str;

    fn try_from(usd: f64) -> Result<Self, Self::Error> {
        // Refused before rounding, which would lift 0.005 to a cent. The cast
        // saturates, so an absurd figure cannot wrap.
        match usd.is_finite() && usd >= 0.01 {
            true => Ok(Self((usd * 100.0).round() as u64)),
            false => Err("must be a number of dollars above 0, such as 50 or 12.5"),
        }
    }
}

/// A day of the month, 1 to 31
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(try_from = "u8")]
#[schemars(extend("minimum" = 1, "maximum" = 31))]
struct GrantDay(u8);

impl TryFrom<u8> for GrantDay {
    type Error = &'static str;

    fn try_from(day: u8) -> Result<Self, Self::Error> {
        match (1..=31).contains(&day) {
            true => Ok(Self(day)),
            false => Err("must be a day of the month, 1 to 31"),
        }
    }
}

impl ApiCredit {
    /// What the grant is worth, in US dollars
    pub fn cap_usd(&self) -> f64 {
        self.usd.0 as f64 / 100.0
    }

    /// The day of the month, in UTC, it resets on
    pub fn day(&self) -> u8 {
        self.day.0
    }

    /// The grant `now` falls in: from 00:00 UTC on the latest reset day at or
    /// before `now`, to the next one
    pub fn window(&self, now: Timestamp) -> (Timestamp, Timestamp) {
        self.try_window(now).unwrap_or((now, now))
    }

    // None only for a time past the years jiff can name.
    fn try_window(&self, now: Timestamp) -> Option<(Timestamp, Timestamp)> {
        let today = jiff::Timestamp::from_second(i64::try_from(now.0).ok()?)
            .ok()?
            .to_zoned(TimeZone::UTC)
            .date();
        let this = self.reset_in(today)?;
        let month = jiff::Span::new().months(1);
        let (start, end) = if today >= this {
            (this, self.reset_in(today.checked_add(month).ok()?)?)
        } else {
            (self.reset_in(today.checked_sub(month).ok()?)?, this)
        };
        Some((midnight(start)?, midnight(end)?))
    }

    // The reset day of the month `date` is in, the month's last when it is shorter.
    fn reset_in(&self, date: Date) -> Option<Date> {
        let day = i8::try_from(self.day.0).ok()?.min(date.days_in_month());
        Date::new(date.year(), date.month(), day).ok()
    }
}

// 00:00 UTC on `date`
fn midnight(date: Date) -> Option<Timestamp> {
    let zoned = date.at(0, 0, 0, 0).to_zoned(TimeZone::UTC).ok()?;
    Some(Timestamp(
        u64::try_from(zoned.timestamp().as_second()).ok()?,
    ))
}
