//! The project manager's call on the maintainer's held words

use serde_json::Value;

use super::answer::printable;

/// What a `said` entry holds, as a dropped one names it
pub(super) const SAID: &str = "a `thread` number, a `call` of park, send-back, settle or none, \
                               and a `note` for send-back and a `why`";

/// The project manager's call on the maintainer's words on one thread
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct SaidCall {
    /// The pull request or issue the words were written on
    pub(in crate::runner) thread: u64,
    /// What to do
    pub(in crate::runner) call: SaidHow,
    /// What the worker is told, for a send-back
    pub(in crate::runner) note: String,
    /// Why, which the thread and the journal carry
    pub(in crate::runner) why: String,
}

/// What the project manager does about the maintainer's words
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum SaidHow {
    /// The work item waits on the maintainer, who is told why
    Park,
    /// The worker is sent the note
    SendBack,
    /// The review bot threads that hold a merge ruling are accepted
    Settle,
    /// No call: the maintainer is told so
    Nothing,
}

impl SaidHow {
    /// The call as the answer spells it
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Park => "park",
            Self::SendBack => "send-back",
            Self::Settle => "settle",
            Self::Nothing => "none",
        }
    }
}

/// What kelpie carries out at the next comment read for a comment the
/// project manager called on, with no ruling waiting
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) struct Released {
    /// [`SaidHow::SendBack`] or [`SaidHow::Park`]
    pub(in crate::runner) call: SaidHow,
    /// What the worker is told, for a send-back
    pub(in crate::runner) note: String,
    /// Why the project manager made the call
    pub(in crate::runner) why: String,
}

pub(super) fn said(value: &Value) -> Option<SaidCall> {
    let thread = match value.get("thread")? {
        Value::Number(n) => n.as_u64()?,
        Value::String(text) => text.trim().trim_start_matches('#').parse().ok()?,
        _ => return None,
    };
    let call = match value.get("call")?.as_str()?.trim() {
        "park" => SaidHow::Park,
        "send-back" => SaidHow::SendBack,
        "settle" => SaidHow::Settle,
        "none" => SaidHow::Nothing,
        _ => return None,
    };
    let words = |key: &str| match value.get(key) {
        None | Some(Value::Null) => Some(String::new()),
        Some(Value::String(text)) => Some(printable(text.trim())),
        Some(_) => None,
    };
    Some(SaidCall {
        thread,
        call,
        note: words("note")?,
        why: words("why")?,
    })
}
