//! The maintainer's triggers and what `status` shows

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::{Mutex, PoisonError, TryLockError};

use serde::Serialize;

use super::gpu::GpuStatus;
use super::lock_hold::Held;
use super::pace::PacerStatus;
use super::posted::{Body, Posted, Queued};
use super::timings;
use super::{RuleError, Runner};
use crate::board::Skip;
use crate::ports::PaddockHolder;
use crate::ports::{SessionId, Timestamp};
use crate::reply::Reply;
use crate::request::{Request, RuleAction, RuleAnswer};
use crate::review_bot::Bot;
use crate::settings::{AgentName, Merging, PerItemUsd};
use crate::skills::{RepoPlugin, StepSkill};
use crate::state::{Finished, LeaseHeld, Ruling, StateError, Waiting};
use crate::ticket::TicketId;
use crate::work_item::{
    Attached, BotSkipped, Phase, QwenTally, Spend, Split, TestCheck, Turn, WorkItem,
};

// The builder from the runner's borrowed status types to the protocol's owned ones
mod wire;

/// The triggers a runner answers
pub const ACTIONS: [&str; 19] = [
    "status", "add", "rework", "adopt", "rule", "gate", "drop", "timings", "attach", "detach",
    "tell", "pm", "drain", "undrain", "finish", "start", "pausing", "board", "triage",
];

/// How many finished work items `timings` totals when given no count
const TIMINGS_DEFAULT: usize = 10;

/// How many finished work items `status` lists
pub(super) const STATUS_HISTORY: usize = 10;

/// How many issues of the board's order `status` lists
pub(super) const STATUS_ORDER: usize = 5;

/// What `attach` and `detach` take, as their refusals say
const ATTACH_USAGE: &str = "takes a ticket id and the attaching process's pid, and \
                            `attach` the pid of the session it started";

/// What `pm` takes, as its refusals say
const PM_USAGE: &str = "takes `attach <pid>`, `attach <pid> <session pid>` or `detach <pid>`";

/// What `rule` takes, as its refusals say
const RULE_USAGE: &str =
    "takes `<id> yes`, `<id> no <note>`, `<id> rework <note>` or `<id> answer <text>`";

