//! The maintainer's triggers and what `status` shows

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;

use super::gpu::GpuStatus;
use super::pace::PacerStatus;
use super::{Answer, Runner};
use crate::board::{Skip, WorkerModel};
use crate::lease::gpu::LockHolder;
use crate::ports::{ModelSeat, SessionId, Timestamp};
use crate::relay::Settled;
use crate::settings::MergeAuthority;
use crate::skills::StepSkill;
use crate::state::{Finished, LeaseHeld, Ruling, RunState, StateError, Waiting};
use crate::work_item::{CodeRabbitTally, Phase, QwenTally, Spend, Split, Turn, WorkItem};

/// The triggers a runner answers
pub const ACTIONS: [&str; 11] = [
    "status", "start", "pause", "add", "rework", "adopt", "rule", RELAY_RULE, "gate", "drop",
    "timings",
];

/// How many finished work items `timings` totals when given no count
const TIMINGS_DEFAULT: usize = 10;

/// How many finished work items `status` lists
pub(super) const STATUS_HISTORY: usize = 10;

/// `rule`, sent by the relay: the same answer, but the relay is not told
/// of it, since it already knows
pub const RELAY_RULE: &str = "relay-rule";

/// What `rule` and `relay-rule` take, as their refusals say
const RULE_USAGE: &str = "takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`";

/// What `status` answers
#[derive(Debug, Serialize)]
pub struct Status<'a> {
    /// The project
    pub project: &'a str,
    /// Who decides its merges
    pub merge_authority: MergeAuthority,
    /// Running or paused
    pub run: RunState,
    /// When it last started or paused
    pub since: Timestamp,
    /// The first of `work_items`, where a status read before they existed
    /// finds the work item
    pub work_item: Option<WorkItemStatus<'a>>,
    /// Every open work item, oldest first
    pub work_items: Vec<WorkItemStatus<'a>>,
    /// How many work items may be open at once
    pub max_items: u32,
    /// Pull requests adopted and waiting for a free slot, oldest first
    pub adopted: &'a [Waiting],
    /// Ready issues the board passed over on its last poll, and why
    pub skipped: &'a [Skip],
    /// Rulings waiting on the maintainer, oldest first
    pub rulings: &'a [Ruling],
    /// Leases held
    pub leases: &'a [LeaseHeld],
    /// The most recent finished work items, oldest first
    pub history: &'a [Finished],
    /// What usage was when last read, and why nothing new is starting
    pub pacer: PacerStatus<'a>,
    /// The skill each step runs, and why any chosen one could not load
    pub skills: &'a [StepSkill],
    /// Where the local model sat when a round last looked, when there was
    /// an Ollama host to ask
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_model: Option<LocalModelStatus>,
    /// The GPU's load as the metrics page last said, when kelpie's settings
    /// name one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuStatus>,
    /// Who holds each lease a local agent's calls take, by its name, or
    /// null while it is free
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub local_leases: BTreeMap<String, Option<LockHolder>>,
}

/// The local model's placement, as Ollama's `/api/ps` last said
///
/// `/api/ps` has no utilization, so this says where the model sits and not
/// how busy the GPU is.
#[derive(Debug, Serialize)]
pub struct LocalModelStatus {
    /// The model's name
    pub name: String,
    /// How much of it is on the GPU, in whole percent
    pub gpu_percent: u64,
    /// The context length it was loaded with, in tokens
    pub context_length: Option<u64>,
    /// When Ollama unloads it
    pub expires_at: Option<String>,
}

impl From<ModelSeat> for LocalModelStatus {
    fn from(seat: ModelSeat) -> Self {
        Self {
            gpu_percent: seat.gpu_percent(),
            name: seat.name,
            context_length: seat.context_length,
            expires_at: seat.expires_at,
        }
    }
}

