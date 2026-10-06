//! What removed features left in a state file saved before they went
//!
//! Such a file still loads. The relay's, the planning call's, the
//! whole-issue check's, the review loop's, the deep round's later steps' and
//! the shots' fields are dropped before reading. The check's, the judge's
//! and the deep round's calls count as a reviewer's session's. Their time,
//! a round's, a GPU wait's and a review bot's all count as review's, and
//! the shots' and a paused project's as other, so totals still add up. A
//! review saved mid-loop goes on from the reviewer after the one it last
//! recorded. The deep round
//! and the project's own Claude round, `deep` and `claude`, are
//! `defect-hunter`, and a deep round's two reads are its two looks. A work
//! item's worker model becomes the agent named for it. A review bot round
//! becomes a pass of the listed bots. A pending ruling of a removed kind has
//! nothing here to answer it, so the file is refused.

use serde_json::{Map, Value, json};

mod bot_rounds;

pub(super) use bot_rounds::fold_bot_rounds;

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
// work, so their time is review's and their calls a reviewer's. A shots run
// was kelpie's own, between steps, so its time is other. The phases that
// came before review and other took them in, and a paused project's time
// is other's.
const PHASES: [(&str, &str); 10] = [
    ("audit", "review"),
    ("judging", "review"),
    ("deep_round", "review"),
    ("gpu_wait", "review"),
    ("local_round", "review"),
    ("claude_round", "review"),
    ("coderabbit_window", "review"),
    ("coderabbit_review", "review"),
    ("paused", "other"),
    ("shots", "other"),
];
const CALLS: [&str; 3] = ["audit", "judge", "deep"];
const ROLES: [&str; 3] = ["auditor", "judge", "deep_reviewer"];

// The reviewers kelpie defined itself before reviewers were agent files,
// and the agent each now is: the deep round is `defect-hunter`, and so is
// the Claude round, whose quality prompt measured worse than the defect one.
const REVIEWERS: [(&str, &str); 2] = [("deep", "defect-hunter"), ("claude", "defect-hunter")];

// The model ids a `worker:<model>-<effort>` label ran by default, by the
// label's name for them, which the agents named for them start with.
const LABELLED: [(&str, &str); 4] = [
    ("claude-opus-5-5", "opus"),
    ("claude-sonnet-5-5", "sonnet"),
    ("claude-haiku-4-5-20251001", "haiku"),
    ("claude-fable-5-1", "fable"),
];

