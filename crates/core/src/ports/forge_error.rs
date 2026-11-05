//! Why a forge call failed

use std::fmt;

use super::{Timestamp, rate_limit};
use crate::hooks::Refusal;
use crate::local_paths::Leak;
use crate::ticket::TicketId;

/// The longest, in seconds, a rate-limited answer's own wait is believed
const LONGEST_WAIT: u64 = 60 * 60;

/// When a rate-limited answer says to ask again
///
/// A secondary limit says how long to wait in `Retry-After`; a primary one
/// says when it resets in `X-RateLimit-Reset`. Either may be missing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cooldown {
    /// Seconds to wait from the answer, from `Retry-After`
    pub retry_after: Option<u64>,
    /// When the limit resets, from `X-RateLimit-Reset`
    pub reset: Option<Timestamp>,
}

impl Cooldown {
    /// What the two headers' values say: each a whole number, anything else
    /// (an HTTP date in `Retry-After`, an empty value) read as missing
    pub fn read(retry_after: &str, reset: &str) -> Self {
        let number = |text: &str| text.trim().parse::<u64>().ok();
        Self {
            retry_after: number(retry_after),
            reset: number(reset).map(Timestamp),
        }
    }

    /// Until when to hold every call, if the answer said: `Retry-After` from
    /// `now` when it gave one, else the reset when that is still ahead
    ///
    /// A wait is at least a second and at most an hour.
    pub fn until(self, now: Timestamp) -> Option<Timestamp> {
        if let Some(wait) = self.retry_after {
            return Some(Timestamp(now.0.saturating_add(wait.clamp(1, LONGEST_WAIT))));
        }
        let longest = Timestamp(now.0.saturating_add(LONGEST_WAIT));
        self.reset
            .filter(|reset| *reset > now)
            .map(|reset| reset.min(longest))
    }
}

/// Why a forge call failed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeError {
    /// The forge's command line tool could not be started, with the OS's reason
    Spawn(String),
    /// The tool ran and exited unsuccessfully, with this on stderr
    Failed(String),
    /// The tool succeeded but its output was not what was asked for
    Unreadable(String),
    /// A field of the post names something private to this machine, so kelpie never sent it
    LocalPath {
        /// The field, such as "the comment"
        what: &'static str,
        /// What it names
        leak: Leak,
    },
    /// A project's hook refused a field of the post, or failed to run, so
    /// kelpie never sent it
    Hook {
        /// The field, such as "the comment"
        what: &'static str,
        /// The hook, and what it said
        refusal: Refusal,
    },
    /// The forge said its rate limit was used up, so no call is made until it resets then
    Held(Timestamp),
    /// The forge answered that a rate limit is used up, saying so in `said` and
    /// when to ask again in `cooldown`
    Limited {
        /// What to tell the log
        said: String,
        /// When the answer says to ask again
        cooldown: Cooldown,
    },
    /// The GitHub App's write failed, with why
    App(String),
    /// The GitHub App's installation lacks a permission the write needs, with
    /// what GitHub said: asking again changes nothing until the maintainer
    /// accepts it
    NoPermission(String),
    /// The GitHub App could not be asked just now, such as with GitHub out of
    /// reach, busy or rate limited, with why: asking again later may work
    AppBusy(String),
    /// The ticket is not one this source holds, such as a local ticket asked
    /// of a project whose tickets are GitHub issues
    NoIssue {
        /// The ticket it was asked for
        ticket: TicketId,
    },
}

impl ForgeError {
    /// Whether asking again later may succeed with nothing changed
    ///
    /// A failed call passes when `gh` reported the forge's own answer of 5xx or
    /// worse, which it prints as `HTTP 502: Bad Gateway (https://…)` or at the
    /// end of its own line, `(HTTP 502)`. A write that answered so may have
    /// landed all the same, so only a read is asked again on this.
    pub fn passes(&self) -> bool {
        match self {
            Self::Failed(said) => printed_status(said).is_some_and(|status| status >= 500),
            Self::AppBusy(_) | Self::Limited { .. } | Self::Held(_) => true,
            Self::Spawn(_)
            | Self::Unreadable(_)
            | Self::LocalPath { .. }
            | Self::Hook { .. }
            | Self::App(_)
            | Self::NoPermission(_)
            | Self::NoIssue { .. } => false,
        }
    }
}

