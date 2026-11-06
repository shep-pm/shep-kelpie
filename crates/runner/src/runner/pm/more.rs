//! The project manager's call on one more read of a pull request

use serde_json::Value;

use super::act::{WORDS, plain};
use super::answer::{printable, ticket};
use crate::board::briefing::ticket_ref;
use crate::runner::Runner;
use crate::state::StateError;
use crate::ticket::TicketId;
use crate::work_item::ReviewPlan;

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

impl Runner {
    // What one `read` call did, or why it was dropped. A call that says no
    // does nothing: the pass ends as the answer's end clears the ask.
    pub(super) fn settle_read(
        &mut self,
        call: &ReadCall,
    ) -> Result<Result<String, String>, StateError> {
        let named = ticket_ref(call.item);
        if !self.pm.reading.contains(&call.item) {
            return Ok(Err(format!("read {named}: no read of it waits on you")));
        }
        let why = plain(&self.shown(call.why.clone()), WORDS);
        let because = match why.is_empty() {
            true => String::new(),
            false => format!(": {why}"),
        };
        if !call.more {
            return Ok(Ok(format!("ended {named}'s reads{because}")));
        }
        let Some(item) = self.state.item(call.item) else {
            return Ok(Err(format!("read {named}: it is no open work item")));
        };
        let reads = self.need_of(item).saturating_add(1);
        let about = item.review_plan.as_ref().and_then(|p| p.about.clone());
        let mut next = self.state.clone();
        if let (Some(item), Some(reads)) =
            (next.item_mut(call.item), std::num::NonZeroU32::new(reads))
        {
            item.review_plan = Some(ReviewPlan { reads, about });
        }
        self.save(next)?;
        Ok(Ok(format!(
            "let one more reviewer read {named}, which makes {reads} reads{because}"
        )))
    }
}