/// What `board` takes, as its refusals say
const BOARD_USAGE: &str = "takes `move <issue> <position>` or `top <issue>`, or nothing to list";

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
    /// The tickets of the open work items going on, neither parked nor
    /// waiting for a slot, oldest first
    pub working: Vec<TicketId>,
    /// The tickets of the work items that gave their slot up to a ruling and
    /// need one again for a model call, each waiting for one, oldest first
    pub waiting_for_slot: Vec<TicketId>,
    /// The tickets of the work items parked on rulings, which take no slot,
    /// oldest first
    pub parked: Vec<TicketId>,
    /// How many work items may wait parked on rulings before the board
    /// opens nothing new. A merged item on its follow-up ruling, which
    /// `parked` lists, does not count.
    pub pending_rulings: u32,
    /// Pull requests adopted and waiting for a free slot, oldest first
    pub adopted: &'a [Waiting],
    /// The pull requests from other people the board follows, oldest first;
    /// left out when it follows none
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contributions: Vec<super::contributions::Followed<'a>>,
    /// The bumps from bots the board follows, oldest first, with where each
    /// stands; left out when it follows none
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bumps: Vec<super::dependencies::BumpStatus>,
    /// The issues from other people that triage would pick up, oldest
    /// first; left out while none is
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triage: Option<super::triage::TriageStatus<'a>>,
    /// The first issues in the order the board takes the ready ones, which
    /// holds those it would pass over too
    pub order: &'a [TicketId],
    /// Ready issues the board passed over on its last poll, and why
    pub skipped: &'a [Skip],
    /// The warnings that stand now, each told once in the log, such as a
    /// code graph server that is not installed; left out while none does
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub standing: Vec<String>,
    /// The workflows whose latest completed run on `main` failed, in the
    /// order they went red; left out while none is
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub red_workflows: Vec<super::RedStatus>,
    /// Rulings waiting on the maintainer, oldest first
    pub rulings: Vec<RulingStatus<'a>>,
    /// How many rulings the project manager answered, by kind; left out
    /// while it answered none
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub pm_answers: BTreeMap<String, u32>,
    /// Leases held
    pub leases: &'a [LeaseHeld],
    /// The most recent finished work items, oldest first
    pub history: &'a [Finished],
    /// What usage was when last read, and why nothing new is starting
    pub pacer: PacerStatus<'a>,
    /// The skill each step runs, and why any chosen one could not load
    pub skills: &'a [StepSkill],
    /// The plugins the repo turns on, with their skills or why each was
    /// skipped; left out when it turns on none
    #[serde(skip_serializing_if = "<[RepoPlugin]>::is_empty")]
    pub plugins: &'a [RepoPlugin],
    /// Where a worker's pull request template comes from: the repo, its
    /// owner's `.github` repository, or none
    pub pull_request_template: super::TemplateSource,
    /// The GPU's load as the metrics page last said, when kelpie's settings
    /// name one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuStatus>,
    /// The free space on the disk holding kelpie's home when last measured,
    /// in bytes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_free_bytes: Option<u64>,
    /// Who holds the gateway's lease on each model a local agent calls, by
    /// the model's name, or null while it is free
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub local_leases: BTreeMap<String, Option<PaddockHolder>>,
    /// The board briefing the project manager's agent reads
    pub board: &'a Path,
    /// The project manager, when the project names one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pm: Option<super::PmStatus<'a>>,
    /// The size and modification time of the file this runner started from,
    /// which `upgrade` reads to tell a runner on the installed build; left
    /// out when the file cannot be read
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<&'static str>,
    /// The commit the build this runner runs was made from, with `-dirty`
    /// when that build's tree held changes that commit does not; left out for
    /// a build made where git could not answer
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<&'static str>,
    /// The calls still running, while `drain` holds back new ones
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draining: Option<super::Draining>,
    /// Whether `finish` holds the board back, or the runner finished and
    /// waits for its sheep to stop
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<crate::board::briefing::Run>,
    /// How many seconds the runner goes between two reads of its board now:
    /// 60 while a work item is open or the board moves, doubling up to 300
    /// while it is idle and the board shows nothing new
    pub board_poll_secs: u64,
    /// Until when the runner makes no forge call, the forge's rate limit
    /// used up, in seconds since the Unix epoch; left out when it makes them
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forge_held_until: Option<Timestamp>,
    /// Why the stored settings are refused and since when, while they
    /// are: the runner keeps the settings it has; left out otherwise
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_refused: Option<&'a super::Refusal>,
    /// The longest hold of the runner's lock in seconds since it started, by
    /// the source line that took it; left out until a hold is recorded
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub lock: BTreeMap<String, f64>,
}

/// A waiting ruling, as `status` shows it: the ruling as saved, with its
/// `id` holding the number the maintainer answers it by
#[derive(Debug)]
pub struct RulingStatus<'a>(pub &'a Ruling);

impl Serialize for RulingStatus<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut shown = serde_json::to_value(self.0).map_err(serde::ser::Error::custom)?;
        shown["id"] = serde_json::Value::String(self.0.number().to_string());
        shown.serialize(serializer)
    }
}