// The status `gh` printed for a failed call's answer, of which the last is the
// one its own line names
fn printed_status(said: &str) -> Option<u16> {
    (said.match_indices("HTTP ").filter_map(|(at, mark)| {
        let (first, last) = (at + mark.len(), at + mark.len() + 3);
        let code = said.get(first..last)?;
        code.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| code.parse().ok())?
    }))
    .last()
}

impl fmt::Display for ForgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "cannot run gh: {error}"),
            Self::Failed(stderr) => write!(f, "gh failed: {}", stderr.trim()),
            Self::Unreadable(output) => write!(f, "unreadable gh output: {}", output.trim()),
            Self::LocalPath { what, leak } => write!(f, "not posted: {what} names {leak}"),
            Self::Hook { what, refusal } => write!(f, "not posted: {what}: {refusal}"),
            Self::Held(until) => write!(
                f,
                "not asked: the forge's rate limit is used up until {}",
                rate_limit::time_of(*until)
            ),
            Self::Limited { said, .. } => f.write_str(said),
            Self::App(why) => write!(f, "the GitHub App could not post: {why}"),
            Self::NoPermission(why) => {
                write!(f, "the GitHub App lacks a permission it needs: {why}")
            }
            Self::AppBusy(why) => write!(f, "the GitHub App is not answering: {why}"),
            Self::NoIssue { ticket } => {
                write!(f, "ticket {ticket} is not an issue on this forge")
            }
        }
    }
}

impl core::error::Error for ForgeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_headers_are_read_as_whole_numbers_and_nothing_else() {
        assert_eq!(
            Cooldown::read("60", " 1790000600 "),
            Cooldown {
                retry_after: Some(60),
                reset: Some(Timestamp(1_790_000_600)),
            }
        );
        // An HTTP date, an empty value and a header curl had nothing for.
        for none in ["", "Wed, 21 Oct 2026 07:28:00 GMT", "%header{retry-after}"] {
            assert_eq!(Cooldown::read(none, none), Cooldown::default(), "{none:?}");
        }
    }

    #[test]
    fn a_failure_naming_a_5xx_status_passes_and_a_4xx_does_not() {
        for (said, passes) in [
            (
                "HTTP 502: 502 Bad Gateway (https://api.github.com/graphql)",
                true,
            ),
            ("gh: Bad Gateway (HTTP 502)", true),
            ("gh: Not Found (HTTP 404)", false),
            ("no issue #7", false),
        ] {
            assert_eq!(ForgeError::Failed(said.into()).passes(), passes, "{said}");
        }
        assert!(ForgeError::AppBusy("GitHub is not answering".into()).passes());
        assert!(
            !ForgeError::Unreadable("HTTP 502".into()).passes(),
            "an answer that was not gh's own"
        );
    }

    #[test]
    fn a_retry_after_wins_over_a_reset_and_both_are_bounded() {
        let now = Timestamp(1_000);
        let at = |retry_after, reset| Cooldown { retry_after, reset }.until(now);
        assert_eq!(at(Some(90), Some(Timestamp(5_000))), Some(Timestamp(1_090)));
        assert_eq!(
            at(Some(0), None),
            Some(Timestamp(1_001)),
            "at least a second"
        );
        assert_eq!(
            at(Some(86_400), None),
            Some(Timestamp(4_600)),
            "at most an hour"
        );
        assert_eq!(at(None, Some(Timestamp(1_500))), Some(Timestamp(1_500)));
        assert_eq!(at(None, Some(Timestamp(900))), None, "a reset gone by");
        assert_eq!(at(None, Some(Timestamp(99_999))), Some(Timestamp(4_600)));
        assert_eq!(at(None, None), None);
    }
}
