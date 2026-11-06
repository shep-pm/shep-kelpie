//! The project manager's call on one more read of a pull request

use serde_json::Value;

use super::answer::{printable, ticket};
use crate::ticket::TicketId;

/// What a `read` entry holds, as a dropped one names it
pub(super) const MORE: &str = "an `item`, `more` as true or false, and a `why`";

/// The project manager's call on one work item whose round sent a finding
/// above a nit
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReadCall {
    /// The work item's ticket
    pub(super) item: TicketId,
    /// Whether one more reviewer reads it, or its reads end
    pub(super) more: bool,
    /// Why, which the journal carries
    pub(super) why: String,
}

pub(super) fn read_call(value: &Value) -> Option<ReadCall> {
    let item = ticket(value.get("item")?)?;
    let more = value.get("more")?.as_bool()?;
    let why = match value.get("why") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => printable(text.trim()),
        Some(_) => return None,
    };
    Some(ReadCall { item, more, why })
}
