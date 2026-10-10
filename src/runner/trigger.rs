//! The maintainer's triggers and what `status` shows

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;

use super::gpu::GpuStatus;
use super::pace::PacerStatus;
use super::{Answer, Runner};
use crate::board::Skip;
use crate::lease::gpu::LockHolder;
use crate::ports::{ModelSeat, SessionId, Timestamp};
use crate::review_bot::Bot;
use crate::settings::{AgentName, Merging};
use crate::skills::StepSkill;
use crate::state::{Finished, LeaseHeld, Ruling, StateError, Waiting};
use crate::work_item::{Attached, BotSkipped, Phase, QwenTally, Spend, Split, Turn, WorkItem};

/// The triggers a runner answers
pub const ACTIONS: [&str; 17] = [
    "status", "add", "rework", "adopt", "rule", "gate", "drop", "timings", "attach", "detach",
    "tell", "pm", "drain", "undrain", "finish", "start", "pausing",
];

/// How many finished work items `timings` totals when given no count
const TIMINGS_DEFAULT: usize = 10;

/// How many finished work items `status` lists
pub(super) const STATUS_HISTORY: usize = 10;

/// What `attach` and `detach` take, as their refusals say
const ATTACH_USAGE: &str = "takes an issue number and the attaching process's pid, and \
                            `attach` the pid of the session it started";

/// What `pm` takes, as its refusals say
const PM_USAGE: &str = "takes `attach <pid>`, `attach <pid> <session pid>` or `detach <pid>`";

/// What `rule` takes, as its refusals say
const RULE_USAGE: &str =
    "takes `<id> yes`, `<id> no <note>`, `<id> rework <note>` or `<id> answer <text>`";

/// What `status` answers
#[derive(Debug, Serialize)]
pub struct Status<'a> {
    /// The project
    pub project: &'a str,
    /// Who merges its green, reviewed pull requests, `git.merging`
    pub merging: Merging,
    /// The first of `work_items`, where a status read before they existed
    /// finds the work item
    pub work_item: Option<WorkItemStatus<'a>>,
    /// Every open work item, oldest first
    pub work_items: Vec<WorkItemStatus<'a>>,
    /// How many work items may hold a slot at once. `working` can list
    /// more, since an item going on through CI or a merge holds none.
    pub active_items: u32,
    /// The issues of the open work items going on, neither parked nor
    /// waiting for a slot, oldest first
    pub working: Vec<u64>,
    /// The issues of the work items that gave their slot up to a ruling and
    /// need one again for a model call, each waiting for one, oldest first
    pub waiting_for_slot: Vec<u64>,
    /// The issues of the work items parked on rulings, which take no slot,
    /// oldest first
    pub parked: Vec<u64>,
    /// How many work items may wait parked on rulings before the board
    /// opens nothing new. A merged item on its follow-up ruling, which
    /// `parked` lists, does not count.
    pub pending_rulings: u32,
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
    /// The board briefing the project manager's agent reads
    pub board: &'a Path,
    /// The project manager, when the project names one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pm: Option<super::PmStatus<'a>>,
    /// The calls still running, while `drain` holds back new ones
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draining: Option<super::Draining>,
    /// Whether `finish` holds the board back, or the runner finished and
    /// waits for its sheep to stop
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<super::Run>,
    /// Until when the runner makes no forge call, the forge's rate limit
    /// used up, in seconds since the Unix epoch; left out when it makes them
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forge_held_until: Option<Timestamp>,
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
    /// The implementer its worker runs on
    pub agent: &'a AgentName,
    /// Whether its `agent:` label pins it to that implementer
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Its worker's turn in flight, while that waits for its model
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting: Option<super::flight::Waiting>,
    /// The worker's session, which the maintainer can resume by hand
    pub session: &'a SessionId,
    /// Where the worker's turn stands
    pub turn: &'a Turn,
    /// Where it stands between the worker's turns and the merge
    pub phase: &'a Phase,
    /// The worker's draft pull request, once kelpie has seen it
    pub pull_request: Option<u64>,
    /// The reads each review bot has made of its pull request
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub bot_reads: &'a BTreeMap<Bot, u32>,
    /// The listed review bots its pass went on without, and why: one whose
    /// window opens more than an hour on, or one that never answered
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub bots_skipped: &'a [BotSkipped],
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
    /// The maintainer's `attach` holding it, while one does
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attached: Option<&'a Attached>,
    /// The local reviewers that reviewed nothing twice, so the review goes on
    /// without them
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub local_reviewers_down: Vec<&'a AgentName>,
    /// The reviewers whose calls failed so often in a row that a pass went
    /// on without them
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub reviewers_skipped: &'a [AgentName],
    /// Why its last review pass ended with no reviewer having read it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreviewed: Option<&'a str>,
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
            agent: &item.agent,
            pinned: item.pinned,
            waiting: None,
            session: &item.session,
            turn: &item.turn,
            phase: &item.phase,
            pull_request: item.pull_request,
            bot_reads: &item.bot_reads,
            bots_skipped: &item.bots_skipped,
            calls: item.calls.len(),
            cost_usd: item.cost().usd(),
            unpriced_calls: item.calls.iter().filter(|c| c.unpriced).count(),
            by_role: item.spend(),
            qwen: item.qwen,
            attached: item.attached.as_ref(),
            local_reviewers_down: item.local_reviewers_down(),
            reviewers_skipped: &item.reviewers_skipped,
            unreviewed: item.unreviewed.as_deref(),
            timings,
        }
    }
}

