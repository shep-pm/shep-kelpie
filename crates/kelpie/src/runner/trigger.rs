//! The maintainer's triggers and what `status` shows

use std::fmt;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;

use super::pace::PacerStatus;
use super::{Answer, Runner};
use crate::board::WorkerModel;
use crate::ports::{SessionId, Timestamp};
use crate::state::{LeaseHeld, Ruling, RunState, StateError};
use crate::work_item::{CodeRabbitTally, Phase, Turn, WorkItem};

/// The triggers a runner answers
pub const ACTIONS: [&str; 7] = ["status", "start", "pause", "add", "rule", "gate", "drop"];

/// What `rule` takes, as its refusals say
const RULE_USAGE: &str = "`rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`";

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
    /// What usage was when last read, and why nothing new is starting
    pub pacer: PacerStatus<'a>,
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
    /// Its CodeRabbit rounds so far
    pub coderabbit: CodeRabbitTally,
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
            coderabbit: item.coderabbit,
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
    Gate,
    Drop,
}

/// Answers one trigger with a JSON body: the status, or `{"error": ...}`
///
/// Blank params count as none. `add` takes an issue number, `rule` takes
/// `<id> yes`, `<id> no <note>` or `<id> answer <text>`, and every other
/// action takes nothing.
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
        Request::Gate => runner.gate().map_err(|e| e.to_string()),
        Request::Drop => runner.drop_work_item().map_err(|e| e.to_string()),
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
        ("gate", None) => Ok(Request::Gate),
        ("drop", None) => Ok(Request::Drop),
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
        Some(("answer", text)) if !text.trim().is_empty() => Answer::Text(text.trim().to_owned()),
        _ => return None,
    };
    Some(Request::Rule(id, answer))
}

/// Whether `params` reads as `rule`'s own `<id> no <note>` or
/// `<id> answer <text>`, and never as `<id> yes`
///
/// The relay's settings pre-allow `kelpie relay-answer`, so this is what
/// keeps a "yes" the relay was talked into forwarding as an "answer" from
/// reaching `rule` as one: the same parser `rule` itself reads decides it,
/// not a second guess at the grammar.
pub fn is_no_or_answer(params: &str) -> bool {
    matches!(
        read_rule(params),
        Some(Request::Rule(_, Answer::No(_) | Answer::Text(_)))
    )
}

// Digits only, so `+7` and `#7` are refused rather than read as 7.
fn number(text: &str) -> Option<u64> {
    let n = text.parse::<u64>().ok()?;
    (n > 0 && text.bytes().all(|b| b.is_ascii_digit())).then_some(n)
}

/// Why `gate` was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    /// No work item is in flight
    NoWorkItem,
    /// The work item is past its worker's turns: in CI, parked, or merging
    AlreadyGated(u64),
    /// The worker's turn has not ended, with the turn's state
    TurnNotEnded(u64, &'static str),
    /// Kelpie knows no pull request for the work item
    NoPullRequest(u64),
    /// The change could not be saved
    State(StateError),
}

impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoWorkItem => f.write_str("no work item is in flight"),
            Self::AlreadyGated(issue) => {
                write!(f, "the work item for #{issue} is already in the gate")
            }
            Self::TurnNotEnded(issue, state) => {
                write!(f, "the worker's turn on #{issue} is {state}, not ended")
            }
            Self::NoPullRequest(issue) => write!(f, "no pull request is known for #{issue}"),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for GateError {}

