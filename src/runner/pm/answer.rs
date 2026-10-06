//! The project manager's answer: the last JSON object in its reply
//!
//! Each field is read on its own, so a malformed one is dropped and named
//! while the rest still count. Nothing here knows the board: checking an
//! answer against it is the runner's.

use serde_json::{Map, Value};

/// The keys an answer takes, an object with none of them being none: `why`
/// is left out, since an unstick inside an answer has one of its own
const KEYS: [&str; 4] = ["pick", "hold", "unstick", "reply"];

/// The most of a reply or a why kept, in characters
const LONGEST: usize = 2_000;

/// What the project manager answered, field by field
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Answer {
    /// The ready issue to start next, or none to start nothing now
    pub(super) pick: Option<u64>,
    /// The ready issues to hold until an open work item closes
    pub(super) hold: Vec<u64>,
    /// What to do about one stuck work item
    pub(super) unstick: Option<Unstick>,
    /// A message for the maintainer
    pub(super) reply: Option<String>,
    /// Why, in its own words
    pub(super) why: Option<String>,
    /// What could not be read, each named, to log
    pub(super) unread: Vec<String>,
}

/// The project manager's call on one stuck work item
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Unstick {
    /// The work item's issue
    pub(super) item: u64,
    /// What to do
    pub(super) action: Action,
    /// Why, which a ruling it raises carries
    pub(super) why: String,
}

/// What the project manager does about a stuck work item
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    /// Its turn runs again as it is
    Retry,
    /// The maintainer is asked to change what it asks
    ReScope,
    /// The maintainer decides
    Ask,
    /// Left to resolve itself
    Leave,
}

impl Action {
    /// The action as the answer spells it
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::ReScope => "re-scope",
            Self::Ask => "ask",
            Self::Leave => "none",
        }
    }
}

/// The answer in `text`: its last JSON object with any of the answer's keys
pub(super) fn read(text: &str) -> Option<Answer> {
    let fields = last_object(text)?;
    let mut answer = Answer::default();
    for (key, value) in fields {
        let unread = |what: &str| format!("`{key}`: {what}, not {value}");
        match key.as_str() {
            "pick" => match &value {
                Value::Null => {}
                value => match number(value) {
                    Some(n) => answer.pick = Some(n),
                    None => answer.unread.push(unread("an issue number or null")),
                },
            },
            "hold" => match &value {
                Value::Null => {}
                Value::Array(items) => {
                    for item in items {
                        match number(item) {
                            Some(n) => answer.hold.push(n),
                            None => answer
                                .unread
                                .push(format!("`hold`: an issue number, not {item}")),
                        }
                    }
                }
                _ => answer.unread.push(unread("a list of issue numbers")),
            },
            "unstick" => match &value {
                Value::Null => {}
                value => match unstick(value) {
                    Some(u) => answer.unstick = Some(u),
                    None => answer.unread.push(unread(
                        "`item`, an `action` of retry, re-scope, ask or none, and `why`",
                    )),
                },
            },
            "reply" | "why" => match value {
                Value::Null => {}
                Value::String(text) if text.trim().is_empty() => {}
                Value::String(text) => {
                    let text = Some(printable(text.trim()));
                    match key.as_str() {
                        "reply" => answer.reply = text,
                        _ => answer.why = text,
                    }
                }
                _ => answer.unread.push(unread("text or null")),
            },
            _ => answer
                .unread
                .push(format!("`{key}` is not a field kelpie acts on")),
        }
    }
    Some(answer)
}

// The last top-level object in `text` that has an answer's key. Read from
// the start, each whole JSON value is skipped past, so nothing inside one,
// a string's braces included, is ever taken for an answer of its own.
fn last_object(text: &str) -> Option<Map<String, Value>> {
    let mut found = None;
    let mut at = 0;
    while let Some(offset) = text[at..].find('{') {
        let start = at + offset;
        let mut values = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match values.next() {
            Some(Ok(value)) => {
                at = start + values.byte_offset();
                if let Value::Object(fields) = value
                    && KEYS.iter().any(|k| fields.contains_key(*k))
                {
                    found = Some(fields);
                }
            }
            _ => at = start + 1,
        }
    }
    found
}