/// A trigger, read
enum Request {
    Status,
    Add(u64),
    Rework(u64),
    Adopt(u64),
    Rule(u64, Answer),
    Gate(Option<u64>),
    Drop(Option<u64>),
    Timings(usize),
    Attach(u64, u32, Option<u32>),
    Detach(u64, u32),
    Tell(String),
    PmAttach(u32, Option<u32>),
    PmDetach(u32),
    Drain,
    Undrain,
    Finish,
    Start,
    Pausing,
}

/// Answers one trigger with a JSON body: the status, or `{"error": ...}`
///
/// Blank params count as none. `add` takes an issue number, `rework` and
/// `adopt` a pull request number, `rule` takes `<id> yes`, `<id> no <note>`,
/// `<id> rework <note>` or `<id> answer <text>`, `gate` and `drop` take the
/// issue of the work item they are about when more than one is open,
/// `timings` takes how many finished work items to total (ten when left
/// out) and answers the totals, not the status, `attach` and `detach` take
/// an issue and the attaching process's pid, `attach` then the pid of the
/// session it started, if it has, and answers
/// [`Attaching`](super::Attaching) rather than the status, `tell` takes a
/// note for the project manager, `pm` takes `attach` or `detach` with the
/// same pids and answers [`PmAttaching`](super::PmAttaching) for `attach`,
/// and every other action takes nothing. `drain` holds back every new call
/// and answers the status with the calls still running, and `undrain` lets
/// calls start again. `finish` holds the board back until the open work
/// items end, `start` lets it pick again, and `pausing`, which `pause` sends
/// before its stop, clears the saved `finishing` so the runner comes back picking.
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
        Request::Attach(issue, pid, session) => {
            return match runner.attach(issue, pid, session) {
                Ok(attaching) => serde_json::to_string(&attaching).expect("it serializes to JSON"),
                Err(e) => error(e.to_string()),
            };
        }
        Request::Detach(issue, pid) => runner.detach(issue, pid).map_err(|e| e.to_string()),
        Request::PmAttach(pid, session) => {
            return match runner.attach_pm(pid, session) {
                Ok(attaching) => serde_json::to_string(&attaching).expect("it serializes to JSON"),
                Err(e) => error(e.to_string()),
            };
        }
        Request::PmDetach(pid) => runner.detach_pm(pid).map_err(|e| e.to_string()),
        Request::Tell(note) => runner.tell(&note).map_err(|e| e.to_string()),
        Request::Status | Request::Timings(_) => Ok(()),
        Request::Drain => {
            runner.drain();
            Ok(())
        }
        Request::Undrain => {
            runner.undrain();
            Ok(())
        }
        Request::Finish => runner.begin_finishing().map_err(|e| e.to_string()),
        Request::Start => runner.cancel_finishing().map_err(|e| e.to_string()),
        Request::Pausing => runner.pausing().map_err(|e| e.to_string()),
        Request::Add(issue) => runner.add(issue).map(drop).map_err(|e| e.to_string()),
        Request::Rework(number) => runner.rework(number).map(drop).map_err(|e| e.to_string()),
        Request::Adopt(number) => runner.adopt(number).map_err(|e| e.to_string()),
        Request::Rule(id, answer) => runner.rule(id, answer).map_err(|e| e.to_string()),
        Request::Gate(issue) => runner.gate(issue).map_err(|e| e.to_string()),
        Request::Drop(issue) => runner.drop_work_item(issue).map_err(|e| e.to_string()),
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

