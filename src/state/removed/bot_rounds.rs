//! A state file saved while review bots ran a round of their own after CI
//!
//! Such a work item had finished its review pass, so its bot round loads as
//! a pass of the listed review bots alone, at the stage it was saved in: a
//! round waiting for the lease starts that pass afresh, a summon waits on,
//! threads found go to the worker, and a fix under way goes on and, once
//! pushed, resolves the threads it was sent. A question or a ruling that
//! keeps a bot fix resumes in that pass. A pending ruling on a bot that
//! never answered is a pass of the bots instead, and one on the old round
//! cap, which is gone, asks whether to send the threads it held, which a
//! yes sends for a fix that resolves them. An item past its review that no
//! bot had read owes the listed bots a pass after its next green CI. The
//! rounds a work item had counted are CodeRabbit's reads.

use serde_json::{Map, Value, json};

/// Folds every review bot round a file keeps into a pass of the bots
pub(in crate::state) fn fold_bot_rounds(value: &mut Value) {
    fold_pending(value);
    fold(value);
}

fn fold(value: &mut Value) {
    match value {
        Value::Array(list) => list.iter_mut().for_each(fold),
        Value::Object(map) => {
            fold_phase(map);
            fold_ruling(map);
            if map.contains_key("issue") && map.contains_key("branch") {
                owe_bots_after_ci(map);
                fold_tally(map);
            }
            map.values_mut().for_each(fold);
        }
        _ => {}
    }
}

// A bot round's phase, or a question's resume into one, as a pass of the
// bots. Anything else is left for the parser to refuse.
fn fold_phase(map: &mut Map<String, Value>) {
    let review = match map.get("state").and_then(Value::as_str) {
        Some("coderabbit") => match bot_stage(map) {
            Some(review) => review,
            None => return,
        },
        Some("coderabbit-fix") => match map.get("head") {
            Some(head) => bots_pass(json!({ "stage": "fixing", "head": head }), None),
            None => return,
        },
        _ => return,
    };
    *map = review;
    map.insert("state".to_owned(), "review".into());
}

fn bot_stage(map: &Map<String, Value>) -> Option<Map<String, Value>> {
    let bot = map
        .get("bot")
        .cloned()
        .unwrap_or_else(|| "coderabbit".into());
    let head = map.get("head")?;
    let flag = |key: &str| map.get(key).cloned().unwrap_or(Value::Bool(false));
    let review = match map.get("stage")?.as_str()? {
        "lease" => bots_pass(json!({ "stage": "round" }), None),
        "summoned" => {
            let at = map.get("at")?;
            let stage = json!({
                "stage": "summoned",
                "bot": bot,
                "started": at,
                "head": head,
                "at": at,
                "full": flag("full"),
                "resent": flag("resent"),
            });
            bots_pass(stage, Some(bot))
        }
        "found" => {
            let threads = map.get("threads")?.as_array()?;
            let ids = threads.iter().map(|t| t.get("id").cloned());
            let findings = threads.iter().map(|t| t.get("finding").cloned());
            let stage = json!({
                "stage": "found",
                "findings": findings.collect::<Option<Vec<_>>>()?,
                "threads": ids.collect::<Option<Vec<_>>>()?,
            });
            bots_pass(stage, Some(bot))
        }
        "fixing" => bots_pass(json!({ "stage": "fixing", "head": head }), None),
        _ => return None,
    };
    Some(review)
}

// A pass of the listed bots at `stage`, which `reviewer`'s round holds.
fn bots_pass(stage: Value, reviewer: Option<Value>) -> Map<String, Value> {
    let mut review = Map::new();
    review.insert("round".to_owned(), 1.into());
    review.insert("stage".to_owned(), stage);
    if let Some(reviewer) = reviewer {
        review.insert("reviewer".to_owned(), reviewer);
    }
    review.insert("bots_only".to_owned(), true.into());
    review
}

fn fixing(head: Option<&Value>, sent: Option<&Value>) -> Value {
    let mut stage = json!({ "stage": "fixing" });
    if let Some(head) = head {
        stage["head"] = head.clone();
    }
    if let Some(sent) = sent.filter(|sent| sent.as_array().is_some_and(|s| !s.is_empty())) {
        stage["sent"] = sent.clone();
    }
    Value::Object(bots_pass(stage, None))
}

// A fix-not-pushed ruling on a bot round's fix, as one on a fix in the pass
// of the bots.
fn fold_ruling(map: &mut Map<String, Value>) {
    if map.get("kind").and_then(Value::as_str) == Some("fix-not-pushed")
        && let Some(Value::Object(round)) = map.remove("coderabbit")
    {
        map.insert("review".to_owned(), fixing(round.get("head"), None));
    }
}

