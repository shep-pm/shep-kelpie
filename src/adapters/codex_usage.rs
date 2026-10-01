//! The Codex account's usage, read from `codex app-server`
//!
//! Its `account/rateLimits/read` request answers with the account's two
//! windows and runs no model turn. The app server exits with no answer
//! when its stdin closes first, so the requests go in and the pipe stays
//! open until the answer comes back.
//!
//! The shape is the app server's generated JSON schema: a successful
//! answer has not been read from a live account yet.
//!
//! It reads kelpie's own login, in `codex_home`, never the maintainer's
//! `~/.codex`. It runs outside the sandbox, so a login near its expiry is
//! refreshed here, where the refreshed one can be saved.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use super::claude::ClaudeCli;
use super::process::{Processes, RunError};
use crate::ports::{Meter, MeterError, Timestamp, Utilization, Window};

/// How long the app server gets to answer. A refusal took about 5 seconds.
const LIMIT: Duration = Duration::from_secs(30);

/// The request id of the read, which its answer carries
const READ_ID: u64 = 2;

/// The handshake and the read, one JSON-RPC message per line
const REQUESTS: &str = concat!(
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"kelpie","version":"0"}}}"#,
    "\n",
    r#"{"jsonrpc":"2.0","method":"initialized"}"#,
    "\n",
    r#"{"jsonrpc":"2.0","id":2,"method":"account/rateLimits/read"}"#,
    "\n",
);

const SESSION_MINS: u64 = 300;
const WEEK_MINS: u64 = 7 * 24 * 60;

// A reset further out than this from now is not a time kelpie can trust,
// such as one in milliseconds where seconds were meant.
const FURTHEST_SECS: u64 = 8 * 24 * 3600;

/// Reads the Codex account's usage through `codex`, one process per read
#[derive(Debug, Clone)]
pub struct CodexMeter {
    processes: Processes,
    codex_home: PathBuf,
}

impl ClaudeCli {
    /// A Codex meter on the login in `codex_home`, whose reads end with this
    /// one's calls when the runner stops
    pub fn codex_meter(&self, codex_home: PathBuf) -> CodexMeter {
        CodexMeter {
            processes: self.processes.clone(),
            codex_home,
        }
    }
}

impl Meter for CodexMeter {
    fn read(&self, now: Timestamp) -> Result<Utilization, MeterError> {
        let answer = self
            .processes
            .answer_within(
                Command::new("codex")
                    .arg("app-server")
                    .env("CODEX_HOME", &self.codex_home),
                REQUESTS,
                LIMIT,
                &answers_read,
            )
            .map_err(|e| {
                MeterError::Codex(match e {
                    RunError::Io(e) => format!("cannot run codex: {e}"),
                    RunError::Stopped => "codex was stopped with the runner".into(),
                    RunError::TimedOut => "codex did not answer in time".into(),
                })
            })?;
        let answer =
            answer.ok_or_else(|| MeterError::Codex("codex exited with no answer".into()))?;
        parse(&answer, now)
    }
}

// Whether `line` is the answer to the read, by its id.
fn answers_read(line: &str) -> bool {
    #[derive(Deserialize)]
    struct Id {
        id: Option<u64>,
    }
    serde_json::from_str::<Id>(line).is_ok_and(|m| m.id == Some(READ_ID))
}