impl Runner {
    /// Sends the work item into the gate, when its worker's turn has ended
    /// with a pull request kelpie knows but the gate was not entered
    ///
    /// A work item saved before the gate existed is one such.
    ///
    /// # Errors
    ///
    /// [`GateError`] naming why the work item cannot enter the gate. Nothing
    /// changes then.
    pub fn gate(&mut self) -> Result<(), GateError> {
        let item = self.state.work_item.as_ref().ok_or(GateError::NoWorkItem)?;
        let issue = item.issue;
        if item.phase != Phase::Implement {
            return Err(GateError::AlreadyGated(issue));
        }
        let state = match item.turn {
            Turn::Ended { .. } => None,
            Turn::Due => Some("due"),
            Turn::Next { .. } => Some("queued"),
            Turn::Running { .. } => Some("running"),
            Turn::Failed { .. } => Some("failed"),
        };
        if let Some(state) = state {
            return Err(GateError::TurnNotEnded(issue, state));
        }
        if item.pull_request.is_none() {
            return Err(GateError::NoPullRequest(issue));
        }
        let since = self.ports.clock.now();
        self.update(|item| item.phase = Phase::Ci { head: None, since })
            .map_err(GateError::State)
    }
}

pub(super) fn lock(runner: &Mutex<Runner>) -> MutexGuard<'_, Runner> {
    runner.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ports::{Checks, Cost, Usage};
    use crate::runner::{StepReport, step};
    use crate::test::{Rig, Scripted};

    // A running project with issue 7 in flight
    fn with_issue_7(project: &str) -> (Rig, Mutex<Runner>) {
        let rig = Rig::new(project);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        assert_eq!(rig.ask(&runner, "add", Some("7"))["work_item"]["issue"], 7);
        (rig, runner)
    }

    #[test]
    fn is_no_or_answer_refuses_every_shape_of_yes() {
        for refused in [
            "3 yes",
            "3 Yes",
            " 3 yes",
            "3  yes",
            "3 yes extra",
            "yes",
            "3",
            "",
        ] {
            assert!(!is_no_or_answer(refused), "{refused:?}");
        }
        for allowed in ["3 no rename it", "3 answer use --dry-run"] {
            assert!(is_no_or_answer(allowed), "{allowed:?}");
        }
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
                "pacer": { "reading": null, "holding": null },
            })
        );
    }

    #[test]
    fn every_registered_action_is_answered_and_no_other() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        for action in ACTIONS
            .into_iter()
            .filter(|a| !["rule", "gate", "drop"].contains(a))
        {
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

    #[test]
    fn gate_sends_a_work_item_saved_before_the_gate_to_its_merge_ruling() {
        let (rig, runner, head) = Rig::with_pull_request("hazels-lab");
        drop(runner);
        let state = rig.paths().state;
        let text = std::fs::read_to_string(&state).unwrap();
        let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        let item = saved["work_item"].as_object_mut().unwrap();
        item.remove("phase");
        item.remove("red_head");
        std::fs::write(&state, saved.to_string()).unwrap();

        let runner = rig.open().unwrap();
        rig.forge.set_checks(&head, Checks::Passed);
        assert_eq!(step(&runner).unwrap(), None, "left alone until asked");
        let status = rig.ask(&runner, "gate", None);
        assert_eq!(
            status["work_item"]["phase"],
            json!({ "state": "ci", "head": null, "since": Rig::EPOCH })
        );
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 1, .. })
        ));
        assert_eq!(rig.forge.merges(), []);
    }

    #[test]
    fn gate_refuses_anything_but_an_ended_turn_with_a_known_pull_request() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        let refused = |runner: &Mutex<Runner>, error: &str| {
            assert_eq!(rig.ask(runner, "gate", None), json!({ "error": error }));
        };
        refused(&runner, "no work item is in flight");
        rig.ask(&runner, "add", Some("7"));
        refused(&runner, "the worker's turn on #7 is due, not ended");
        assert_eq!(
            rig.ask(&runner, "gate", Some("7")),
            json!({ "error": "`gate` takes no params" })
        );

        rig.ask(&runner, "start", None);
        rig.claude
            .script([Scripted::Reply(Usage::default(), Cost(1))]);
        step(&runner).unwrap();
        refused(&runner, "no pull request is known for #7");

        let (rig, runner, _) = Rig::with_pull_request("rotom");
        assert_eq!(
            rig.ask(&runner, "gate", None),
            json!({ "error": "the work item for #7 is already in the gate" })
        );
    }
}
