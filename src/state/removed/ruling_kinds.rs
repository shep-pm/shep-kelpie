//! A state file saved while rulings had 13 kinds
//!
//! Eight of them are now the reasons a work item is `stuck`, and each loads
//! as a `stuck` ruling naming it, its question and fields kept. Their
//! `reason` field, where they had one, is now `why`, since `reason` names the
//! old kind. `claude-files` is `agent-files`.

use serde_json::{Map, Value};

// The kinds that are now a `stuck` ruling's reasons, under the same names
const STUCK: [&str; 8] = [
    "rebase",
    "still-red",
    "merge-refused",
    "closed",
    "local-model-spilled",
    "fix-not-pushed",
    "turn-timeout",
    "turn-failed",
];

/// Names each pending ruling's kind as one of the six
pub(in crate::state) fn fold_ruling_kinds(value: &mut Value) {
    let rulings = value.get_mut("rulings").and_then(Value::as_array_mut);
    let kinds = rulings.into_iter().flatten().filter_map(|ruling| {
        let kind = ruling.get_mut("kind")?;
        kind.as_object_mut()
    });
    kinds.for_each(fold);
}

// Anything that is not one of the old kinds is left for the parser.
fn fold(kind: &mut Map<String, Value>) {
    let Some(name) = kind.get("kind").and_then(Value::as_str) else {
        return;
    };
    if name == "claude-files" {
        kind.insert("kind".to_owned(), "agent-files".into());
        return;
    }
    if !STUCK.contains(&name) {
        return;
    }
    let reason = name.to_owned();
    if let Some(why) = kind.remove("reason") {
        kind.insert("why".to_owned(), why);
    }
    kind.insert("kind".to_owned(), "stuck".into());
    kind.insert("reason".to_owned(), reason.into());
}
