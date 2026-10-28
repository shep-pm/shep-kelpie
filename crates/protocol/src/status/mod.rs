//! What a runner's `status` answers, as owned data

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::de::{self, Deserializer};
use serde::ser::{self, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use shep_kelpie_core::board::Skip;
use shep_kelpie_core::board::briefing::Run;
use shep_kelpie_core::ports::{PaddockHolder, SessionId, Timestamp};
use shep_kelpie_core::review_bot::Bot;
use shep_kelpie_core::settings::{AgentName, Merging};
use shep_kelpie_core::skills::{RepoPlugin, StepSkill};
use shep_kelpie_core::state::{self, Ruling};
use shep_kelpie_core::ticket::TicketId;
use shep_kelpie_core::work_item::{
    Attached, BotSkipped, Phase, QwenTally, Spend, Split, TestCheck, Turn,
};

pub mod standing;
pub mod work;

pub use standing::{
    AccountStatus, CallRole, CallRunning, Draining, GpuStatus, PacerStatus, RedStatus, Refusal,
    TemplateSource,
};
pub use work::{
    BumpStatus, ClockStatus, ContributionDue, DailyStatus, DueStatus, Failing, Followed, Held,
    HeldUp, PmStatus, Settled, TriageStatus, Waiting,
};

/// What `status` answers
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// The project
    pub project: String,
    /// Who merges its green, reviewed pull requests
    pub merging: Merging,
    /// The first of `work_items`, where a status read before they existed
    /// finds the work item
    pub work_item: Option<WorkItemStatus>,
    /// Every open work item, oldest first
    pub work_items: Vec<WorkItemStatus>,
    /// How many work items may hold a slot at once
    pub active_items: u32,
    /// The tickets of the open work items going on, oldest first
    pub working: Vec<TicketId>,
    /// The tickets of the work items that gave their slot up to a ruling and
    /// need one again for a model call, oldest first
    pub waiting_for_slot: Vec<TicketId>,
    /// The tickets of the work items parked on rulings, oldest first
    pub parked: Vec<TicketId>,
    /// How many work items may wait parked on rulings before the board opens
    /// nothing new
    pub pending_rulings: u32,
    /// Pull requests adopted and waiting for a free slot, oldest first
    pub adopted: Vec<state::Waiting>,
    /// The pull requests from other people the board follows, oldest first;
    /// left out when it follows none
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributions: Vec<Followed>,
    /// The bumps from bots the board follows, oldest first; left out when it
    /// follows none
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bumps: Vec<BumpStatus>,
    /// The issues from other people that triage would pick up, oldest first;
    /// left out while none is
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<TriageStatus>,
    /// The first issues in the order the board takes the ready ones, which
    /// holds those it would pass over too
    pub order: Vec<TicketId>,
    /// Ready issues the board passed over on its last poll, and why
    pub skipped: Vec<Skip>,
    /// The warnings that stand now, each told once in the log; left out while
    /// none does
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub standing: Vec<String>,
    /// The workflows whose latest completed run on `main` failed, in the order
    /// they went red; left out while none is
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub red_workflows: Vec<RedStatus>,
    /// Rulings waiting on the maintainer, oldest first
    pub rulings: Vec<RulingStatus>,
    /// How many rulings the project manager answered, by kind; left out while
    /// it answered none
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pm_answers: BTreeMap<String, u32>,
    /// Leases held
    pub leases: Vec<state::LeaseHeld>,
    /// The most recent finished work items, oldest first
    pub history: Vec<state::Finished>,
    /// What usage was when last read, and why nothing new is starting
    pub pacer: PacerStatus,
    /// The skill each step runs, and why any chosen one could not load
    pub skills: Vec<StepSkill>,
    /// The plugins the repo turns on, with their skills or why each was
    /// skipped; left out when it turns on none
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<RepoPlugin>,
    /// Where a worker's pull request template comes from
    pub pull_request_template: TemplateSource,
    /// The GPU's load as the metrics page last said, when kelpie's settings
    /// name one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuStatus>,
    /// The free space on the disk holding kelpie's home when last measured, in
    /// bytes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_free_bytes: Option<u64>,
    /// Who holds the gateway's lease on each model a local agent calls, or
    /// null while it is free
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub local_leases: BTreeMap<String, Option<PaddockHolder>>,
    /// The board briefing the project manager's agent reads
    pub board: PathBuf,
    /// The project manager, when the project names one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pm: Option<PmStatus>,
    /// The size and modification time of the file this runner started from;
    /// left out when the file cannot be read
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// The commit the build this runner runs was made from; left out where git
    /// could not answer
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The calls still running, while `drain` holds back new ones
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draining: Option<Draining>,
    /// Whether `finish` holds the board back, or the runner finished and waits
    /// for its sheep to stop
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<Run>,
    /// How many seconds the runner goes between two reads of its board now
    pub board_poll_secs: u64,
    /// Until when the runner makes no forge call, in seconds since the Unix
    /// epoch; left out when it makes them
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge_held_until: Option<Timestamp>,
    /// Why the stored settings are refused and since when, while they are;
    /// left out otherwise
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_refused: Option<Refusal>,
    /// The longest hold of the runner's lock in seconds since it started, by
    /// the source line that took it; left out until a hold is recorded
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lock: BTreeMap<String, f64>,
}