// `text` with every control character but a newline taken out, cut past
// [`LONGEST`] characters, since it is printed and posted as it is.
fn printable(text: &str) -> String {
    let kept: Vec<char> = (text.chars())
        .filter(|&c| c == '\n' || !c.is_control())
        .collect();
    let mut out: String = kept.iter().take(LONGEST).collect();
    if kept.len() > LONGEST {
        out.push_str(" …");
    }
    out
}

// A positive whole number, as a JSON number or digits in a string.
fn number(value: &Value) -> Option<u64> {
    let n = match value {
        Value::Number(n) => n.as_u64()?,
        Value::String(text) => text.trim().trim_start_matches('#').parse().ok()?,
        _ => return None,
    };
    (n > 0).then_some(n)
}

fn unstick(value: &Value) -> Option<Unstick> {
    let item = number(value.get("item")?)?;
    let action = match value.get("action")?.as_str()? {
        "retry" => Action::Retry,
        "re-scope" => Action::ReScope,
        "ask" => Action::Ask,
        "none" => Action::Leave,
        _ => return None,
    };
    let why = value.get("why").and_then(Value::as_str).unwrap_or("");
    Some(Unstick {
        item,
        action,
        why: why.trim().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_object_after_the_reasoning_is_the_answer() {
        let text = "I read the board. {\"pick\": 3} was my first thought.\n\n```json\n\
                    {\"pick\": 12, \"hold\": [4, \"#9\"], \"unstick\": {\"item\": 7, \
                    \"action\": \"re-scope\", \"why\": \"split the parser out\"}, \
                    \"reply\": null, \"why\": \"#12 touches nothing open.\"}\n```\n";
        let answer = read(text).unwrap();
        assert_eq!(
            answer,
            Answer {
                pick: Some(12),
                hold: vec![4, 9],
                unstick: Some(Unstick {
                    item: 7,
                    action: Action::ReScope,
                    why: "split the parser out".into(),
                }),
                reply: None,
                why: Some("#12 touches nothing open.".into()),
                unread: Vec::new(),
            }
        );
    }

    #[test]
    fn braces_inside_the_answers_text_are_not_an_answer_of_their_own() {
        let text = "{\"pick\": 12, \"why\": \"not {\\\"pick\\\": 3} as before\"} {unfinished";
        assert_eq!(read(text).unwrap().pick, Some(12));
        let inner = "{\"reply\": \"{\\\"pick\\\": 3}\", \"pick\": null}";
        assert_eq!(read(inner).unwrap().pick, None);
    }

    #[test]
    fn a_reply_loses_its_control_characters_and_keeps_its_lines() {
        let text = "{\"reply\": \"\\u001b[31mRed\\u0007\\nDone.\"}";
        assert_eq!(read(text).unwrap().reply.as_deref(), Some("[31mRed\nDone."));
    }

    #[test]
    fn a_reply_with_no_answer_object_has_no_answer() {
        assert_eq!(read("I would pick #12."), None);
        assert_eq!(read("{\"item\": 7}"), None);
        assert_eq!(read("{\"pick\": 12"), None);
    }

    #[test]
    fn a_malformed_field_is_named_and_the_rest_still_count() {
        let text = "{\"pick\": \"soon\", \"hold\": [5, true], \"unstick\": {\"item\": 7, \
                    \"action\": \"reboot\"}, \"merge_order\": [71], \"reply\": \"On it.\"}";
        let answer = read(text).unwrap();
        assert_eq!(
            (
                answer.pick,
                &answer.hold,
                &answer.unstick,
                answer.reply.as_deref()
            ),
            (None, &vec![5], &None, Some("On it."))
        );
        assert_eq!(
            answer.unread,
            [
                "`hold`: an issue number, not true",
                "`merge_order` is not a field kelpie acts on",
                "`pick`: an issue number or null, not \"soon\"",
                "`unstick`: `item`, an `action` of retry, re-scope, ask or none, and `why`, \
                 not {\"action\":\"reboot\",\"item\":7}",
            ]
        );
    }
}
