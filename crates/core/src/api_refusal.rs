//! An API refusal: the API turning a key away for spent credit or a spent limit
//!
//! The runner reads a refused call's text with [`refusal`] and tells the dog,
//! which holds every API agent until [`Refusal::hold_until`]. The wording is
//! Anthropic's documentation's, not a recording.

use serde::{Deserialize, Serialize};

use crate::pacer::RECHECK_SECS;
use crate::ports::Timestamp;

/// How long exhausted credit, or a spend limit with no readable time, holds
/// before the API is tried again, in seconds
///
/// The credit message names no time, and a top-up arrives whenever the maintainer makes it.
pub const CREDIT_RECHECK_SECS: u64 = 6 * 3600;

/// Why the API refused the key
// wire format: changing this is a breaking change to the door
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// The organization's spend limit is reached, with the time access resumes when the message gave one
    SpendLimit {
        /// When access resumes, if the message said
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resumes: Option<Timestamp>,
    },
    /// The credit balance is too low, which names no time
    NoCredit,
}

/// The refusal `text` holds, or None when it holds none
pub fn refusal(text: &str) -> Option<Refusal> {
    let text = text.to_lowercase();
    if text.contains("you have reached your") && text.contains("api usage limits") {
        return Some(Refusal::SpendLimit {
            resumes: resumes(&text),
        });
    }
    (text.contains("credit balance is too low") || text.contains("credit balance too low"))
        .then_some(Refusal::NoCredit)
}

// The time after "you will regain access on", as `2026-09-01 at 00:00 utc`.
fn resumes(text: &str) -> Option<Timestamp> {
    let said = text.split("you will regain access on ").nth(1)?;
    let mut words = said.split_whitespace();
    let (date, at, time, zone) = (words.next()?, words.next()?, words.next()?, words.next()?);
    if at != "at" || zone.trim_end_matches(|c: char| !c.is_ascii_alphabetic()) != "utc" {
        return None;
    }
    let time = time
        .get(..5)
        .filter(|t| t.as_bytes().get(2) == Some(&b':'))?;
    let stamp: jiff::Timestamp = format!("{date}T{time}:00Z").parse().ok()?;
    Some(Timestamp(u64::try_from(stamp.as_second()).ok()?))
}

impl Refusal {
    /// When the API may be tried again, seen from `now`
    ///
    /// The resume time when it is after `now`. Otherwise one [`RECHECK_SECS`]
    /// on from `now` for a spend limit, and [`CREDIT_RECHECK_SECS`] for
    /// exhausted credit or a limit whose time could not be read.
    pub fn hold_until(self, now: Timestamp) -> Timestamp {
        match self {
            Self::SpendLimit { resumes: Some(at) } if at > now => at,
            Self::SpendLimit { resumes: Some(_) } => Timestamp(now.0 + RECHECK_SECS),
            Self::SpendLimit { resumes: None } | Self::NoCredit => {
                Timestamp(now.0 + CREDIT_RECHECK_SECS)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp(1_790_000_000);
    // 2026-09-01 00:00:00 UTC
    const SEPT_FIRST: Timestamp = Timestamp(1_788_220_800);

    #[test]
    fn a_spend_limit_message_is_a_refusal_with_its_resume_time() {
        let text = "API Error: 400 {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\
                    \"message\":\"You have reached your specified API usage limits. \
                    You will regain access on 2026-09-01 at 00:00 UTC.\"}}";
        let workspace = "You have reached your specified workspace API usage limits. \
                         You will regain access on 2026-09-01 at 00:00 UTC.";

        for said in [text, workspace] {
            let read = refusal(said);
            assert_eq!(
                read,
                Some(Refusal::SpendLimit {
                    resumes: Some(SEPT_FIRST)
                }),
                "{said}"
            );
            assert_eq!(
                read.unwrap().hold_until(Timestamp(SEPT_FIRST.0 - 5)),
                SEPT_FIRST
            );
        }
    }

    #[test]
    fn a_spend_limit_message_with_no_readable_time_holds_the_credit_interval() {
        for said in [
            "You have reached your specified API usage limits.",
            "You have reached your specified API usage limits. You will regain access soon.",
            "You have reached your specified API usage limits. \
             You will regain access on 2026-13-45 at 99:00 UTC.",
        ] {
            let read = refusal(said).unwrap();
            assert_eq!(read, Refusal::SpendLimit { resumes: None }, "{said}");
            assert_eq!(read.hold_until(NOW), Timestamp(NOW.0 + CREDIT_RECHECK_SECS));
        }
    }

    #[test]
    fn a_resume_time_not_after_now_holds_one_recheck() {
        let limit = Refusal::SpendLimit {
            resumes: Some(SEPT_FIRST),
        };

        assert_eq!(
            limit.hold_until(SEPT_FIRST),
            Timestamp(SEPT_FIRST.0 + RECHECK_SECS)
        );
        let later = Timestamp(SEPT_FIRST.0 + 3600);
        assert_eq!(limit.hold_until(later), Timestamp(later.0 + RECHECK_SECS));
    }

    #[test]
    fn both_credit_wordings_are_a_refusal_with_no_time() {
        for said in [
            "Your credit balance is too low to access the Anthropic API. \
             Please go to Plans & Billing to upgrade or purchase credits.",
            "Credit balance too low · Add funds: https://console.anthropic.com/settings/billing",
        ] {
            let read = refusal(said).unwrap();
            assert_eq!(read, Refusal::NoCredit, "{said}");
            assert_eq!(read.hold_until(NOW), Timestamp(NOW.0 + CREDIT_RECHECK_SECS));
        }
    }

    #[test]
    fn any_other_failure_text_is_no_refusal() {
        for said in [
            "",
            "API Error: 529 overloaded_error",
            "rate_limit_error: you will regain access on 2026-09-01 at 00:00 UTC",
            "You have reached your usage limit for this session",
            "The test suite reached its limits",
        ] {
            assert_eq!(refusal(said), None, "{said}");
        }
    }
}
