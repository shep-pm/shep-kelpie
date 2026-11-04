//! What `goose run --output-format stream-json` prints, read as a call's reply
//!
//! One JSON object a line, tagged `type`: `message`, `notification`, `error`
//! and `complete`. goose also prints its banner on stdout before the first, so
//! a line is read only when it starts with `{`.

use std::process::Output;

use serde::{Deserialize, Serialize};

use super::GOOSE;
use crate::adapters::harness::tail;
use crate::ports::{AgentError, AgentReply, Session, Usage};

/// What goose says in place of a reply when it ran out of turns, and exits 0
const TURN_LIMIT: &str = "I've reached the maximum number of actions";

/// How goose leads the message it prints when its model host answers no request
const NETWORK_ERROR: &str = "Network error:";

/// What goose says when `--resume` names a session it does not have
const NO_SESSION: &str = "No session found with name";

/// A goose session's tokens so far, as its last `complete` event reported them
///
/// goose reports the session's running totals, not the call's own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Spent {
    /// Every input token, cached or not
    pub(super) input_tokens: u64,
    /// Every output token
    pub(super) output_tokens: u64,
    /// The input tokens read from the cache
    pub(super) cache_read_input_tokens: u64,
    /// The input tokens written to the cache
    pub(super) cache_write_input_tokens: u64,
    /// Whether a terminal session has spent tokens since these were kept, so the next
    /// call's totals hold more than its own and its usage cannot be told
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) rebase: bool,
}

impl Spent {
    /// What was spent after `before`, as kelpie counts a call's tokens
    ///
    /// The cached input is counted within the input, as OpenAI counts it.
    fn since(self, before: Self) -> Usage {
        let less = |now: u64, then: u64| now.saturating_sub(then);
        let cache_read = less(self.cache_read_input_tokens, before.cache_read_input_tokens);
        let cache_write = less(
            self.cache_write_input_tokens,
            before.cache_write_input_tokens,
        );
        Usage {
            input: less(self.input_tokens, before.input_tokens)
                .saturating_sub(cache_read)
                .saturating_sub(cache_write),
            cache_write,
            cache_write_5m: 0,
            cache_read,
            output: less(self.output_tokens, before.output_tokens),
        }
    }
}

/// A call's reply or why it has none, and the session's totals if it printed them
pub(super) struct Read {
    pub(super) reply: Result<AgentReply, AgentError>,
    /// The session's totals after the call, to keep whatever the reply is
    pub(super) spent: Option<Spent>,
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_write_input_tokens: Option<u64>,
}

impl Line {
    // A `complete` event leaves out a count it has none of.
    fn totals(&self) -> Spent {
        Spent {
            input_tokens: self.input_tokens.unwrap_or_default(),
            output_tokens: self.output_tokens.unwrap_or_default(),
            cache_read_input_tokens: self.cache_read_input_tokens.unwrap_or_default(),
            cache_write_input_tokens: self.cache_write_input_tokens.unwrap_or_default(),
            rebase: false,
        }
    }
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    id: Option<String>,
    role: String,
    #[serde(default)]
    content: Vec<Content>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
}

impl Message {
    fn text(&self) -> String {
        let blocks = self.content.iter().filter(|c| c.kind == "text");
        blocks.filter_map(|c| c.text.as_deref()).collect()
    }
}

/// The reply of a call `output` is the result of, which began with `before` spent
pub(super) fn read(output: &Output, session: &Session, before: Spent) -> Read {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The last assistant message: its id, and its text so far.
    let mut last: Option<(Option<String>, String)> = None;
    let mut complete = None;
    let mut error = None;
    // Whether the latest message to print could not be read, so `last` is stale.
    let mut unread = false;
    for text in stdout.lines().filter(|l| l.starts_with('{')) {
        let Ok(line) = serde_json::from_str::<Line>(text) else {
            unread |= text.contains(r#""type":"message""#);
            continue;
        };
        match line.kind.as_str() {
            "message" => match line.message {
                Some(message) if message.role == "assistant" => {
                    unread = false;
                    // goose streams a reply as several events that share one id,
                    // each carrying a piece of its text.
                    match &mut last {
                        Some((id, text)) if id.is_some() && *id == message.id => {
                            text.push_str(&message.text());
                        }
                        _ => last = Some((message.id.clone(), message.text())),
                    }
                }
                _ => {}
            },
            "complete" => complete = Some(line.totals()),
            "error" => error = line.error,
            _ => {}
        }
    }
    let reply = (|| {
        let asked = session.id();
        if !output.status.success() {
            if matches!(session, Session::Resume(_)) && stderr.contains(NO_SESSION) {
                return Err(AgentError::NoSession(GOOSE, asked.clone()));
            }
            let why = error.as_deref().unwrap_or(tail(&stderr));
            return Err(failed(why));
        }
        let (Some(last), false) = (last, unread) else {
            return Err(match error {
                Some(why) => failed(&why),
                None => AgentError::Unreadable(GOOSE, tail(&stdout).to_owned()),
            });
        };
        let (_, text) = last;
        if text.starts_with(TURN_LIMIT) {
            return Err(AgentError::Failed(
                GOOSE,
                format!("goose hit its turn limit: {}", tail(&text)),
            ));
        }
        // goose reports a provider's failure as its reply, exits 0 and counts no
        // tokens, so a reply that spent some is not taken for one.
        let counted = complete.unwrap_or(before).since(before);
        let spent_none = before.rebase || counted == Usage::default();
        if text.starts_with(NETWORK_ERROR) && spent_none {
            return Err(failed(&text));
        }
        // After a terminal session the totals hold its tokens too, which kelpie cannot read
        // back from goose's database, so this call is charged none.
        let usage = if before.rebase {
            Usage::default()
        } else {
            counted
        };
        Ok(AgentReply {
            session_id: asked.clone(),
            text,
            usage,
            session_cost: None,
            context: None,
            compactions: 0,
            metered: None,
        })
    })();
    // Totals only grow, so ones below what the session had are not its own.
    let spent = complete.filter(|s| {
        s.input_tokens >= before.input_tokens && s.output_tokens >= before.output_tokens
    });
    Read { reply, spent }
}

// An error the model host did not answer is the host's, and passes. Any other
// is the call's own.
pub(super) fn failed(detail: &str) -> AgentError {
    // Read whole: the sign may come before the end the error keeps.
    let unreachable = reaches_no_host(detail);
    let detail = tail(detail).to_owned();
    match unreachable {
        true => AgentError::Unreachable(GOOSE, detail),
        false => AgentError::Failed(GOOSE, detail),
    }
}

// Whether goose's error says its model host could not be reached: a connection
// refused, reset or never made, or a 502 or 503 leading the text or named by it.
// An error that leads with another status is the call's own, whatever its text
// goes on to mention.
fn reaches_no_host(detail: &str) -> bool {
    let lower = detail.trim_start().to_ascii_lowercase();
    let digits = lower.bytes().take_while(u8::is_ascii_digit).count();
    // Three digits then a letter, or a dot and a digit, are a word or an address.
    let rest = &lower[digits..];
    let address = rest
        .strip_prefix('.')
        .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()));
    let leads_with_status =
        digits == 3 && !address && !rest.starts_with(|c: char| c.is_ascii_alphanumeric());
    if leads_with_status {
        return matches!(&lower[..digits], "502" | "503");
    }
    [
        "could not connect",
        "connection refused",
        "connection reset",
        "econnrefused",
        "econnreset",
        "bad gateway",
        "service unavailable",
    ]
    .iter()
    .any(|sign| lower.contains(sign))
}