/// An open work item, as `status` shows it
#[derive(Debug, Serialize)]
pub struct WorkItemStatus<'a> {
    /// The ticket it resolves
    pub issue: TicketId,
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
    /// Whether its `agent:` label or the project manager's pick pins it to that implementer
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Its worker's turn in flight, while that waits for its model
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting: Option<super::flight::Waiting>,
    /// What its due worker turn waits on, while its last step left that turn
    /// unstarted
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held_up: Option<super::turn::held_up::HeldUp>,
    /// Its step that failed again and again, from the second failure in a row
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failing: Option<super::looks::Failing>,
    /// `"none"` while its wait on CI has had no check reported, beside the
    /// wait's `since` in `phase`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<&'static str>,
    /// What its build folder held when last measured, in bytes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_bytes: Option<u64>,
    /// The worker's session, which the maintainer can resume by hand
    pub session: &'a SessionId,
    /// Where the worker's turn stands
    pub turn: &'a Turn,
    /// Where it stands between the worker's turns and the merge
    pub phase: &'a Phase,
    /// The worker's draft pull request, once kelpie has seen it
    pub pull_request: Option<u64>,
    /// The head whose failed CI jobs kelpie reran, while the rerun runs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerunning: Option<&'a str>,
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
    /// What its priced calls have cost against its cap, in US dollars
    pub budget: BudgetStatus,
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
    /// What kelpie ran of the tests reviewers attached to findings: the
    /// outcome before the fix and after it, per finding
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub test_checks: &'a [TestCheck],
    /// Where its wall time went, and the phase it is in now
    pub timings: Split,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl<'a> WorkItemStatus<'a> {
    /// `item` as `status` shows it, against a cap of `per_item` before any
    /// raise, and with its time read as `timings`
    pub(super) fn new(item: &'a WorkItem, per_item: PerItemUsd, timings: Split) -> Self {
        Self {
            issue: item.issue,
            title: &item.title,
            branch: &item.branch,
            adopted: item.adopted,
            worktree: &item.worktree,
            agent: &item.agent,
            pinned: item.pinned,
            waiting: None,
            held_up: None,
            failing: None,
            checks: None,
            build_bytes: None,
            session: &item.session,
            turn: &item.turn,
            phase: &item.phase,
            pull_request: item.pull_request,
            rerunning: item.rerunning(),
            bot_reads: &item.bot_reads,
            bots_skipped: &item.bots_skipped,
            calls: item.calls.len(),
            cost_usd: item.cost().usd(),
            unpriced_calls: item.calls.iter().filter(|c| c.unpriced).count(),
            by_role: item.spend(),
            budget: BudgetStatus {
                spent_usd: item.priced().total().usd(),
                cap_usd: item.budget_cap(per_item).usd(),
            },
            qwen: item.qwen,
            attached: item.attached.as_ref(),
            local_reviewers_down: item.local_reviewers_down(),
            reviewers_skipped: &item.reviewers_skipped,
            unreviewed: item.unreviewed.as_deref(),
            test_checks: &item.test_checks,
            timings,
        }
    }
}

/// A work item's priced spend against its cap, as `status` shows it
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BudgetStatus {
    /// What its worker's and reviewers' priced calls have cost
    pub spent_usd: f64,
    /// What they may cost before its worker's next turn waits on a ruling
    pub cap_usd: f64,
}

/// Answers one trigger with a JSON body: the status, or `{"error": ...}`
///
/// Blank params count as none. `rule` takes `<id> yes`, `<id> no <note>`,
/// `<id> rework <note>` or `<id> answer <text>`. `attach` and `detach` take a
/// ticket and a pid, and answer [`Attaching`](super::Attaching) rather than
/// the status. `drain` holds back every new call, `drain pause` also ends each
/// worker turn at its next tool call, and `undrain` lets calls start again.
/// `status`, `drain` and `undrain` come from what the loop last posted.
pub fn answer(
    runner: &Mutex<Runner>,
    posted: &Posted,
    action: &str,
    params: Option<&str>,
) -> String {
    match request_of(action, params) {
        Ok(request) => body(&answer_request(runner, posted, &request)),
        Err(e) => body(&Reply::Refused(e)),
    }
}

/// The request `shep trigger <action> [params]` reads as
///
/// Blank params count as none. The socket's verbs read their text through
/// this too, so a request typed and one sent as text cannot disagree.
///
/// # Errors
///
/// The text the runner refuses with: an unknown action, or params the action
/// does not take.
pub fn request_of(action: &str, params: Option<&str>) -> Result<Request, String> {
    read(action, params.map(str::trim).filter(|p| !p.is_empty()))
}

/// Answers `request` of `runner`, taking its lock unless a pass holds it and the
/// posted copy has the answer
///
/// What both `shep trigger` and the runner's socket call, so the two cannot drift.
pub fn answer_request(runner: &Mutex<Runner>, posted: &Posted, request: &Request) -> Reply {
    let held = try_lock(runner);
    if held.is_none()
        && let Some(reply) = from_posted(posted, request)
    {
        return reply;
    }
    // Memory changes only after a save succeeds, so a panicked holder
    // cannot have left the runner half changed.
    let mut runner = held.unwrap_or_else(|| lock(runner));
    handle(&mut runner, posted, request)
}

/// The body a reply answers with
fn body(reply: &Reply) -> String {
    serde_json::to_string(reply).expect("the answer serializes to JSON")
}

