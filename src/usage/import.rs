//! Reading the calls a runner logged before the ledger into it
//!
//! A runner logs each worker turn that ends as an `ended` step, or `asked`
//! when the turn ended on a question, behind the time shep stamps the line
//! with. Those are the only calls it logged with their usage. Each becomes a
//! call line marked `imported`, unless the ledger already has the same
//! session ending within a few seconds of it.

use std::path::Path;

use serde::Deserialize;

use super::{CallKind, CallLine, Ended, Line, append_to, read, units};
use crate::ports::{Role, Timestamp, Usage};

/// Seconds apart a logged call and a ledger line of the same session may
/// end and still be the same call: the step is logged just after it is
/// recorded
const SLACK: u64 = 10;

/// What an import read and added
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Imported {
    /// Calls added to the ledger
    pub added: usize,
    /// Calls the ledger already held
    pub known: usize,
}

// A logged step, as far as an import reads it. Older runners left out
// `cost_usd` and `pull_request`, and some usage fields, so all may be missing.
#[derive(Deserialize)]
struct Step {
    step: String,
    issue: Option<u64>,
    session: Option<String>,
    #[serde(default)]
    usage: Option<LoggedUsage>,
    #[serde(default)]
    cost_usd: Option<f64>,
    #[serde(default)]
    pull_request: Option<u64>,
}

#[derive(Deserialize)]
struct LoggedUsage {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    cache_write: u64,
    #[serde(default)]
    cache_write_5m: u64,
    #[serde(default)]
    cache_read: u64,
    #[serde(default)]
    output: u64,
}

/// The call a runner's log line records, if it records one
///
/// The line is shep's time stamp, RFC 3339, then the step's JSON object.
fn call_of(line: &str) -> Option<CallLine> {
    let (stamp, json) = line.split_once(' ')?;
    let at: jiff::Timestamp = stamp.parse().ok()?;
    let step: Step = serde_json::from_str(json.trim()).ok()?;
    if !matches!(step.step.as_str(), "ended" | "asked") {
        return None;
    }
    let logged = step.usage?;
    let usage = Usage {
        input: logged.input,
        cache_write: logged.cache_write,
        cache_write_5m: logged.cache_write_5m,
        cache_read: logged.cache_read,
        output: logged.output,
    };
    Some(CallLine {
        at: Timestamp(u64::try_from(at.as_second()).ok()?),
        issue: step.issue,
        pull_request: step.pull_request,
        role: Role::Worker,
        kind: CallKind::Turn,
        agent: None,
        harness: None,
        model: None,
        effort: None,
        session: Some(step.session?),
        usage,
        units: units(usage),
        cost_usd: step.cost_usd,
        unpriced: step.cost_usd.is_none(),
        session_cost_usd: None,
        seconds: None,
        ended: Ended::Answered,
        gpu_seconds: None,
        pacer: Default::default(),
        imported: true,
    })
}