// The pending rulings on a bot round, which need the work item they park:
// a bot that never answered gets a pass of the bots in place of the ruling,
// and the round cap's held threads are asked about again for a fix that
// resolves them. A file before work items had issues keeps its one item
// under `work_item`.
fn fold_pending(value: &mut Value) {
    let Some(state) = value.as_object_mut() else {
        return;
    };
    let Some(Value::Array(rulings)) = state.remove("rulings") else {
        return;
    };
    let mut kept = Vec::new();
    for mut ruling in rulings {
        let kind = ruling["kind"]["kind"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let id = ruling.get("id").cloned().unwrap_or_default();
        let issue = ruling.get("issue").and_then(Value::as_u64);
        let parked = parked_on(state, &id, issue);
        match (kind.as_str(), parked) {
            ("coderabbit-silent", Some(item)) => {
                let mut pass = bots_pass(json!({ "stage": "round" }), None);
                pass.insert("state".to_owned(), "review".into());
                item.insert("phase".to_owned(), Value::Object(pass));
            }
            // Nothing is parked on it, so nothing is left for its yes to move.
            ("coderabbit-silent", None) => {
                eprintln!(
                    "state file: ruling {id}, on a review bot that never answered, parks no \
                     work item, so it is dropped"
                );
            }
            ("coderabbit-cap", parked) => {
                let held = parked.and_then(|item| item.get("held").cloned());
                let cap = &ruling["kind"];
                let review = fixing(cap.get("head"), held.as_ref());
                let prompt = cap.get("prompt").cloned().unwrap_or_else(|| "".into());
                let question = cap_question(&ruling);
                ruling["kind"] =
                    json!({ "kind": "fix-not-pushed", "review": review, "prompt": prompt });
                ruling["question"] = question.into();
                kept.push(ruling);
            }
            _ => kept.push(ruling),
        }
    }
    state.insert("rulings".to_owned(), Value::Array(kept));
}

// The work item parked on ruling `id`, of `issue` when the ruling names it.
fn parked_on<'a>(
    state: &'a mut Map<String, Value>,
    id: &Value,
    issue: Option<u64>,
) -> Option<&'a mut Map<String, Value>> {
    let parked = |item: &Map<String, Value>| {
        let on = item.get("phase").is_some_and(|phase| {
            phase.get("state").and_then(Value::as_str) == Some("ruling")
                && phase.get("id") == Some(id)
        });
        let of = issue.is_none_or(|issue| item.get("issue").and_then(Value::as_u64) == Some(issue));
        on && of
    };
    let lone = state.get("work_item").and_then(Value::as_object);
    if lone.is_some_and(parked) {
        return state.get_mut("work_item")?.as_object_mut();
    }
    let items = state.get_mut("work_items")?.as_array_mut()?;
    items
        .iter_mut()
        .filter_map(Value::as_object_mut)
        .find(|item| parked(item))
}

// The cap's question, said again for what a yes now does.
fn cap_question(ruling: &Value) -> String {
    let id = ruling.get("id").and_then(Value::as_u64).unwrap_or_default();
    let about = ruling
        .get("pull_request")
        .and_then(Value::as_u64)
        .map_or_else(
            || "its pull request".to_owned(),
            |n| format!("pull request #{n}"),
        );
    let held = ruling["kind"]
        .get("held")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    format!(
        "CodeRabbit's rounds on {about} reached their cap with {held} of its threads open. \
         `shep kelpie rule {id} yes` sends the worker those threads, which kelpie resolves \
         once its fix moves the head, and `shep kelpie rule {id} no <note>` sends the \
         worker your note."
    )
}

// An item past its review, with a pull request no bot had read clean, owes
// the listed bots a pass once CI is next green, before its merge. Every
// such file kept the bots' tally, which says whether one had.
fn owe_bots_after_ci(item: &mut Map<String, Value>) {
    let Some(tally) = item.get("coderabbit") else {
        return;
    };
    let satisfied = tally.get("satisfied").and_then(Value::as_bool) == Some(true);
    let opened = item.get("pull_request").is_some_and(|n| !n.is_null());
    let phase = item
        .get("phase")
        .and_then(|p| p.get("state"))
        .and_then(Value::as_str);
    let past_review = matches!(phase, Some("ci" | "ruling" | "implement"));
    if opened && past_review && !satisfied {
        item.insert("bots_after_ci".to_owned(), true.into());
    }
}

fn fold_tally(item: &mut Map<String, Value>) {
    let Some(Value::Object(tally)) = item.remove("coderabbit") else {
        return;
    };
    let rounds = tally.get("rounds").and_then(Value::as_u64).unwrap_or(0);
    if rounds > 0 {
        item.insert("bot_reads".to_owned(), json!({ "coderabbit": rounds }));
    }
}