/// An open work item, as `status` shows it
#[derive(Debug, Serialize)]
pub struct WorkItemStatus<'a> {
    /// The issue it resolves
    pub issue: u64,
    /// The issue's title
    pub title: &'a str,
    /// Its branch
    pub branch: &'a str,
    /// Whether it adopts a pull request kelpie didn't open
    pub adopted: bool,
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
    /// What the calls whose harness reports dollars have cost, in US dollars
    pub cost_usd: f64,
    /// Calls whose harness reported no dollars, which `cost_usd` leaves out
    #[serde(skip_serializing_if = "is_zero")]
    pub unpriced_calls: usize,
    /// Calls and cost by role
    pub by_role: Spend,
    /// Its qwen rounds, which cost no money
    pub qwen: QwenTally,
    /// Why kelpie's last shots run failed, when it did
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shots_failed: Option<&'a str>,
    /// Where its wall time went, and the phase it is in now
    pub timings: Split,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl<'a> WorkItemStatus<'a> {
    /// `item` as `status` shows it, with its time read as `timings`
    pub(super) fn new(item: &'a WorkItem, timings: Split) -> Self {
        Self {
            issue: item.issue,
            title: &item.title,
            branch: &item.branch,
            adopted: item.adopted,
            worktree: &item.worktree,
            worker: &item.worker,
            session: &item.session,
            turn: &item.turn,
            phase: &item.phase,
            pull_request: item.pull_request,
            coderabbit: item.coderabbit,
            calls: item.calls.len(),
            cost_usd: item.cost().usd(),
            unpriced_calls: item.calls.iter().filter(|c| c.unpriced).count(),
            by_role: item.spend(),
            qwen: item.qwen,
            shots_failed: item.shots.as_ref().and_then(|r| r.run.failed.as_deref()),
            timings,
        }
    }
}

/// A trigger, read
enum Request {
    Status,
    Start,
    Pause,
    Add(u64),
    Rework(u64),
    Adopt(u64),
    Rule(u64, Answer),
    RelayRule(u64, Answer),
    Gate(Option<u64>),
    Drop(Option<u64>),
    Timings(usize),
}

/// Answers one trigger with a JSON body: the status, or `{"error": ...}`
///
/// Blank params count as none. `add` takes an issue number, `rework` and
/// `adopt` a pull request number, `rule` and `relay-rule` take `<id> yes`,
/// `<id> no <note>` or `<id> answer <text>`, `gate` and `drop` take the
/// issue of the work item they are about when more than one is open,
/// `timings` takes how many finished work items to total (ten when left
/// out) and answers the totals, not the status, and every other action takes
/// nothing.
///
/// A ruling the relay was sent, settled by anything but `relay-rule`, is
/// told to it.
pub fn answer(runner: &Mutex<Runner>, action: &str, params: Option<&str>) -> String {
    let error = |message: String| serde_json::json!({ "error": message }).to_string();
    let request = match read(action, params.map(str::trim).filter(|p| !p.is_empty())) {
        Ok(request) => request,
        Err(e) => return error(e),
    };
    // Memory changes only after a save succeeds, so a panicked holder
    // cannot have left the runner half changed.
    let mut runner = lock(runner);
    let totals_of = match &request {
        Request::Timings(last) => Some(*last),
        _ => None,
    };
    let changed = match request {
        Request::Status | Request::Timings(_) => Ok(()),
        Request::Start => runner.start().map_err(|e| e.to_string()),
        Request::Pause => runner.pause().map_err(|e| e.to_string()),
        Request::Add(issue) => runner.add(issue).map(drop).map_err(|e| e.to_string()),
        Request::Rework(number) => runner.rework(number).map(drop).map_err(|e| e.to_string()),
        Request::Adopt(number) => runner.adopt(number).map_err(|e| e.to_string()),
        Request::Rule(id, answer) => runner.rule_and_tell(id, answer).map_err(|e| e.to_string()),
        Request::RelayRule(id, answer) => runner.rule(id, answer).map_err(|e| e.to_string()),
        Request::Gate(issue) => runner.gate(issue).map_err(|e| e.to_string()),
        Request::Drop(issue) => {
            let relayed = runner.relayed();
            let dropped = runner.drop_work_item(issue).map_err(|e| e.to_string());
            if dropped.is_ok() {
                runner.settled_without_relay(&relayed, &Settled::Dropped);
            }
            dropped
        }
    };
    match (changed, totals_of) {
        (Ok(()), Some(last)) => {
            serde_json::to_string(&runner.totals(last)).expect("totals serialize to JSON")
        }
        (Ok(()), None) => {
            serde_json::to_string(&runner.status()).expect("status serializes to JSON")
        }
        (Err(e), _) => error(e),
    }
}