/// Whether `action` asks the runner's loop for a pass
///
/// `status` and `timings` only ask, and `drain` only sets what the next
/// pass reads, so none wakes the loop: a drain's wait asks `drain` and
/// `status` twice a second. `undrain` wakes it, to start the calls it lets go.
pub fn wakes(action: &str) -> bool {
    !matches!(action, "status" | "timings" | "drain")
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
        ("timings", Some(p)) => number(p)
            .and_then(|n| usize::try_from(n).ok())
            .map(Request::Timings)
            .ok_or_else(|| format!("`timings` takes a count of finished work items, not {p:?}")),
        ("timings", None) => Ok(Request::Timings(TIMINGS_DEFAULT)),
        ("attach" | "detach", Some(p)) => {
            let words: Vec<&str> = p.split_whitespace().collect();
            let read = match (action, words.as_slice()) {
                ("attach", [issue, pid]) => number(issue)
                    .zip(pid_of(pid))
                    .map(|(issue, pid)| Request::Attach(issue, pid, None)),
                ("attach", [issue, pid, session]) => number(issue)
                    .zip(pid_of(pid).zip(pid_of(session)))
                    .map(|(issue, (pid, session))| Request::Attach(issue, pid, Some(session))),
                ("detach", [issue, pid]) => number(issue)
                    .zip(pid_of(pid))
                    .map(|(issue, pid)| Request::Detach(issue, pid)),
                _ => None,
            };
            read.ok_or_else(|| format!("`{action}` {ATTACH_USAGE}, not {p:?}"))
        }
        ("attach" | "detach", None) => Err(format!("`{action}` {ATTACH_USAGE}")),
        ("tell", Some(note)) => Ok(Request::Tell(note.to_owned())),
        ("tell", None) => Err("`tell` takes a note for the project manager".into()),
        ("pm", Some(p)) => {
            let words: Vec<&str> = p.split_whitespace().collect();
            let read = match words.as_slice() {
                ["attach", pid] => pid_of(pid).map(|pid| Request::PmAttach(pid, None)),
                ["attach", pid, session] => (pid_of(pid).zip(pid_of(session)))
                    .map(|(pid, session)| Request::PmAttach(pid, Some(session))),
                ["detach", pid] => pid_of(pid).map(Request::PmDetach),
                _ => None,
            };
            read.ok_or_else(|| format!("`pm` {PM_USAGE}, not {p:?}"))
        }
        ("pm", None) => Err(format!("`pm` {PM_USAGE}")),
        ("rule", None) => Err(format!("`{action}` {RULE_USAGE}")),
        (_, _) if !ACTIONS.contains(&action) => Err(format!("unknown action `{action}`")),
        (_, Some(_)) => Err(format!("`{action}` takes no params")),
        ("gate", None) => Ok(Request::Gate(None)),
        ("drop", None) => Ok(Request::Drop(None)),
        ("drain", None) => Ok(Request::Drain),
        ("undrain", None) => Ok(Request::Undrain),
        ("finish", None) => Ok(Request::Finish),
        ("start", None) => Ok(Request::Start),
        ("pausing", None) => Ok(Request::Pausing),
        (_, None) => Ok(Request::Status),
    }
}

/// `rule`'s params read: `<id> yes`, `<id> no <note>`, `<id> rework <note>`
/// or `<id> answer <text>`
pub(super) fn read_rule(params: &str) -> Option<(u64, Answer)> {
    let (id, rest) = params.split_once(char::is_whitespace)?;
    let id = number(id)?;
    let rest = rest.trim_start();
    let answer = match rest.split_once(char::is_whitespace) {
        None if rest == "yes" => Answer::Yes,
        Some(("no", note)) if !note.trim().is_empty() => Answer::No(note.trim().to_owned()),
        Some(("rework", note)) if !note.trim().is_empty() => Answer::Rework(note.trim().to_owned()),
        Some(("answer", text)) if !text.trim().is_empty() => Answer::Text(text.trim().to_owned()),
        _ => return None,
    };
    Some((id, answer))
}

// A pid that names one process: not 0, and not past `i32::MAX`, which
// a signal would take as a negative process group.
fn pid_of(text: &str) -> Option<u32> {
    number(text)
        .filter(|&pid| pid <= i32::MAX.unsigned_abs().into())
        .and_then(|pid| u32::try_from(pid).ok())
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
    fn a_new_project_has_nothing_in_flight() {
        let rig = Rig::new("koji");
        rig.retro_on();
        let runner = rig.open().unwrap();
        assert_eq!(
            rig.ask(&runner, "status", None),
            json!({
                "project": "koji",
                "merging": "ask",
                "work_item": null,
                "work_items": [],
                "active_items": 1,
                "working": [],
                "waiting_for_slot": [],
                "parked": [],
                "pending_rulings": 2,
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
                "board": rig.paths().board,
            })
        );
    }

    #[test]
    fn every_registered_action_is_answered_and_no_other() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        for action in ACTIONS.into_iter().filter(|a| {
            ![
                "rework", "adopt", "rule", "gate", "drop", "attach", "detach", "tell", "pm",
            ]
            .contains(a)
        }) {
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
        let no_pm = "the project has no project manager: name one in `agents.pm`, such as `pm`";
        let me = std::process::id();
        assert_eq!(
            rig.ask(&runner, "tell", Some("hold #4")),
            json!({ "error": no_pm })
        );
        assert_eq!(
            rig.ask(&runner, "pm", Some(&format!("attach {me}"))),
            json!({ "error": no_pm })
        );
        assert_eq!(
            rig.ask(&runner, "pm", Some(&format!("detach {me}"))),
            json!({ "error": no_pm })
        );
        assert_eq!(
            rig.ask(&runner, "pm", Some("attach 0")),
            json!({ "error": "`pm` takes `attach <pid>`, `attach <pid> <session pid>` or \
                              `detach <pid>`, not \"attach 0\"" })
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
            rig.ask(&runner, "rule", None),
            json!({ "error": "`rule` takes `<id> yes`, `<id> no <note>`, `<id> rework <note>` or `<id> answer <text>`" })
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
            rig.ask(&runner, "drain", Some("now")),
            json!({ "error": "`drain` takes no params" })
        );
        assert_eq!(rig.ask(&runner, "status", None)["draining"], json!(null));
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