// What a work item given to the old local worker runs on: the project named
// that agent in settings, which the state file never held.
const LOCAL: &str = "local";

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
    // A version 1 file's one work item is moved into the list after this.
    if let Some(Value::Object(item)) = state.get_mut("work_item") {
        name_the_agent(item);
    }
    for item in objects(state.get_mut("work_items")) {
        name_the_agent(item);
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

/// A work item's worker as a state file saved it before agent files
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OldWorker {
    /// The work item's issue
    pub issue: u64,
    /// The agent the work item now runs on
    pub agent: String,
    /// The model its worker ran
    pub model: String,
    /// The effort its worker ran at
    pub effort: String,
    /// Whether it was given to the project's local worker
    pub local: bool,
}

/// Each work item's worker in a state file saved before agent files
pub(super) fn old_workers(value: &Value) -> Vec<OldWorker> {
    let one = value.get("work_item").into_iter();
    let listed = value.get("work_items").and_then(Value::as_array);
    one.chain(listed.into_iter().flatten())
        .filter_map(Value::as_object)
        .filter_map(old_worker)
        .collect()
}

// A worker's model and effort become the agent `<model>-<effort>`, so Sonnet
// 5.5 at high is `sonnet-high`, as kelpie's own agent files name them. An
// item given to the local worker runs on the agent `local`.
fn old_worker(item: &Map<String, Value>) -> Option<OldWorker> {
    let worker = item.get("worker")?.as_object()?;
    let model = worker.get("model")?.as_str()?;
    let effort = worker.get("effort")?.as_str()?;
    let local = worker.get("local").and_then(Value::as_bool) == Some(true);
    let agent = match local {
        true => LOCAL.to_owned(),
        false => {
            let named = LABELLED.iter().find(|(id, _)| *id == model);
            let model = named.map_or_else(|| as_name(model), |(_, name)| (*name).to_owned());
            format!("{model}-{effort}")
        }
    };
    Some(OldWorker {
        issue: item
            .get("issue")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        agent,
        model: model.to_owned(),
        effort: effort.to_owned(),
        local,
    })
}

// Anything that is not a worker record is left for the parser to refuse.
fn name_the_agent(item: &mut Map<String, Value>) {
    let Some(old) = old_worker(item) else {
        return;
    };
    item.remove("worker");
    item.insert("agent".to_owned(), old.agent.into());
}

// A model id as part of an agent's name: lowercase letters, digits and `-`.
fn as_name(model: &str) -> String {
    let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let name = model.to_ascii_lowercase();
    name.chars()
        .map(|c| if allowed(c) { c } else { '-' })
        .collect()
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

// A deep round saved at a read is `defect-hunter`'s look: the first is a
// round, and the second its second look. One saved past its two reads has
// finished reading. Findings not
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
        (Some("read"), _) => json!({ "stage": "round" }),
        (Some("missed"), _) => match stage.get("first") {
            Some(first) => json!({ "stage": "second-look", "first": first }),
            None => return,
        },
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

/// Names a file's reviewers as agent files: its `deep` and `claude` become
/// `defect-hunter`, wherever a review or a work item's skipped list keeps one
///
/// Only for a file saved before reviewers were agent files, since a newer
/// one may list agents of those names.
pub(super) fn name_reviewers_as_files(value: &mut Value) {
    match value {
        Value::Array(list) => list.iter_mut().for_each(name_reviewers_as_files),
        Value::Object(map) => {
            if map.contains_key("round") && map.contains_key("stage") {
                rename_reviewers(map);
            }
            if map.contains_key("issue") && map.contains_key("branch") {
                rename_skipped(map);
            }
            map.values_mut().for_each(name_reviewers_as_files);
        }
        _ => {}
    }
}

// A review's reviewer, the pass's reviewers and the one before, each once.
fn rename_reviewers(review: &mut Map<String, Value>) {
    for key in ["reviewer", "last"] {
        if let Some(name) = review.get_mut(key) {
            rename(name);
        }
    }
    if let Some(Value::Array(ran)) = review.get_mut("ran") {
        rename_each(ran);
    }
    // A pass that ran `deep` and was cut short in `claude` has run
    // `defect-hunter` already, so it does not run again.
    let ran = review.get("ran").and_then(Value::as_array);
    let reviewer = review.get("reviewer");
    if reviewer.is_some_and(|name| ran.is_some_and(|ran| ran.contains(name))) {
        review.remove("reviewer");
    }
}

// The reviewers a work item skipped, by their names as agent files. Only a
// local reviewer's name keys its failures, and none of those is renamed.
fn rename_skipped(item: &mut Map<String, Value>) {
    if let Some(Value::Array(skipped)) = item.get_mut("reviewers_skipped") {
        rename_each(skipped);
    }
}

// Two old names can become one, which a list keeps once.
fn rename_each(names: &mut Vec<Value>) {
    names.iter_mut().for_each(rename);
    let mut seen = Vec::new();
    names.retain(|name| {
        let first = !seen.contains(name);
        seen.push(name.clone());
        first
    });
}

fn rename(name: &mut Value) {
    let renamed = REVIEWERS
        .iter()
        .find(|(old, _)| name.as_str() == Some(old))
        .map(|(_, new)| *new);
    if let Some(new) = renamed {
        *name = new.into();
    }
}

#[cfg(test)]
mod tests;
