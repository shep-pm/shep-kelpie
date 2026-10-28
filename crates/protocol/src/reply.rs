//! What a runner answers a request with

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use shep_kelpie_core::ports::SessionId;
use shep_kelpie_core::terminal::Foreground;
use shep_kelpie_core::ticket::TicketId;
use shep_kelpie_core::work_item::Seconds;

use crate::request::Request;
use crate::status::Status;

/// What a runner answers a request with
///
/// Each answer serialises as the bare body `shep trigger` returns today: the
/// answer itself, or `{"error": ...}`. A body carries no tag naming which one
/// it is, so reading one back takes the request that asked.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// The status, which `status`, `add`, `gate` and the rest answer with.
    /// Boxed, since it is far the largest answer and a reply is read once
    Status(Box<Status>),
    /// Where finished work items' time went, which `timings` answers with
    Timings(Totals),
    /// What `attach` answers
    Attaching(Attaching),
    /// What `pm attach` answers
    PmAttaching(PmAttaching),
    /// The board's list, which `board` answers with
    Board(Board),
    /// Why the runner refused, with the text it says why
    Refused(String),
}

impl Reply {
    /// The body `request` asked for, read back
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] when the body is not JSON, or is not the answer
    /// this request gets.
    pub fn decode(request: &Request, body: &str) -> Result<Reply, serde_json::Error> {
        let shown: Value = serde_json::from_str(body)?;
        if let Some(why) = refused(&shown) {
            return Ok(Self::Refused(why));
        }
        Ok(match request {
            Request::Timings(_) => Self::Timings(read(&shown)?),
            Request::Attach(..) => Self::Attaching(read(&shown)?),
            Request::PmAttach(..) => Self::PmAttaching(read(&shown)?),
            Request::Board(_) => Self::Board(read(&shown)?),
            _ => Self::Status(Box::new(read(&shown)?)),
        })
    }
}

/// A line a runner pushes after a `watch`, which `Reply` is not: a reply
/// answers a request, an event comes unasked
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "status", rename_all = "snake_case")]
pub enum Event {
    /// The whole status, now or after a change. Boxed for the reason
    /// [`Reply::Status`] is
    Status(Box<Status>),
}

impl Serialize for Reply {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Status(status) => status.serialize(serializer),
            Self::Timings(totals) => totals.serialize(serializer),
            Self::Attaching(attaching) => attaching.serialize(serializer),
            Self::PmAttaching(attaching) => attaching.serialize(serializer),
            Self::Board(board) => board.serialize(serializer),
            Self::Refused(why) => serde_json::json!({ "error": why }).serialize(serializer),
        }
    }
}

// The refusal's text, where a body is only `{"error": ...}`.
fn refused(shown: &Value) -> Option<String> {
    let fields = shown.as_object()?;
    if fields.len() != 1 {
        return None;
    }
    fields
        .get("error")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

// `shown` as the answer a request gets.
fn read<T: DeserializeOwned>(shown: &Value) -> Result<T, serde_json::Error> {
    T::deserialize(shown)
}

/// What `timings` answers
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    /// The project
    pub project: String,
    /// How many finished work items the totals cover
    pub items: usize,
    /// Their tickets, oldest first
    pub issues: Vec<TicketId>,
    /// Their wall time together
    pub wall: u64,
    /// Every phase together, summing to `wall`
    pub seconds: Seconds,
    /// The same as a plain-text table
    pub table: String,
}

/// What `board` answers
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    /// The project
    pub project: String,
    /// The ready issues in the order the board takes them, first to go first
    pub board: Vec<Place>,
}

/// An issue's place in the board's list
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    /// Its place in the list, from 1
    pub position: usize,
    /// Its ticket
    pub issue: TicketId,
    /// Its title
    pub title: String,
    /// Its priority, from `P0` to `P4`, or null with none
    pub priority: Option<String>,
    /// Whether an open issue blocks it, so the board passes it over
    pub blocked: bool,
    /// Whether the project manager holds it back until an open work item closes
    pub held: bool,
}

/// What `attach` answers
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "attach", rename_all = "lowercase", deny_unknown_fields)]
pub enum Attaching {
    /// The work item is held, and a call for it is still in flight
    Waiting {
        /// The ticket it is for
        issue: TicketId,
    },
    /// The work item is held, and its worker's session is ready to run
    Ready {
        /// The ticket it is for
        issue: TicketId,
        /// The worker's session
        session: SessionId,
        /// Its worktree
        worktree: PathBuf,
        /// The session, resumed in the worker's settings and sandbox
        command: Foreground,
    },
    /// The work item is held while the session's process runs too
    Running {
        /// The ticket it is for
        issue: TicketId,
    },
}

/// What `pm attach` answers
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "attach", rename_all = "lowercase", deny_unknown_fields)]
pub enum PmAttaching {
    /// The project manager is held, and a wake of its is still in flight
    Waiting,
    /// The project manager is held, and its session is ready to run
    Ready {
        /// Its session, which a wake has started or this starts
        session: SessionId,
        /// Its folder
        folder: PathBuf,
        /// The session in its settings and sandbox
        command: Foreground,
    },
    /// It is held while the session's process runs too
    Running,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn recorded() -> Status {
        crate::status::tests::recorded_status()
    }

    #[test]
    fn a_status_event_names_itself_and_round_trips() {
        let event = Event::Status(Box::new(recorded()));
        let shown = serde_json::to_value(&event).unwrap();
        assert_eq!(shown["event"], "status");
        assert_eq!(shown["status"]["project"], recorded().project);
        assert_eq!(serde_json::from_value::<Event>(shown).unwrap(), event);
    }

    #[test]
    fn a_status_is_not_an_event() {
        let bare = serde_json::to_value(recorded()).unwrap();
        assert!(serde_json::from_value::<Event>(bare).is_err());
        assert!(serde_json::from_value::<Event>(json!({ "event": "other" })).is_err());
    }
}