/// Answers `request` of `runner`, whose lock the caller holds
///
/// [`answer_request`] takes the lock and calls this, for `shep trigger` and the
/// runner's socket alike.
fn handle(runner: &mut Runner, posted: &Posted, request: &Request) -> Reply {
    let totals_of = match request {
        Request::Timings(last) => Some(*last),
        _ => None,
    };
    let deferred = defer(runner, posted, request);
    let queued = queued_verb(request)
        .filter(|_| !deferred)
        .map(|verb| verb.to_string());
    let changed = match request {
        _ if deferred => Ok(()),
        Request::Attach(ticket, pid, session) => {
            return match runner.attach(*ticket, *pid, *session) {
                Ok(attaching) => {
                    runner.post();
                    Reply::Attaching(attaching.into())
                }
                Err(e) => Reply::Refused(e.to_string()),
            };
        }
        Request::Detach(ticket, pid) => runner.detach(*ticket, *pid).map_err(|e| e.to_string()),
        Request::PmAttach(pid, session) => {
            return match runner.attach_pm(*pid, *session) {
                Ok(attaching) => {
                    runner.post();
                    Reply::PmAttaching(attaching.into())
                }
                Err(e) => Reply::Refused(e.to_string()),
            };
        }
        Request::PmDetach(pid) => runner.detach_pm(*pid).map_err(|e| e.to_string()),
        Request::Board(moving) => {
            if let Some((issue, position)) = moving
                && let Err(e) = runner.move_on_board(*issue, *position)
            {
                return Reply::Refused(e.to_string());
            }
            runner.post();
            return Reply::Board(runner.board().into());
        }
        Request::Tell(note) => runner.tell(note).map_err(|e| e.to_string()),
        Request::Status | Request::Timings(_) => Ok(()),
        Request::Drain { at_tool } => {
            runner.drain(*at_tool);
            Ok(())
        }
        Request::Undrain { keep } => {
            runner.undrain(*keep);
            Ok(())
        }
        Request::Finish => runner.begin_finishing().map_err(|e| e.to_string()),
        Request::Start => runner.cancel_finishing().map_err(|e| e.to_string()),
        Request::Pausing => runner.pausing().map_err(|e| e.to_string()),
        Request::Add(ticket) => runner.add(*ticket).map(drop).map_err(|e| e.to_string()),
        Request::Rework(number) => runner.rework(*number).map(drop).map_err(|e| e.to_string()),
        Request::Adopt(number) => runner.adopt(*number).map_err(|e| e.to_string()),
        Request::Rule(id, answer) => runner
            .rule(*id, answer.clone().into())
            .map_err(|e| e.to_string()),
        Request::Gate(ticket) => runner.gate(*ticket).map_err(|e| e.to_string()),
        Request::Drop(ticket) => runner.drop_work_item(*ticket).map_err(|e| e.to_string()),
        Request::Triage(issue) => runner.triage_ask(*issue),
        // The socket serves a watch before it gets here, and no trigger reads as one.
        Request::Watch => return Reply::Refused("`watch` is answered by the socket alone".into()),
    };
    // A refusal the queue left stands until the verb goes through, however it did.
    if let (Ok(()), Some(key)) = (&changed, &queued) {
        runner.notes.ends(key);
    }
    // What a trigger asks while a pass holds the lock reads this copy.
    runner.post();
    match (changed, totals_of) {
        (Ok(()), Some(last)) => Reply::Timings(runner.totals(last).into()),
        (Ok(()), None) => Reply::Status(Box::new((&runner.status()).into())),
        (Err(e), _) => Reply::Refused(e),
    }
}

/// Whether `action` asks the runner's loop for a pass
///
/// `status`, `timings` and `watch` only ask, and `drain` starts its retro
/// itself, so none wakes the loop: a drain's wait asks `drain` and
/// `status` twice a second. `undrain` wakes it, to start the calls it lets go,
/// and so does each verb that is queued while a pass holds the lock.
pub fn wakes(action: &str) -> bool {
    !matches!(action, "status" | "timings" | "drain" | "watch")
}