/// Adds each call `log`'s lines record to the ledger at `ledger`, oldest
/// first, but those it already holds
///
/// # Errors
///
/// A message naming the file that could not be read or written.
pub fn import(log: &str, ledger: &Path) -> Result<Imported, String> {
    let held = read(ledger).map_err(|e| format!("cannot read {}: {e}", ledger.display()))?;
    // Only what the ledger held before is matched loosely: two turns of one
    // session may end within the slack of each other in the same log.
    // Each held line stands for one logged call at most, so a turn the
    // ledger has cannot hide its session's next one.
    let mut held: Vec<Option<CallLine>> = held
        .into_iter()
        .filter_map(|line| match line {
            Line::Call(call) => Some(Some(call)),
            Line::Finished(_) => None,
        })
        .collect();
    // The log's own calls so far, a line logged twice being one call
    let mut seen: Vec<CallLine> = Vec::new();
    let mut imported = Imported::default();
    for call in log.lines().filter_map(call_of) {
        if seen.contains(&call) {
            imported.known += 1;
            continue;
        }
        seen.push(call.clone());
        let session = call.session.as_deref().unwrap_or_default();
        let matched = (held.iter_mut()).find(|h| {
            h.as_ref()
                .is_some_and(|h| h.same_call(session, call.at, SLACK))
        });
        if let Some(matched) = matched {
            *matched = None;
            imported.known += 1;
            continue;
        }
        append_to(ledger, &Line::Call(call))
            .map_err(|e| format!("cannot add to {}: {e}", ledger.display()))?;
        imported.added += 1;
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two runners' `ended` lines, the second an older shape with no cost
    // or pull request, and lines that record no call.
    const LOG: &str = r#"2026-10-01T16:53:32.105-04:00 {"step":"ended","issue":135,"session":"15eb335f","usage":{"input":196,"cache_write":135548,"cache_read":10214685,"output":40225},"cost_usd":2.987771,"work_item_cost_usd":2.987771,"pull_request":271}
2026-10-01T17:00:00.000-04:00 {"step":"held","kind":"window","reason":"the turn ended","until":1}
2026-10-01T17:05:10.000-04:00 {"step":"ended","issue":136,"session":"9f3ec502","usage":{"input":10,"cache_write":20,"cache_read":30,"output":40}}
2026-10-01T17:06:00.000-04:00 {"step":"asked","issue":136,"session":"9f3ec502","usage":{"input":1,"cache_write":2,"cache_read":3,"output":4},"cost_usd":0.5,"work_item_cost_usd":0.5,"pull_request":null,"id":3,"question":"q","comment_failed":null}
not a step at all
2026-10-01T17:07:00.000-04:00 {"step":"ended","issue":13"#;

    #[test]
    fn each_logged_call_is_added_once_whatever_its_shape() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("usage.jsonl");
        assert_eq!(
            import(LOG, &ledger).unwrap(),
            Imported { added: 3, known: 0 }
        );
        let lines = read(&ledger).unwrap();
        let Line::Call(first) = &lines[0] else {
            panic!("{lines:?}")
        };
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::json!({
                "at": 1_790_888_012,
                "issue": 135,
                "pull_request": 271,
                "role": "worker",
                "kind": "turn",
                "session": "15eb335f",
                "usage": {
                    "input": 196, "cache_write": 135_548,
                    "cache_read": 10_214_685, "output": 40_225,
                },
                "units": 1_493_886,
                "cost_usd": 2.987771,
                "ended": "answered",
                "imported": true,
            })
        );
        let Line::Call(older) = &lines[1] else {
            panic!("{lines:?}")
        };
        assert!(older.unpriced && older.cost_usd.is_none(), "{older:?}");

        assert_eq!(
            import(LOG, &ledger).unwrap(),
            Imported { added: 0, known: 3 },
            "a second import adds nothing"
        );
        assert_eq!(read(&ledger).unwrap().len(), 3);
    }

    #[test]
    fn two_turns_of_one_session_seconds_apart_both_import_and_once_only() {
        let log = r#"2026-10-01T17:05:10.000-04:00 {"step":"ended","issue":136,"session":"9f3ec502","usage":{"input":10,"cache_write":20,"cache_read":30,"output":40}}
2026-10-01T17:05:15.000-04:00 {"step":"ended","issue":136,"session":"9f3ec502","usage":{"input":1,"cache_write":2,"cache_read":3,"output":4}}
2026-10-01T17:05:15.000-04:00 {"step":"ended","issue":136,"session":"9f3ec502","usage":{"input":1,"cache_write":2,"cache_read":3,"output":4}}"#;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("usage.jsonl");
        assert_eq!(
            import(log, &ledger).unwrap(),
            Imported { added: 2, known: 1 },
            "the line logged twice is one call"
        );
        assert_eq!(
            import(log, &ledger).unwrap(),
            Imported { added: 0, known: 3 }
        );
        assert_eq!(read(&ledger).unwrap().len(), 2);

        // A ledger that kept only the first turn gets the second back.
        let partial = dir.path().join("partial.jsonl");
        let first = log.lines().next().unwrap();
        assert_eq!(
            import(first, &partial).unwrap(),
            Imported { added: 1, known: 0 }
        );
        assert_eq!(
            import(log, &partial).unwrap(),
            Imported { added: 1, known: 2 }
        );
        assert_eq!(read(&partial).unwrap().len(), 2);
    }

    #[test]
    fn a_five_minute_cache_write_imports_at_its_own_weight() {
        let log = r#"2026-10-01T17:05:10.000-04:00 {"step":"ended","issue":136,"session":"9f3ec502","usage":{"input":0,"cache_write":1000,"cache_write_5m":400,"cache_read":0,"output":0}}"#;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("usage.jsonl");
        import(log, &ledger).unwrap();
        let lines = read(&ledger).unwrap();
        let [Line::Call(call)] = lines.as_slice() else {
            panic!("one call")
        };
        assert_eq!((call.usage.cache_write_5m, call.units), (400, 1700));
    }
}
