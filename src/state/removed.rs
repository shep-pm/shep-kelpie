//! What removed features left in a state file saved before they went
//!
//! Such a file still loads. The relay's, the planning call's, the
//! whole-issue check's, the review loop's, the deep round's later steps' and
//! the shots' fields are dropped before reading. The check's and the judge's
//! time and calls count as review's, and the shots' time as other, so totals
//! still add up. A review saved mid-loop goes on from the reviewer after the
//! one it last recorded. A pending ruling of a removed kind has nothing here
//! to answer it, so the file is refused.

use serde_json::{Map, Value, json};

// The kinds of ruling removed features raised, which nothing answers now
const RULINGS: [&str; 6] = [
    "split",
    "split-stuck",
    "close-stuck",
    "audit",
    "review-guard",
    "deep-review",
];

// The judge and the check each ran as a fresh Claude session reviewing the
// work, so their time is a Claude round's and their calls a reviewer's. A
// shots run was kelpie's own, between steps, so its time is other.
const PHASES: [(&str, &str); 3] = [
    ("audit", "claude_round"),
    ("judging", "claude_round"),
    ("shots", "other"),
];
const CALLS: [&str; 2] = ["audit", "judge"];
const ROLES: [&str; 2] = ["auditor", "judge"];

/// The first pending ruling of a removed kind, by id and kind
pub(super) fn removed_ruling(value: &Value) -> Option<(u64, String)> {
    let rulings = value.get("rulings")?.as_array()?;
    rulings.iter().find_map(|ruling| {
        let kind = ruling.get("kind")?.get("kind")?.as_str()?;
        let id = ruling.get("id")?.as_u64()?;
        RULINGS.contains(&kind).then(|| (id, kind.to_owned()))
    })
}

/// Drops what removed features saved, and moves the whole-issue check's and
/// the judge's time and calls to review's, and the shots' time to other
///
/// Only these names are touched; any other unknown field is still refused.
pub(super) fn drop_removed_fields(value: &mut Value) {
    let Some(state) = value.as_object_mut() else {
        return;
    };
    state.remove("relay_clears");
    state.remove("plans");
    for notice in objects(state.get_mut("notices")) {
        notice.remove("shots_failed");
    }
    for ruling in objects(state.get_mut("rulings")) {
        ruling.remove("relayed");
        ruling.remove("resend");
        if let Some(kind) = ruling.get_mut("kind") {
            drop_loop(kind);
            if let Some(kind) = kind.as_object_mut() {
                kind.remove("shots_failed");
            }
            // Only the deep round's pin check said why a pushed fix fell short.
            if let Some(kind) = kind.as_object_mut()
                && kind.get("kind").and_then(Value::as_str) == Some("fix-not-pushed")
            {
                kind.remove("why");
            }
        }
    }
    for item in objects(state.get_mut("work_items")) {
        item.remove("audit");
        item.remove("local_rounds");
        item.remove("shots");
        item.remove("shots_comment");
        end_shots_run(item);
        fold_review_calls(item);
        for value in item.values_mut() {
            drop_loop(value);
        }
    }
    for finished in objects(state.get_mut("history")) {
        fold_seconds(finished);
    }
}

fn objects(list: Option<&mut Value>) -> impl Iterator<Item = &mut Map<String, Value>> {
    let list = list.and_then(Value::as_array_mut).into_iter().flatten();
    list.filter_map(Value::as_object_mut)
}

// A shots run in flight never resumes, so it ends as the runner's start ends
// any call cut short, and its time from there counts by the work item's phase.
fn end_shots_run(item: &mut Map<String, Value>) {
    let Some(timings) = item.get_mut("timings").and_then(Value::as_object_mut) else {
        return;
    };
    if timings.get("call").and_then(Value::as_str) != Some("shots") {
        return;
    }
    timings.remove("call");
    timings.remove("queued");
    item.insert("review_call".to_owned(), json!({ "state": "idle" }));
}

fn fold_review_calls(item: &mut Map<String, Value>) {
    if let Some(timings) = item.get_mut("timings").and_then(Value::as_object_mut) {
        fold_seconds(timings);
        if timings
            .get("call")
            .and_then(Value::as_str)
            .is_some_and(|call| CALLS.contains(&call))
        {
            timings.insert("call".to_owned(), "claude".into());
        }
    }
    for call in objects(item.get_mut("calls")) {
        if call
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| ROLES.contains(&role))
        {
            call.insert("role".to_owned(), "reviewer".into());
        }
    }
}

fn fold_seconds(holder: &mut Map<String, Value>) {
    let Some(seconds) = holder.get_mut("seconds").and_then(Value::as_object_mut) else {
        return;
    };
    for (phase, into) in PHASES {
        let from = seconds.get(phase).and_then(Value::as_u64);
        let to = seconds.get(into).map_or(Some(0), Value::as_u64);
        // Anything that is not a count is left for the parser to refuse.
        let (Some(from), Some(to)) = (from, to) else {
            continue;
        };
        seconds.remove(phase);
        seconds.insert(into.to_owned(), from.saturating_add(to).into());
    }
}

// Every review, wherever a phase, a resume or a ruling keeps one, loses the
// loop's streak, guard and lone-reviewer mark, and its `last` reviewer is
// where the pass goes on from. A round or a bot's review waiting on the
// judge keeps its findings, without verdicts, and a fix loses its streak mark.
fn drop_loop(value: &mut Value) {
    match value {
        Value::Array(list) => list.iter_mut().for_each(drop_loop),
        Value::Object(map) => {
            if map.contains_key("round") && map.contains_key("stage") {
                map.remove("consecutive_clean");
                map.remove("guard_cleared");
                map.remove("alone");
                finish_deep(map);
            }
            match map.get("stage").and_then(Value::as_str) {
                Some("judging") => {
                    map.insert("stage".to_owned(), "found".into());
                    map.remove("verdicts");
                }
                Some("fixing") => {
                    map.remove("clean");
                }
                _ => {}
            }
            map.values_mut().for_each(drop_loop);
        }
        _ => {}
    }
}

// A deep round saved past its two reads has finished reading. Findings not
// yet sent go to the fix turn as any round's do, a fix under way goes on, and
// a fix already made ends the round, so the pass goes on.
fn finish_deep(review: &mut Map<String, Value>) {
    let Some(stage) = review.get_mut("stage").and_then(Value::as_object_mut) else {
        return;
    };
    if stage.get("stage").and_then(Value::as_str) != Some("deep") {
        return;
    }
    let again = stage.get("again").and_then(Value::as_bool) == Some(true);
    let held = stage.get("held").and_then(Value::as_array).map(|held| {
        let findings = held.iter().filter_map(|h| h.get("finding")).cloned();
        findings.collect::<Vec<_>>()
    });
    // Anything else is left for the parser to refuse.
    let next = match (stage.get("step").and_then(Value::as_str), held) {
        (Some("confirming"), Some(held)) => json!({ "stage": "found", "findings": held }),
        (Some("sending"), Some(held)) if !again => json!({ "stage": "found", "findings": held }),
        (Some("sending" | "rechecking"), Some(_)) => json!({ "stage": "found", "findings": [] }),
        (Some("fixing"), Some(_)) => match stage.get("head") {
            Some(head) => json!({ "stage": "fixing", "head": head }),
            None => json!({ "stage": "fixing" }),
        },
        _ => return,
    };
    review.insert("stage".to_owned(), next);
}

#[cfg(test)]
mod tests;