// Answers `request` from what the loop last posted, for a runner whose lock a
// pass holds. Only the triggers that ask have a posted answer: `status`,
// `timings` and the `board` listing. A `drain` or `undrain` answered that way
// sets its flag here and leaves the rest for the pass at its next lock.
fn from_posted(posted: &Posted, request: &Request) -> Option<Reply> {
    if let Some(verb) = queued_verb(request) {
        let body = posted.body()?;
        // A ruling the posted status does not list is refused here, since the
        // reply is the only place its refusal would reach the maintainer.
        if let Request::Rule(id, _) = request
            && !ruling_waits(&body.status, *id)
        {
            return Some(Reply::Refused(RuleError::NoSuchRuling(*id).to_string()));
        }
        // The answer is read back before the verb is queued, so a post this
        // crate cannot read leaves the verb for the pass, which applies it once.
        let answer = status_of(posted, &body)?;
        if !posted.ask_queued(verb) {
            return None;
        }
        Some(answer)
    } else if let Request::Timings(last) = request {
        let body = posted.body()?;
        let totals = timings::totals_of(&body.project, &body.history, *last);
        Some(Reply::Timings(totals.into()))
    } else if matches!(request, Request::Board(None)) {
        let body = posted.body()?;
        serde_json::from_value(body.board).ok().map(Reply::Board)
    } else {
        let asked = match request {
            Request::Status => true,
            Request::Drain { at_tool } => {
                posted.ask_drain(*at_tool);
                true
            }
            Request::Undrain { keep } => {
                posted.ask_undrain(*keep);
                true
            }
            _ => false,
        };
        if !asked {
            return None;
        }
        status_of(posted, &posted.body()?)
    }
}

// Whether `request` is left for the end of the job that is changing the worktree it would
// change: a `drop` is marked, a `rework` or `adopt` is queued, and an `attach` is told to wait
// where it is answered.
fn defer(runner: &mut Runner, posted: &Posted, request: &Request) -> bool {
    match request {
        Request::Drop(ticket)
            if runner
                .busy
                .is_changing(*ticket, &runner.state.open_tickets()) =>
        {
            runner.busy.drop_after();
            true
        }
        Request::Rework(_) | Request::Adopt(_) if runner.busy.any() => {
            queued_verb(request).is_some_and(|verb| posted.ask_queued(verb))
        }
        _ => false,
    }
}

// The verb `request` queues for the pass, when it is one that reads the forge.
fn queued_verb(request: &Request) -> Option<Queued> {
    Some(match request {
        Request::Add(ticket) => Queued::Add(*ticket),
        Request::Rework(number) => Queued::Rework(*number),
        Request::Adopt(number) => Queued::Adopt(*number),
        Request::Triage(issue) => Queued::Triage(*issue),
        Request::Rule(id, answer) => Queued::Rule(*id, answer.clone().into()),
        _ => return None,
    })
}

// Whether the posted `status` lists a ruling the maintainer answers by `id`.
fn ruling_waits(status: &serde_json::Value, id: TicketId) -> bool {
    let named = id.to_string();
    (status["rulings"].as_array())
        .is_some_and(|rulings| rulings.iter().any(|r| r["id"].as_str() == Some(&named)))
}

// The posted status as a reply, with `draining` as the flag stands now.
//
// The post is read back as the protocol's owned status. A post the protocol
// cannot read is no answer, so the trigger waits for the lock and asks the runner.
fn status_of(posted: &Posted, body: &Body) -> Option<Reply> {
    let Body { status, drain, .. } = body;
    let mut status = status.clone();
    if posted.draining() {
        status["draining"] = drain.clone();
    } else {
        (status.as_object_mut()?).remove("draining");
    }
    serde_json::from_value(status)
        .ok()
        .map(|status| Reply::Status(Box::new(status)))
}

