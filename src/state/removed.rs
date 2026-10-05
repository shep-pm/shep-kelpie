//! What removed features left in a state file saved before they went
//!
//! Such a file still loads. The relay's, the planning call's and the
//! whole-issue check's fields are dropped before reading, and the check's time
//! and calls count as review's, so totals still add up. A pending ruling of a
//! removed kind has nothing here to answer it, so the file is refused.

use serde_json::{Map, Value};

// The kinds of ruling removed features raised, which nothing answers now
const RULINGS: [&str; 4] = ["split", "split-stuck", "close-stuck", "audit"];

/// The first pending ruling of a removed kind, by id and kind
pub(super) fn removed_ruling(value: &Value) -> Option<(u64, String)> {
    let rulings = value.get("rulings")?.as_array()?;
    rulings.iter().find_map(|ruling| {
        let kind = ruling.get("kind")?.get("kind")?.as_str()?;
        let id = ruling.get("id")?.as_u64()?;
        RULINGS.contains(&kind).then(|| (id, kind.to_owned()))
    })
}

/// Drops what removed features saved, and moves the whole-issue check's time
/// and calls to review's
///
/// Only these names are touched; any other unknown field is still refused.
pub(super) fn drop_removed_fields(value: &mut Value) {
    let Some(state) = value.as_object_mut() else {
        return;
    };
    state.remove("relay_clears");
    state.remove("plans");
    for ruling in objects(state.get_mut("rulings")) {
        ruling.remove("relayed");
        ruling.remove("resend");
    }
    for item in objects(state.get_mut("work_items")) {
        drop_audit(item);
    }
    for finished in objects(state.get_mut("history")) {
        fold_audit_seconds(finished);
    }
}

fn objects(list: Option<&mut Value>) -> impl Iterator<Item = &mut Map<String, Value>> {
    let list = list.and_then(Value::as_array_mut).into_iter().flatten();
    list.filter_map(Value::as_object_mut)
}

// The check was a fresh Claude session reviewing the work, so its time is a
// Claude round's and its calls a reviewer's.
fn drop_audit(item: &mut Map<String, Value>) {
    item.remove("audit");
    if let Some(timings) = item.get_mut("timings").and_then(Value::as_object_mut) {
        fold_audit_seconds(timings);
        if timings.get("call").and_then(Value::as_str) == Some("audit") {
            timings.insert("call".to_owned(), "claude".into());
        }
    }
    for call in objects(item.get_mut("calls")) {
        if call.get("role").and_then(Value::as_str) == Some("auditor") {
            call.insert("role".to_owned(), "reviewer".into());
        }
    }
}

fn fold_audit_seconds(holder: &mut Map<String, Value>) {
    let Some(seconds) = holder.get_mut("seconds").and_then(Value::as_object_mut) else {
        return;
    };
    let audit = seconds.get("audit").and_then(Value::as_u64);
    let round = seconds.get("claude_round").map_or(Some(0), Value::as_u64);
    // Anything that is not a count is left for the parser to refuse.
    let (Some(audit), Some(round)) = (audit, round) else {
        return;
    };
    seconds.remove("audit");
    seconds.insert(
        "claude_round".to_owned(),
        audit.saturating_add(round).into(),
    );
}

#[cfg(test)]
mod tests;