/// Reads the account's two windows out of the app server's answer
pub(super) fn parse(answer: &str, now: Timestamp) -> Result<Utilization, MeterError> {
    #[derive(Deserialize)]
    struct Answer {
        result: Option<Read>,
        error: Option<Refusal>,
    }
    #[derive(Deserialize)]
    struct Refusal {
        message: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Read {
        rate_limits: Snapshot,
    }
    #[derive(Deserialize)]
    struct Snapshot {
        primary: Option<Limit>,
        secondary: Option<Limit>,
    }

    let unreadable = |why: &str| MeterError::Codex(format!("unreadable codex rate limits: {why}"));
    let answer: Answer = serde_json::from_str(answer).map_err(|e| unreadable(&e.to_string()))?;
    if let Some(refusal) = answer.error {
        // The message carries the backend's JSON body, over several lines.
        let message: Vec<&str> = refusal.message.split_whitespace().collect();
        return Err(MeterError::Codex(format!(
            "codex refused: {}",
            message.join(" ")
        )));
    }
    let limits = answer
        .result
        .ok_or_else(|| unreadable("no result"))?
        .rate_limits;
    let windows = [limits.primary, limits.secondary];
    // Each window says how long it is. One that does not is taken to be in
    // the order the app server lists them: the 5-hour window first.
    let find = |mins: u64, at: usize| {
        let by_length = windows
            .iter()
            .flatten()
            .find(|w| w.window_duration_mins == Some(mins));
        let by_order = windows[at]
            .as_ref()
            .filter(|w| w.window_duration_mins.is_none());
        by_length.or(by_order)
    };
    let window = |mins, at, name| {
        let limit = find(mins, at).ok_or_else(|| unreadable(&format!("no {name} window")))?;
        limit
            .window(now)
            .map_err(|why| unreadable(&format!("the {name} window {why}")))
    };
    Ok(Utilization {
        session: window(SESSION_MINS, 0, "5-hour")?,
        week: window(WEEK_MINS, 1, "weekly")?,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Limit {
    used_percent: i64,
    window_duration_mins: Option<u64>,
    resets_at: Option<i64>,
}

impl Limit {
    fn window(&self, now: Timestamp) -> Result<Window, String> {
        let used_pct = u32::try_from(self.used_percent)
            .map_err(|_| format!("is {}% used", self.used_percent))?;
        let resets_at = self.resets_at.ok_or("has no reset time")?;
        let resets_at = u64::try_from(resets_at)
            .ok()
            .filter(|at| at.abs_diff(now.0) <= FURTHEST_SECS)
            .ok_or_else(|| format!("resets at {resets_at}, not within a week of now"))?;
        Ok(Window {
            used_pct,
            resets_at: Timestamp(resets_at),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Written by hand from `codex app-server generate-json-schema`, on codex-cli 0.146.0.
    const SHAPED: &str = include_str!("../../fixtures/codex-rate-limits.json");
    // Recorded from codex-cli 0.159.3 on kelpie's own login, a Plus plan,
    // with its account id zeroed.
    const PLUS: &str = include_str!("../../fixtures/codex-rate-limits-plus.json");
    // Recorded from codex-cli 0.146.0 on a login whose workspace is deactivated.
    const REFUSED: &str = include_str!("../../fixtures/codex-rate-limits-402.json");

    const NOW: Timestamp = Timestamp(1_790_812_581);

    #[test]
    fn the_schema_shaped_answer_reads_as_both_windows() {
        let usage = parse(SHAPED, NOW).unwrap();
        assert_eq!(
            (usage.session, usage.week),
            (
                Window {
                    used_pct: 23,
                    resets_at: Timestamp(1_790_826_000)
                },
                Window {
                    used_pct: 41,
                    resets_at: Timestamp(1_791_158_400)
                },
            )
        );
    }

    #[test]
    fn a_live_answer_reads_as_both_windows_with_resets_in_seconds() {
        let usage = parse(PLUS, NOW).unwrap();
        assert_eq!(
            (usage.session, usage.week),
            (
                Window {
                    used_pct: 0,
                    resets_at: Timestamp(1_790_884_973)
                },
                Window {
                    used_pct: 1,
                    resets_at: Timestamp(1_791_434_416)
                },
            )
        );
    }

    #[test]
    fn the_recorded_refusal_is_an_error_naming_why() {
        let err = parse(REFUSED, NOW).unwrap_err().to_string();
        assert!(
            err.starts_with("codex refused: failed to fetch codex rate limits"),
            "{err}"
        );
        assert!(err.contains("\"code\": \"deactivated_workspace\""), "{err}");
        assert!(!err.contains('\n'), "one line for the log: {err}");
    }

    #[test]
    fn windows_are_told_apart_by_length_before_order() {
        let swapped = r#"{"id":2,"result":{"rateLimits":{
            "primary":{"usedPercent":70,"windowDurationMins":10080,"resetsAt":1791158400},
            "secondary":{"usedPercent":5,"windowDurationMins":300,"resetsAt":1790826000}}}}"#;
        let usage = parse(swapped, NOW).unwrap();
        assert_eq!((usage.session.used_pct, usage.week.used_pct), (5, 70));

        let unlabelled = r#"{"id":2,"result":{"rateLimits":{
            "primary":{"usedPercent":5,"resetsAt":1790826000},
            "secondary":{"usedPercent":70,"resetsAt":1791158400}}}}"#;
        let usage = parse(unlabelled, NOW).unwrap();
        assert_eq!((usage.session.used_pct, usage.week.used_pct), (5, 70));
    }

    #[test]
    fn a_window_kelpie_cannot_place_is_unreadable() {
        let cases = [
            (
                r#"{"primary":{"usedPercent":5,"windowDurationMins":300,"resetsAt":1790826000}}"#,
                "no weekly window",
            ),
            (
                r#"{"primary":{"usedPercent":5,"windowDurationMins":300},
                    "secondary":{"usedPercent":7,"windowDurationMins":10080,"resetsAt":1791158400}}"#,
                "the 5-hour window has no reset time",
            ),
            (
                r#"{"primary":{"usedPercent":5,"windowDurationMins":300,"resetsAt":1790826000000},
                    "secondary":{"usedPercent":7,"windowDurationMins":10080,"resetsAt":1791158400}}"#,
                "the 5-hour window resets at 1790826000000, not within a week of now",
            ),
            (
                r#"{"primary":{"usedPercent":-1,"windowDurationMins":300,"resetsAt":1790826000},
                    "secondary":{"usedPercent":7,"windowDurationMins":10080,"resetsAt":1791158400}}"#,
                "the 5-hour window is -1% used",
            ),
        ];
        for (limits, why) in cases {
            let answer = format!(r#"{{"id":2,"result":{{"rateLimits":{limits}}}}}"#);
            let err = parse(&answer, NOW).unwrap_err().to_string();
            assert_eq!(err, format!("unreadable codex rate limits: {why}"));
        }
    }

    #[test]
    fn only_the_reads_own_answer_ends_the_wait() {
        assert!(answers_read(SHAPED));
        assert!(answers_read(REFUSED));
        let initialized = r#"{"id":1,"result":{"userAgent":"kelpie/0.146.0"}}"#;
        let notice = r#"{"method":"remoteControl/status/changed","params":{}}"#;
        assert!(!answers_read(initialized));
        assert!(!answers_read(notice));
        assert!(!answers_read("not json"));
    }
}