fn read(action: &str, params: Option<&str>) -> Result<Request, String> {
    match (action, params) {
        ("add", Some(p)) => ticket(p)
            .map(Request::Add)
            .ok_or_else(|| format!("{p:?} is not a ticket id")),
        ("add", None) => Err("`add` takes a ticket id".into()),
        ("rework", Some(p)) => number(p)
            .map(Request::Rework)
            .ok_or_else(|| format!("{p:?} is not a pull request number")),
        ("rework", None) => Err("`rework` takes a pull request number".into()),
        ("adopt", Some(p)) => number(p)
            .map(Request::Adopt)
            .ok_or_else(|| format!("{p:?} is not a pull request number")),
        ("adopt", None) => Err("`adopt` takes a pull request number".into()),
        ("triage", Some(p)) => number(p)
            .map(Request::Triage)
            .ok_or_else(|| format!("{p:?} is not an issue number")),
        ("triage", None) => Err("`triage` takes an issue number".into()),
        ("gate", Some(p)) => ticket(p)
            .map(|ticket| Request::Gate(Some(ticket)))
            .ok_or_else(|| format!("{p:?} is not a ticket id")),
        ("drop", Some(p)) => ticket(p)
            .map(|ticket| Request::Drop(Some(ticket)))
            .ok_or_else(|| format!("{p:?} is not a ticket id")),
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
                ("attach", [ticket_id, pid]) => ticket(ticket_id)
                    .zip(pid_of(pid))
                    .map(|(ticket, pid)| Request::Attach(ticket, pid, None)),
                ("attach", [ticket_id, pid, session]) => ticket(ticket_id)
                    .zip(pid_of(pid).zip(pid_of(session)))
                    .map(|(ticket, (pid, session))| Request::Attach(ticket, pid, Some(session))),
                ("detach", [ticket_id, pid]) => ticket(ticket_id)
                    .zip(pid_of(pid))
                    .map(|(ticket, pid)| Request::Detach(ticket, pid)),
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
        ("board", None) => Ok(Request::Board(None)),
        ("board", Some(p)) => {
            let words: Vec<&str> = p.split_whitespace().collect();
            let read = match words.as_slice() {
                // A position of 0 is read, so the runner can say positions start at 1.
                ["move", issue, position] => {
                    let place = position.bytes().all(|b| b.is_ascii_digit());
                    ticket(issue).zip(place.then(|| position.parse().ok()).flatten())
                }
                // The first place, which no position reads as past the end.
                ["top", issue] => ticket(issue).map(|ticket| (ticket, 1)),
                _ => None,
            };
            read.map(|moving| Request::Board(Some(moving)))
                .ok_or_else(|| format!("`board` {BOARD_USAGE}, not {p:?}"))
        }
        // What `pause` sends: drain, and end each worker turn at its next tool call.
        ("drain", Some("pause")) => Ok(Request::Drain { at_tool: true }),
        // What `pause` sends when it gives up: undrain, and keep the turns it ended stopped.
        ("undrain", Some("keep")) => Ok(Request::Undrain { keep: true }),
        ("undrain", Some(p)) => Err(format!("`undrain` takes `keep` or nothing, not {p:?}")),
        (_, _) if !ACTIONS.contains(&action) => Err(format!("unknown action `{action}`")),
        (_, Some(_)) => Err(format!("`{action}` takes no params")),
        ("gate", None) => Ok(Request::Gate(None)),
        ("drop", None) => Ok(Request::Drop(None)),
        ("drain", None) => Ok(Request::Drain { at_tool: false }),
        ("undrain", None) => Ok(Request::Undrain { keep: false }),
        ("finish", None) => Ok(Request::Finish),
        ("start", None) => Ok(Request::Start),
        ("pausing", None) => Ok(Request::Pausing),
        (_, None) => Ok(Request::Status),
    }
}

/// `rule`'s params read: `<id> yes`, `<id> no <note>`, `<id> rework <note>`
/// or `<id> answer <text>`
pub(super) fn read_rule(params: &str) -> Option<(TicketId, RuleAnswer)> {
    let (id, rest) = params.split_once(char::is_whitespace)?;
    let id = ruling_id(id)?;
    let rest = rest.trim_start();
    let answer = match rest.split_once(char::is_whitespace) {
        None if rest == "yes" => RuleAnswer::Yes,
        None if rest == "rerun" => RuleAnswer::Do {
            action: RuleAction::Rerun,
        },
        None if rest == "restore" => RuleAnswer::Do {
            action: RuleAction::Restore,
        },
        Some(("yes", head)) if is_head(head.trim()) => RuleAnswer::Trust {
            head: head.trim().to_owned(),
        },
        Some(("no", note)) if !note.trim().is_empty() => RuleAnswer::No {
            note: note.trim().to_owned(),
        },
        Some(("rework", note)) if !note.trim().is_empty() => RuleAnswer::Rework {
            note: note.trim().to_owned(),
        },
        Some(("answer", text)) if !text.trim().is_empty() => RuleAnswer::Text {
            text: text.trim().to_owned(),
        },
        _ => return None,
    };
    Some((id, answer))
}

// A commit hash, or the start of one: hex digits and nothing else.
pub(super) fn is_head(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit())
}

// A pid that names one process: not 0, and not past `i32::MAX`, which
// a signal would take as a negative process group.
fn pid_of(text: &str) -> Option<u32> {
    number(text)
        .filter(|&pid| pid <= i32::MAX.unsigned_abs().into())
        .and_then(|pid| u32::try_from(pid).ok())
}