fn read(action: &str, params: Option<&str>) -> Result<Request, String> {
    match (action, params) {
        ("add", Some(p)) => number(p)
            .map(Request::Add)
            .ok_or_else(|| format!("{p:?} is not an issue number")),
        ("add", None) => Err("`add` takes an issue number".into()),
        ("rework", Some(p)) => number(p)
            .map(Request::Rework)
            .ok_or_else(|| format!("{p:?} is not a pull request number")),
        ("rework", None) => Err("`rework` takes a pull request number".into()),
        ("adopt", Some(p)) => number(p)
            .map(Request::Adopt)
            .ok_or_else(|| format!("{p:?} is not a pull request number")),
        ("adopt", None) => Err("`adopt` takes a pull request number".into()),
        ("gate", Some(p)) => number(p)
            .map(|issue| Request::Gate(Some(issue)))
            .ok_or_else(|| format!("{p:?} is not an issue number")),
        ("drop", Some(p)) => number(p)
            .map(|issue| Request::Drop(Some(issue)))
            .ok_or_else(|| format!("{p:?} is not an issue number")),
        ("rule", Some(p)) => read_rule(p)
            .map(|(id, answer)| Request::Rule(id, answer))
            .ok_or_else(|| format!("`{action}` {RULE_USAGE}, not {p:?}")),
        (RELAY_RULE, Some(p)) => read_rule(p)
            .map(|(id, answer)| Request::RelayRule(id, answer))
            .ok_or_else(|| format!("`{action}` {RULE_USAGE}, not {p:?}")),
        ("timings", Some(p)) => number(p)
            .and_then(|n| usize::try_from(n).ok())
            .map(Request::Timings)
            .ok_or_else(|| format!("`timings` takes a count of finished work items, not {p:?}")),
        ("timings", None) => Ok(Request::Timings(TIMINGS_DEFAULT)),
        ("rule" | RELAY_RULE, None) => Err(format!("`{action}` {RULE_USAGE}")),
        (_, _) if !ACTIONS.contains(&action) => Err(format!("unknown action `{action}`")),
        (_, Some(_)) => Err(format!("`{action}` takes no params")),
        ("start", None) => Ok(Request::Start),
        ("pause", None) => Ok(Request::Pause),
        ("gate", None) => Ok(Request::Gate(None)),
        ("drop", None) => Ok(Request::Drop(None)),
        (_, None) => Ok(Request::Status),
    }
}

/// `rule`'s params read: `<id> yes`, `<id> no <note>` or `<id> answer <text>`
pub(super) fn read_rule(params: &str) -> Option<(u64, Answer)> {
    let (id, rest) = params.split_once(char::is_whitespace)?;
    let id = number(id)?;
    let rest = rest.trim_start();
    let answer = match rest.split_once(char::is_whitespace) {
        None if rest == "yes" => Answer::Yes,
        Some(("no", note)) if !note.trim().is_empty() => Answer::No(note.trim().to_owned()),
        Some(("answer", text)) if !text.trim().is_empty() => Answer::Text(text.trim().to_owned()),
        _ => return None,
    };
    Some((id, answer))
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
        Some((_, Answer::No(_) | Answer::Text(_)))
    )
}

// Digits only, so `+7` and `#7` are refused rather than read as 7.
pub(super) fn number(text: &str) -> Option<u64> {
    let n = text.parse::<u64>().ok()?;
    (n > 0 && text.bytes().all(|b| b.is_ascii_digit())).then_some(n)
}

/// Why a trigger could not tell which work item it is about
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhichItem {
    /// No work item is in flight
    NoWorkItem,
    /// These issues' work items are open, and the trigger named none
    Several(Vec<u64>),
    /// No work item is open for this issue
    NotOpen(u64),
}

impl fmt::Display for WhichItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoWorkItem => f.write_str("no work item is in flight"),
            Self::Several(issues) => write!(
                f,
                "the work items for {} are open, so name the issue of one",
                issue_list(issues)
            ),
            Self::NotOpen(issue) => write!(f, "no work item for #{issue} is open"),
        }
    }
}

impl core::error::Error for WhichItem {}

/// `#7`, `#7 and #9`, or `#7, #9 and #12`
pub(super) fn issue_list(issues: &[u64]) -> String {
    let named: Vec<String> = issues.iter().map(|i| format!("#{i}")).collect();
    match named.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => named.concat(),
    }
}

