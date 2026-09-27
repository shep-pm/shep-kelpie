//! The maintainer's triggers and what `status` shows

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;

use super::{Answer, Runner};
use crate::board::WorkerModel;
use crate::ports::{SessionId, Timestamp};
use crate::state::{LeaseHeld, Ruling, RunState};
use crate::work_item::{Phase, Turn, WorkItem};

/// The triggers a runner answers
pub const ACTIONS: [&str; 5] = ["status", "start", "pause", "add", "rule"];

/// What `rule` takes, as its refusals say
const RULE_USAGE: &str = "`rule` takes `<id> yes` or `<id> no <note>`";

/// What `status` answers
#[derive(Debug, Serialize)]
pub struct Status<'a> {
    /// The project
    pub project: &'a str,
    /// Running or paused
    pub run: RunState,
    /// When it last started or paused
    pub since: Timestamp,
    /// The work item in flight
    pub work_item: Option<WorkItemStatus<'a>>,
    /// Rulings waiting on the maintainer, oldest first
    pub rulings: &'a [Ruling],
    /// Leases held
    pub leases: &'a [LeaseHeld],
}

/// The work item in flight, as `status` shows it
#[derive(Debug, Serialize)]
pub struct WorkItemStatus<'a> {
    /// The issue it resolves
    pub issue: u64,
    /// The issue's title
    pub title: &'a str,
    /// Its branch
    pub branch: &'a str,
    /// Its worktree
    pub worktree: &'a Path,
    /// The model and effort its worker runs on
    pub worker: &'a WorkerModel,
    /// The worker's session, which the maintainer can resume by hand
    pub session: &'a SessionId,
    /// Where the worker's turn stands
    pub turn: &'a Turn,
    /// Where it stands between the worker's turns and the merge
    pub phase: &'a Phase,
    /// The worker's draft pull request, once kelpie has seen it
    pub pull_request: Option<u64>,
    /// Claude calls made for it so far
    pub calls: usize,
    /// What they have cost, in US dollars
    pub cost_usd: f64,
}

impl<'a> From<&'a WorkItem> for WorkItemStatus<'a> {
    fn from(item: &'a WorkItem) -> Self {
        Self {
            issue: item.issue,
            title: &item.title,
            branch: &item.branch,
            worktree: &item.worktree,
            worker: &item.worker,
            session: &item.session,
            turn: &item.turn,
            phase: &item.phase,
            pull_request: item.pull_request,
            calls: item.calls.len(),
            cost_usd: item.cost().usd(),
        }
    }
}

/// A trigger, read
enum Request {
    Status,
    Start,
    Pause,
    Add(u64),
    Rule(u64, Answer),
}

/// Answers one trigger with a JSON body: the status, or `{"error": ...}`
///
/// Blank params count as none. `add` takes an issue number, `rule` takes
/// `<id> yes` or `<id> no <note>`, and every other action takes nothing.
pub fn answer(runner: &Mutex<Runner>, action: &str, params: Option<&str>) -> String {
    let error = |message: String| serde_json::json!({ "error": message }).to_string();
    let request = match read(action, params.map(str::trim).filter(|p| !p.is_empty())) {
        Ok(request) => request,
        Err(e) => return error(e),
    };
    // Memory changes only after a save succeeds, so a panicked holder
    // cannot have left the runner half changed.
    let mut runner = lock(runner);
    let changed = match request {
        Request::Status => Ok(()),
        Request::Start => runner.start().map_err(|e| e.to_string()),
        Request::Pause => runner.pause().map_err(|e| e.to_string()),
        Request::Add(issue) => runner.add(issue).map(drop).map_err(|e| e.to_string()),
        Request::Rule(id, answer) => runner.rule(id, answer).map_err(|e| e.to_string()),
    };
    match changed {
        Ok(()) => serde_json::to_string(&runner.status()).expect("status serializes to JSON"),
        Err(e) => error(e),
    }
}

