//! A plan refusal: Codex turning a turn away for its plan's usage limit
//!
//! The runner reads a failed Codex turn's text with [`codex_limit`] and holds
//! Codex until the time it names. The wording is Codex's own, recorded.

use jiff::tz::TimeZone;

use crate::pacer::RECHECK_SECS;
use crate::ports::Timestamp;

/// When Codex says to try again, or `None` when `text` is no usage-limit refusal
///
/// A clock time alone, `5:46 PM`, is the next such time after `now`. A date
/// with a time, `Nov 12th, 2026 7:38 AM`, is that moment. Both are read in
/// `zone`. A limit whose time cannot be read, or is not after `now`, is one
/// [`RECHECK_SECS`] on from `now`.
pub fn codex_limit(text: &str, now: Timestamp, zone: &TimeZone) -> Option<Timestamp> {
    let text = text.replace('\u{2019}', "'").to_lowercase();
    if !text.contains("hit your usage limit") {
        return None;
    }
    let said = text.split("try again at ").nth(1);
    let at = said.and_then(|said| named_time(said, now, zone));
    Some(
        at.filter(|at| *at > now)
            .unwrap_or(Timestamp(now.0 + RECHECK_SECS)),
    )
}

// The time `said` opens with, in either of Codex's shapes.
fn named_time(said: &str, now: Timestamp, zone: &TimeZone) -> Option<Timestamp> {
    let mut words = said.split_whitespace();
    let first = words.next()?;
    if first.contains(':') {
        let time = clock(first, words.next()?)?;
        let today = jiff::Timestamp::from_second(i64::try_from(now.0).ok()?)
            .ok()?
            .to_zoned(zone.clone())
            .date();
        let next = stamp(today.at(time.hour(), time.minute(), 0, 0), zone)?;
        if next > now {
            return Some(next);
        }
        return stamp(
            today.tomorrow().ok()?.at(time.hour(), time.minute(), 0, 0),
            zone,
        );
    }
    let month = months().position(|month| month == first)?;
    let day = words.next()?.strip_suffix(',')?;
    let day = ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|suffix| day.strip_suffix(suffix))?;
    let year = words.next()?;
    let time = clock(words.next()?, words.next()?)?;
    let date = jiff::civil::date(
        year.parse().ok()?,
        i8::try_from(month + 1).ok()?,
        day.parse().ok()?,
    );
    stamp(date.at(time.hour(), time.minute(), 0, 0), zone)
}

fn months() -> impl Iterator<Item = &'static str> {
    [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .into_iter()
}

// `5:46` and `pm.` as a time of day.
fn clock(time: &str, meridiem: &str) -> Option<jiff::civil::Time> {
    let (hour, minute) = time.split_once(':')?;
    let hour: i8 = hour.parse().ok()?;
    let minute: i8 = minute.parse().ok()?;
    let pm = match meridiem.trim_end_matches('.') {
        "am" => false,
        "pm" => true,
        _ => return None,
    };
    if !(1..=12).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    jiff::civil::Time::new(hour % 12 + if pm { 12 } else { 0 }, minute, 0, 0).ok()
}

fn stamp(at: jiff::civil::DateTime, zone: &TimeZone) -> Option<Timestamp> {
    let at = at.to_zoned(zone.clone()).ok()?;
    Some(Timestamp(u64::try_from(at.timestamp().as_second()).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(stamp: &str) -> Timestamp {
        let stamp: jiff::Timestamp = stamp.parse().unwrap();
        Timestamp(u64::try_from(stamp.as_second()).unwrap())
    }

    fn est() -> TimeZone {
        TimeZone::fixed(jiff::tz::offset(-5))
    }

    fn said(when: &str) -> String {
        format!(
            "You've hit your usage limit. Upgrade to Pro \
             (https://chatgpt.com/explore/pro), visit \
             https://chatgpt.com/codex/settings/usage to purchase more credits \
             or try again at {when}."
        )
    }

    #[test]
    fn a_clock_time_alone_is_the_next_such_time() {
        let before = at("2026-11-09T12:00:00-05:00");
        let after = at("2026-11-09T18:00:00-05:00");
        let text = said("5:46 PM");

        assert_eq!(
            codex_limit(&text, before, &est()),
            Some(at("2026-11-09T17:46:00-05:00"))
        );
        assert_eq!(
            codex_limit(&text, after, &est()),
            Some(at("2026-11-10T17:46:00-05:00"))
        );
        assert_eq!(
            codex_limit(&said("12:05 AM"), before, &est()),
            Some(at("2026-11-10T00:05:00-05:00"))
        );
        assert_eq!(
            codex_limit(&said("12:05 PM"), before, &est()),
            Some(at("2026-11-09T12:05:00-05:00"))
        );
    }

    #[test]
    fn a_date_and_time_is_read_in_the_zone_given() {
        let now = at("2026-11-09T12:00:00Z");
        let text = said("Nov 12th, 2026 7:38 AM");

        assert_eq!(
            codex_limit(&text, now, &TimeZone::UTC),
            Some(at("2026-11-12T07:38:00Z"))
        );
        assert_eq!(
            codex_limit(&text, now, &est()),
            Some(at("2026-11-12T07:38:00-05:00"))
        );
        for day in ["1st", "2nd", "3rd", "4th"] {
            let text = said(&format!("Dec {day}, 2026 7:38 PM"));
            assert!(codex_limit(&text, now, &TimeZone::UTC).is_some(), "{day}");
        }
    }

    #[test]
    fn a_limit_with_no_readable_time_holds_until_the_recheck() {
        let now = at("2026-11-09T12:00:00Z");
        let recheck = Some(Timestamp(now.0 + RECHECK_SECS));

        for text in [
            "You've hit your usage limit.",
            "You've hit your usage limit. Try again later.",
            "You've hit your usage limit, or try again at tomorrow.",
            "You've hit your usage limit. Try again at 25:99 PM.",
            "You've hit your usage limit. Try again at Nov 12th 2026 7:38 AM.",
            // A date already gone is no time to wait for.
            "You've hit your usage limit. Try again at Nov 1st, 2026 7:38 AM.",
        ] {
            assert_eq!(codex_limit(text, now, &TimeZone::UTC), recheck, "{text}");
        }
    }

    #[test]
    fn other_text_is_no_codex_limit() {
        let now = at("2026-11-09T12:00:00Z");

        for text in [
            "",
            "overloaded",
            "stream disconnected before completion",
            "You have reached your specified API usage limits.",
            "Try again at 5:46 PM.",
        ] {
            assert_eq!(codex_limit(text, now, &TimeZone::UTC), None, "{text}");
        }
    }
}