/// Why `gate` was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateError {
    /// The trigger named no work item, or one not open
    Which(WhichItem),
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
            Self::Which(e) => e.fmt(f),
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

impl core::error::Error for GateError {}

impl Runner {
    /// Sends the work item into the gate, when its worker's turn has ended
    /// with a pull request kelpie knows but the gate was not entered
    ///
    /// A work item saved before the gate existed is one such. `issue` names
    /// the work item, and may be left out while only one is open.
    ///
    /// # Errors
    ///
    /// [`GateError`] naming why the work item cannot enter the gate. Nothing
    /// changes then.
    pub fn gate(&mut self, issue: Option<u64>) -> Result<(), GateError> {
        self.choose(issue).map_err(GateError::Which)?;
        let item = self.current().expect("the work item chosen");
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

impl Runner {
    // Works on the work item for `issue`, or on the only one open when no
    // issue is named.
    pub(super) fn choose(&mut self, issue: Option<u64>) -> Result<(), WhichItem> {
        let open = self.state.open_issues();
        let chosen = match (issue, open.as_slice()) {
            (Some(issue), _) if open.contains(&issue) => issue,
            (Some(issue), _) => return Err(WhichItem::NotOpen(issue)),
            (None, []) => return Err(WhichItem::NoWorkItem),
            (None, [only]) => *only,
            (None, _) => return Err(WhichItem::Several(open)),
        };
        self.focus = Some(chosen);
        Ok(())
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
    use crate::skills::Step;
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
    fn issue_lists_read_as_prose() {
        assert_eq!(issue_list(&[7]), "#7");
        assert_eq!(issue_list(&[7, 9]), "#7 and #9");
        assert_eq!(issue_list(&[7, 9, 12]), "#7, #9 and #12");
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
                "merge_authority": "ask",
                "run": "paused",
                "since": Rig::EPOCH,
                "work_item": null,
                "work_items": [],
                "max_items": 1,
                "adopted": [],
                "skipped": [],
                "rulings": [],
                "leases": [],
                "history": [],
                "pacer": { "enabled": true, "claude": { "reading": null, "holding": null } },
                "skills": Step::ALL.map(|step| json!({
                    "step": step.as_str(),
                    "skill": format!("/mattpocock:{}", step.default_skill()),
                    "fallback": null,
                })),
            })
        );
    }

    #[test]
    fn every_registered_action_is_answered_and_no_other() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        for action in ACTIONS
            .into_iter()
            .filter(|a| !["rework", "adopt", "rule", RELAY_RULE, "gate", "drop"].contains(a))
        {
            let params = (action == "add").then_some("7");
            assert_eq!(
                rig.ask(&runner, action, params)["project"],
                "koji",
                "{action}"
            );
        }
        assert_eq!(
            rig.ask(&runner, "rework", Some("71")),
            json!({ "error": "the work item for #7 is in flight" })
        );
        assert_eq!(
            rig.ask(&runner, "adopt", Some("71")),
            json!({ "error": "cannot read pull request #71: gh failed: no pull request #71" })
        );
        assert_eq!(
            rig.ask(&runner, "rule", Some("1 yes")),
            json!({ "error": "no ruling 1 is pending" })
        );
        assert_eq!(
            rig.ask(&runner, RELAY_RULE, Some("1 yes")),
            json!({ "error": "no ruling 1 is pending" })
        );
        assert_eq!(
            rig.ask(&runner, RELAY_RULE, None),
            json!({ "error": "`relay-rule` takes `<id> yes`, `<id> no <note>` or `<id> answer <text>`" })
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
        let (rig, runner, head) = Rig::with_pull_request("webapp");
        drop(runner);
        let state = rig.paths().state;
        let text = std::fs::read_to_string(&state).unwrap();
        let mut saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        let item = saved["work_items"][0].as_object_mut().unwrap();
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
            json!({ "error": "the worker's turn on #7 is due, not ended" })
        );
        assert_eq!(
            rig.ask(&runner, "gate", Some("8")),
            json!({ "error": "no work item for #8 is open" })
        );
        assert_eq!(
            rig.ask(&runner, "gate", Some("#7")),
            json!({ "error": "\"#7\" is not an issue number" })
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