// A ticket's id as written: `7` or `k12`, so `+7` and `#7` are refused rather
// than read as 7.
pub(super) fn ticket(text: &str) -> Option<TicketId> {
    text.parse().ok()
}

// The id an answer names a ruling by: a ticket id as `TicketId` spells it, or
// the digits an older kelpie wrote with leading zeros, which name the same
// GitHub ticket. `shep kelpie rule` takes a ticket id as it is spelled; an
// answer left in the `answers` folder by an older kelpie is read either way.
pub(super) fn ruling_id(text: &str) -> Option<TicketId> {
    match text.starts_with('k') {
        true => ticket(text),
        false => ticket(text.trim_start_matches('0')),
    }
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
    /// These tickets' work items are open, and the trigger named none
    Several(Vec<TicketId>),
    /// No work item is open for this issue
    NotOpen(TicketId),
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
            Self::NotOpen(ticket) => match ticket.github() {
                Some(issue) => write!(f, "no work item for #{issue} is open"),
                None => write!(f, "no work item for {ticket} is open"),
            },
        }
    }
}

impl core::error::Error for WhichItem {}

/// `#7`, `#7 and #9`, or `#7, #9 and #12`
pub(super) fn issue_list(tickets: &[TicketId]) -> String {
    let named: Vec<String> = tickets.iter().map(TicketId::shown).collect();
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
    AlreadyGated(TicketId),
    /// The worker's turn has not ended, with the turn's state
    TurnNotEnded(TicketId, &'static str),
    /// Kelpie knows no pull request for the work item
    NoPullRequest(TicketId),
    /// The change could not be saved
    State(StateError),
}

impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Which(e) => e.fmt(f),
            Self::AlreadyGated(issue) => {
                write!(
                    f,
                    "the work item for {} is already in the gate",
                    issue.shown()
                )
            }
            Self::TurnNotEnded(issue, state) => {
                write!(
                    f,
                    "the worker's turn on {} is {state}, not ended",
                    issue.shown()
                )
            }
            Self::NoPullRequest(issue) => {
                write!(f, "no pull request is known for {}", issue.shown())
            }
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for GateError {}

impl Runner {
    /// Sends the work item into the gate, when its worker's turn has ended
    /// with a pull request kelpie knows but the gate was not entered
    ///
    /// A work item saved before the gate existed is one such. `ticket` names
    /// the work item, and may be left out while only one is open.
    ///
    /// # Errors
    ///
    /// [`GateError`] naming why the work item cannot enter the gate. Nothing
    /// changes then.
    pub fn gate(&mut self, ticket: Option<TicketId>) -> Result<(), GateError> {
        self.choose(ticket).map_err(GateError::Which)?;
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
    // Works on the work item for `ticket`, or on the only one open when no
    // ticket is named. A work item is keyed by an issue number, so a ticket
    // from another source has none open.
    pub(super) fn choose(&mut self, ticket: Option<TicketId>) -> Result<(), WhichItem> {
        let open = self.state.open_tickets();
        let chosen = match (ticket, open.as_slice()) {
            (Some(ticket), _) if open.contains(&ticket) => ticket,
            (Some(ticket), _) => return Err(WhichItem::NotOpen(ticket)),
            (None, []) => return Err(WhichItem::NoWorkItem),
            (None, [only]) => *only,
            (None, _) => return Err(WhichItem::Several(open)),
        };
        self.focus = Some(chosen);
        Ok(())
    }
}

/// The runner's lock, taken and waited for, and timed from the line that calls this
#[track_caller]
pub(super) fn lock(runner: &Mutex<Runner>) -> Held<'_> {
    Held::new(runner.lock().unwrap_or_else(PoisonError::into_inner))
}

// The runner's lock if no pass holds it, timed from the line that calls this.
#[track_caller]
fn try_lock(runner: &Mutex<Runner>) -> Option<Held<'_>> {
    match runner.try_lock() {
        Ok(guard) => Some(Held::new(guard)),
        Err(TryLockError::Poisoned(e)) => Some(Held::new(e.into_inner())),
        Err(TryLockError::WouldBlock) => None,
    }
}

#[cfg(test)]
pub(in crate::runner) mod tests;