fn read(action: &str, params: Option<&str>) -> Result<Request, String> {
    match (action, params) {
        ("add", Some(p)) => number(p)
            .map(Request::Add)
            .ok_or_else(|| format!("{p:?} is not an issue number")),
        ("add", None) => Err("`add` takes an issue number".into()),
        ("rule", Some(p)) => read_rule(p).ok_or_else(|| format!("{RULE_USAGE}, not {p:?}")),
        ("rule", None) => Err(RULE_USAGE.into()),
        (_, _) if !ACTIONS.contains(&action) => Err(format!("unknown action `{action}`")),
        (_, Some(_)) => Err(format!("`{action}` takes no params")),
        ("start", None) => Ok(Request::Start),
        ("pause", None) => Ok(Request::Pause),
        (_, None) => Ok(Request::Status),
    }
}

fn read_rule(params: &str) -> Option<Request> {
    let (id, rest) = params.split_once(char::is_whitespace)?;
    let id = number(id)?;
    let rest = rest.trim_start();
    let answer = match rest.split_once(char::is_whitespace) {
        None if rest == "yes" => Answer::Yes,
        Some(("no", note)) if !note.trim().is_empty() => Answer::No(note.trim().to_owned()),
        _ => return None,
    };
    Some(Request::Rule(id, answer))
}

// Digits only, so `+7` and `#7` are refused rather than read as 7.
fn number(text: &str) -> Option<u64> {
    let n = text.parse::<u64>().ok()?;
    (n > 0 && text.bytes().all(|b| b.is_ascii_digit())).then_some(n)
}

pub(super) fn lock(runner: &Mutex<Runner>) -> MutexGuard<'_, Runner> {
    runner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test::Rig;

    // A running project with issue 7 in flight
    fn with_issue_7(project: &str) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new(project);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        assert_eq!(rig.ask(&runner, "add", Some("7"))["work_item"]["issue"], 7);
        (rig, runner)
    }

    #[test]
    fn a_new_project_is_paused_with_nothing_in_flight() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "status", None),
            json!({
                "project": "koji",
                "run": "paused",
                "since": Rig::EPOCH,
                "work_item": null,
                "rulings": [],
                "leases": [],
            })
        );
    }

    #[test]
    fn every_registered_action_is_answered_and_no_other() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        for action in ACTIONS.into_iter().filter(|&a| a != "rule") {
            let params = (action == "add").then_some("7");
            assert_eq!(
                rig.ask(&runner, action, params)["project"],
                "koji",
                "{action}"
            );
        }
        assert_eq!(
            rig.ask(&runner, "rule", Some("1 yes")),
            json!({ "error": "no ruling 1 is pending" })
        );
        assert_eq!(
            rig.ask(&runner, "merge", None),
            json!({ "error": "unknown action `merge`" })
        );
    }

    #[test]
    fn a_trigger_with_params_is_refused_and_changes_nothing() {
        let rig = Rig::new("rotom");
        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "start", Some("now")),
            json!({ "error": "`start` takes no params" })
        );
        assert_eq!(rig.ask(&runner, "status", None)["run"], "paused");
    }

    #[test]
    fn add_takes_one_plain_issue_number() {
        let rig = Rig::new("golbat");
        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "add", None),
            json!({ "error": "`add` takes an issue number" })
        );
        for bad in ["#7", "0", "-7", "+7", "7 8", "seven"] {
            assert_eq!(
                rig.ask(&runner, "add", Some(bad)),
                json!({ "error": format!("{bad:?} is not an issue number") }),
            );
        }
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    }

    #[test]
    fn a_second_add_while_one_is_in_flight_is_refused() {
        let (rig, runner) = with_issue_7("golbat");
        assert_eq!(
            rig.ask(&runner, "add", Some("8")),
            json!({ "error": "the work item for #7 is in flight" })
        );
    }

    #[test]
    fn adding_an_issue_the_forge_cannot_show_changes_nothing() {
        let rig = Rig::new("reactmap");
        rig.forge.remove_issue(9);
        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "add", Some("9")),
            json!({ "error": "cannot read the issue: gh failed: no issue #9" })
        );
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    }
}