/// An open work item, as `status` shows it
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkItemStatus {
    /// The ticket it resolves
    pub issue: TicketId,
    /// The issue's title
    pub title: String,
    /// Its branch
    pub branch: String,
    /// Whether it adopts a pull request kelpie didn't open
    pub adopted: bool,
    /// Its worktree
    pub worktree: PathBuf,
    /// The implementer its worker runs on
    pub agent: AgentName,
    /// Whether its `agent:` label or the project manager's pick pins it to that implementer
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Its worker's turn in flight, while that waits for its model
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<Waiting>,
    /// What its due worker turn waits on, while its last step left that turn
    /// unstarted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_up: Option<HeldUp>,
    /// Its step that failed again and again, from the second failure in a row
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing: Option<Failing>,
    /// `"none"` while its wait on CI has had no check reported, beside the
    /// wait's `since` in `phase`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<String>,
    /// What its build folder held when last measured, in bytes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_bytes: Option<u64>,
    /// The worker's session, which the maintainer can resume by hand
    pub session: SessionId,
    /// Where the worker's turn stands
    pub turn: Turn,
    /// Where it stands between the worker's turns and the merge
    pub phase: Phase,
    /// The worker's draft pull request, once kelpie has seen it
    pub pull_request: Option<u64>,
    /// The head whose failed CI jobs kelpie reran, while the rerun runs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerunning: Option<String>,
    /// The reads each review bot has made of its pull request
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bot_reads: BTreeMap<Bot, u32>,
    /// The listed review bots its pass went on without, and why
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bots_skipped: Vec<BotSkipped>,
    /// Claude calls made for it so far
    pub calls: usize,
    /// What the calls whose harness reports dollars have cost, in US dollars
    pub cost_usd: f64,
    /// Calls whose harness reported no dollars, which `cost_usd` leaves out
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unpriced_calls: usize,
    /// Calls and cost by role
    pub by_role: Spend,
    /// What its priced calls have cost against its cap, in US dollars
    pub budget: BudgetStatus,
    /// Its qwen rounds, which cost no money
    pub qwen: QwenTally,
    /// The maintainer's `attach` holding it, while one does
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached: Option<Attached>,
    /// The local reviewers that reviewed nothing twice, so the review goes on
    /// without them
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_reviewers_down: Vec<AgentName>,
    /// The reviewers whose calls failed so often in a row that a pass went on
    /// without them
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewers_skipped: Vec<AgentName>,
    /// Why its last review pass ended with no reviewer having read it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreviewed: Option<String>,
    /// What kelpie ran of the tests reviewers attached to findings, per finding
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_checks: Vec<TestCheck>,
    /// Where its wall time went, and the phase it is in now
    pub timings: Split,
}

/// A work item's priced spend against its cap, as `status` shows it
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BudgetStatus {
    /// What its worker's and reviewers' priced calls have cost
    pub spent_usd: f64,
    /// What they may cost before its worker's next turn waits on a ruling
    pub cap_usd: f64,
}

/// A waiting ruling, as `status` shows it: the ruling as saved, with its `id`
/// holding the number the maintainer answers it by
#[derive(Debug, Clone, PartialEq)]
pub struct RulingStatus {
    /// The ruling as the state saved it
    pub ruling: Ruling,
    /// The number the maintainer answers it by
    pub id: TicketId,
}

impl Serialize for RulingStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut shown = serde_json::to_value(&self.ruling).map_err(ser::Error::custom)?;
        shown["id"] = serde_json::to_value(self.id).map_err(ser::Error::custom)?;
        shown.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RulingStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let shown = Value::deserialize(deserializer)?;
        let id = shown
            .get("id")
            .ok_or_else(|| de::Error::custom("a ruling names the id it is answered by"))?;
        Ok(Self {
            id: TicketId::deserialize(id).map_err(de::Error::custom)?,
            ruling: Ruling::deserialize(&shown).map_err(de::Error::custom)?,
        })
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[cfg(test)]
pub(crate) mod tests;
