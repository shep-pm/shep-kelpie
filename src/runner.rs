//! The project runner
//!
//! One runner per project, run as a sheep under kelpie's own shepherd. It
//! checks the project's settings when it starts, keeps the project's state
//! file, answers the maintainer's triggers, and runs the worker's turns.
//! Every change is saved before it takes effect in memory.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use crate::agents::{Agents, AgentsError};
use crate::board::{LabelError, Skip, agent_label, old_worker_label};
use crate::local_paths::LocalPaths;
use crate::pacer::Assessment;
use crate::ports::{ForgeError, Guarded, Leased, Ports, SessionId, Timestamp, Visibility};
use crate::review_bot::{Bot, Profile};
use crate::settings::{
    Account, AgentName, ForgeSlug, ListedReviewer, RoleAgents, Settings, SettingsError,
};
use crate::skills::Skills;
use crate::state::ids::RulingIds;
use crate::state::{ProjectState, StateError, StateStore};
use crate::webhook::{KelpieSettings, Webhook};
use crate::work_item::{
    Known, Phase, QwenTally, ReviewCallState, Seat, Timings, Turn, WorkItem, new_session_id,
};

mod adopt;
#[cfg(test)]
mod agents_tests;
mod alert;
mod attach;
mod briefing;
mod claim;
mod claude_files;
#[cfg(test)]
mod coderabbit;
mod dispatch;
mod drain;
mod flight;
mod follow_up;
mod gate;
mod gpu;
#[cfg(test)]
mod gpu_tests;
mod guard_hooks;
mod in_flight;
mod instructions;
mod kept;
#[cfg(test)]
mod kept_tests;
mod ledger;
#[cfg(test)]
mod ledger_tests;
mod left;
#[cfg(test)]
mod limits_tests;
mod merge;
mod older_bots;
#[cfg(test)]
mod overlap_tests;
mod pace;
mod parent;
mod paths;
mod pm;
mod question;
mod replies;
mod report;
mod reread;
mod review;
mod review_bot;
mod rework;
mod ruling;
#[cfg(test)]
mod several;
mod slots;
#[cfg(test)]
mod slots_tests;
mod timings;
mod trigger;
mod turn;
mod words;
mod worker_files;

pub use crate::coderabbit::LABEL as SUMMON_LABEL;
pub use adopt::AdoptError;
pub use attach::{AttachError, Attaching};
pub use claim::IN_PROGRESS;
pub use drain::{CallRole, CallRunning, Draining};
#[cfg(test)]
pub use flight::step;
pub use flight::{Pass, advance};
pub use gpu::GpuStatus;
pub use in_flight::Stopping;
pub use ledger::count_stopped;
pub use left::leave as leave_answer;
pub use merge::DropError;
pub use pace::PacerStatus;
pub use paths::{ProjectName, ProjectNameError, ProjectPaths};
pub use pm::{PmAttaching, PmError, PmStatus};
pub use replies::READ_EVERY;
pub use report::StepReport;
pub use rework::{HUMAN, ReworkError};
pub use ruling::{Answer, RuleError};
pub use timings::{Totals, settle};
use trigger::issue_list;
pub use trigger::{ACTIONS, Status, WorkItemStatus, answer};
pub use trigger::{GateError, WhichItem};
pub use words::{Wants, read_answer};

#[cfg(test)]
pub(crate) use gate::CHECKS_SETTLE;
#[cfg(test)]
pub(crate) use review_bot::SETTLE_LEAST;

/// Why a runner could not start
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// A setting is missing, malformed or does not hold
    Settings(SettingsError),
    /// An agent file cannot be read or used
    Agents(AgentsError),
    /// The forge could not be asked about the project's repo
    Forge(ForgeError),
    /// The state file could not be read
    State(StateError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Settings(e) => e.fmt(f),
            Self::Agents(e) => e.fmt(f),
            Self::Forge(e) => write!(f, "cannot check the repo on the forge: {e}"),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for OpenError {}

impl From<SettingsError> for OpenError {
    fn from(e: SettingsError) -> Self {
        Self::Settings(e)
    }
}

impl From<AgentsError> for OpenError {
    fn from(e: AgentsError) -> Self {
        Self::Agents(e)
    }
}

impl From<ForgeError> for OpenError {
    fn from(e: ForgeError) -> Self {
        Self::Forge(e)
    }
}

impl From<StateError> for OpenError {
    fn from(e: StateError) -> Self {
        Self::State(e)
    }
}

/// Why `add` was refused
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddError {
    /// No slot is free under `concurrency.active_items`, counting the items waiting for
    /// one, or one for this issue is open already: the issues of those in
    /// the way
    InFlight(Vec<u64>),
    /// The forge could not show the issue
    Forge(ForgeError),
    /// The issue's `agent:` label cannot be used
    Label(LabelError),
    /// No random session id could be drawn, with the OS's reason
    Session(String),
    /// The work item could not be saved
    State(StateError),
}

impl fmt::Display for AddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InFlight(issues) if issues.len() == 1 => {
                write!(f, "the work item for {} is in flight", issue_list(issues))
            }
            Self::InFlight(issues) => {
                write!(f, "the work items for {} are in flight", issue_list(issues))
            }
            Self::Forge(e) => write!(f, "cannot read the issue: {e}"),
            Self::Label(e) => e.fmt(f),
            Self::Session(e) => write!(f, "cannot draw a session id: {e}"),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for AddError {}

/// One project's runner
#[derive(Debug)]
pub struct Runner {
    project: ProjectName,
    settings: Settings,
    // The project's repo on the forge: `git.remote`, or the checkout's
    // `origin` when it is absent, read when the runner starts
    remote: ForgeSlug,
    // The project's extra worker instructions, read once when the runner starts
    extra_instructions: Option<String>,
    // Each step's skill, loaded when the runner starts or its settings change
    skills: Skills,
    paths: ProjectPaths,
    kelpie: PathBuf,
    store: StateStore,
    state: ProjectState,
    ports: Ports,
    // The pacer's last reading of each account's usage and when, kept in memory only
    pacing: BTreeMap<Account, (Timestamp, Assessment)>,
    // What the board passed over on its last poll, kept in memory only
    skipped: Vec<Skip>,
    // The forge's refusals in a row to close a done parent, kept in memory only
    close_refused: parent::Refused,
    // The review's reviewers, in order, from the project's list
    lineup: Vec<ListedReviewer>,
    // The project's implementers
    agents: RoleAgents,
    // Every agent kelpie's files define, from which each turn runs its work item's agent
    book: Agents,
    // The maintainer's home folder, for `~/` in kelpie's own settings
    home: PathBuf,
    // The GPU's last reading, from the page kelpie's settings name
    gpu: gpu::GpuWatch,
    // None when kelpie's settings set no webhook
    webhook: Option<Webhook>,
    // The last failed webhook post, kept in memory so a restart tries at once
    retry: Option<alert::Retry>,
    // Reading the webhook's topic for replies, kept in memory only
    reading: replies::Reading,
    // What a reply's code is checked against, on an ntfy webhook
    totp: Option<replies::Authenticator>,
    // The account kelpie acts as, read once a run when a rework first needs it
    viewer: Option<String>,
    // The issue of the work item a step or trigger is working on, set before
    // anything reads it: every change to a work item goes to this one
    focus: Option<u64>,
    // What last did something in a step, kept in memory only, so the next
    // step starts with the one after it
    last_acted: Option<turn::Slot>,
    // What the runner carried on without, kept in memory only until
    // `take_notes` hands it out
    notes: Vec<String>,
    // The calls this process has in flight, kept in memory only
    flights: flight::Flights,
    // Where each call is recorded as it ends
    ledger: crate::usage::Ledger,
    // What the board briefing keeps between writes, in memory only
    brief: briefing::BoardCache,
    // What the board withholds, as the forge does
    local: LocalPaths,
    // The project manager's wakes and decisions, in memory only
    pm: pm::Desk,
    // Whether `drain` holds back every new call, in memory only, so a
    // restart ends it
    draining: bool,
    // What the answers folder held that was no answer, in memory only
    left: left::Seen,
    // When each work item parked on a merge ruling last read the review bots
    // its pass went on without, kept in memory only
    late_reads: BTreeMap<u64, Timestamp>,
}

impl Runner {
    /// Checks the project's settings hold, and reads its state
    ///
    /// `settings` and `kelpie_settings` are as [`crate::settings::source::load`]
    /// read them. `kelpie` is the kelpie binary, which each worker's file-tool
    /// hook runs. No forge post may name `home`, kelpie's home or the checkout.
    ///
    /// # Errors
    ///
    /// [`OpenError`] naming the setting, forge call or file that failed.
    pub fn open(
        project: ProjectName,
        settings: Settings,
        kelpie_settings: KelpieSettings,
        paths: &ProjectPaths,
        home: &Path,
        kelpie: &Path,
        mut ports: Ports,
    ) -> Result<Self, OpenError> {
        let checkout = settings.git.checkout.as_path();
        let local = LocalPaths::new([home, paths.kelpie_home.as_path(), checkout]);
        ports.forge = Box::new(Guarded::new(ports.forge, local.clone()));
        let leases = Arc::clone(&ports.local_leases);
        ports.agents = Arc::new(Leased::new(Arc::clone(&ports.agents), leases));
        let gpu = gpu::GpuWatch::start(
            Arc::clone(&ports.gpu),
            kelpie_settings.gpu_metrics_url.clone(),
        );
        let store = StateStore::new(paths.state.clone());
        let book = Agents::load(&paths.agents)?;
        let (book, mut notes) = kept::keep_old_agents(&store, &paths.agents, book)?;
        notes.extend(book.skipped());
        notes.extend(worker_files::remove_old(&paths.worker));
        let agents = settings.role_agents(&book)?;
        let lineup = settings.lineup(&book, home)?;
        let listed = agents.implementers.iter().map(|i| &i.name);
        let reviewers = lineup.iter().map(|r| &r.name);
        (kelpie_settings.gateways()).check_listed(&book, listed.chain(reviewers))?;
        let webhook = kelpie_settings.webhook;
        if webhook.is_none() {
            eprintln!(
                "kelpie's settings set no webhook, so rulings reach you only in the log, \
                 `status` and `shep kelpie rule`"
            );
        }
        let totp = replies::authenticator(webhook.as_ref(), &paths.totp)?;
        let remote = check_checkout(&settings)?;
        let extra_instructions = instructions::read_extra(&settings)?;
        let env_home = std::env::var_os("HOME").map(PathBuf::from);
        guard_hooks::check(
            &settings,
            env_home.as_deref(),
            std::env::var_os("PATH").as_deref(),
        )?;
        check_bots(&lineup, &ports)?;
        crate::skills::check(&settings.skills, &paths.skills)?;
        let skills = Skills::load(&settings.skills, &paths.skills);
        check_coderabbit(&remote, &lineup, &ports)?;
        check_local(&lineup, &ports)?;
        let mut state = store.load()?.unwrap_or_default();
        // A review call in flight when the runner stopped never resumes on
        // its own, unlike a turn: nothing reruns review_step to naturally
        // clear it, so a restart clears it here instead of leaving it stuck
        // running forever and refusing every later drop.
        let cut_short =
            |item: &WorkItem| matches!(item.review_call, ReviewCallState::Running { .. });
        if state.work_items.iter().any(cut_short) {
            for item in state.work_items.iter_mut().filter(|item| cut_short(item)) {
                item.call_ended();
            }
            store.save(&state)?;
        }
        // A new run is a new epoch, and the dog reclaims what the old one held.
        if !state.leases.is_empty() {
            state.leases.clear();
            store.save(&state)?;
        }
        if timings::reload(&mut state, ports.clock.now()) {
            store.save(&state)?;
        }
        let mut runner = Self {
            project,
            settings,
            remote,
            extra_instructions,
            skills,
            paths: paths.clone(),
            kelpie: kelpie.to_owned(),
            store,
            state,
            ports,
            pacing: BTreeMap::new(),
            skipped: Vec::new(),
            close_refused: parent::Refused::new(),
            lineup,
            agents,
            book,
            home: home.to_owned(),
            gpu,
            webhook,
            retry: None,
            reading: replies::Reading::default(),
            totp,
            viewer: None,
            focus: None,
            last_acted: None,
            notes,
            flights: flight::Flights::default(),
            ledger: crate::usage::Ledger::in_folder(&paths.folder),
            brief: briefing::BoardCache::default(),
            local,
            pm: pm::Desk::default(),
            draining: false,
            left: left::Seen::default(),
            late_reads: BTreeMap::new(),
        };
        runner.settle_older_bots()?;
        runner.settle_labels();
        runner.brief_now();
        Ok(runner)
    }

    /// The project's settings, as read when the runner started
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The project's repo on the forge, as read when the runner started
    pub fn remote(&self) -> &ForgeSlug {
        &self.remote
    }

    // The profile of `bot`, which a start checks every listed bot has.
    fn profile(&self, bot: Bot) -> std::sync::Arc<dyn Profile> {
        let found = self.ports.review_bots.iter().find(|p| p.bot() == bot);
        std::sync::Arc::clone(found.expect("a listed review bot has a profile"))
    }

    // The review bots the project lists, in its order.
    fn listed_bots(&self) -> Vec<crate::review_bot::BotReviewer> {
        self.lineup.iter().filter_map(ListedReviewer::bot).collect()
    }

    /// A log line for each step whose skill could not load
    pub fn skill_notices(&self) -> impl Iterator<Item = String> + '_ {
        self.skills.notices()
    }

    fn names(&self) -> Names<'_> {
        Names {
            project: self.project.as_str(),
            ids: RulingIds::under(&self.paths.kelpie_home),
        }
    }

    /// The project's state as `status` reports it
    pub fn status(&self) -> Status<'_> {
        let now = self.ports.clock.now();
        Status {
            project: self.project.as_str(),
            merging: self.settings.git.merging,
            work_item: self
                .state
                .work_items
                .first()
                .map(|item| self.item_status(item, now)),
            work_items: self
                .state
                .work_items
                .iter()
                .map(|item| self.item_status(item, now))
                .collect(),
            active_items: self.settings.concurrency.active_items.get(),
            working: self.issues_where(|i| !i.parked() && i.seat != Seat::Waiting),
            waiting_for_slot: self.issues_where(|i| i.seat == Seat::Waiting),
            parked: self.parked_issues(),
            pending_rulings: self.settings.concurrency.pending_rulings,
            adopted: &self.state.adopted,
            skipped: &self.skipped,
            rulings: &self.state.rulings,
            leases: &self.state.leases,
            history: &self.state.history[self
                .state
                .history
                .len()
                .saturating_sub(trigger::STATUS_HISTORY)..],
            pacer: self.pacer_status(now),
            skills: self.skills.status(),
            local_model: self.ports.reviewer.seat().map(Into::into),
            gpu: self.gpu.status(),
            local_leases: self.local_leases(),
            board: &self.paths.board,
            pm: self.pm_status(),
            draining: self.draining_status(),
        }
    }

    fn item_status<'a>(&self, item: &'a WorkItem, now: Timestamp) -> WorkItemStatus<'a> {
        let split = item.split(now, self.timing_phase(item));
        WorkItemStatus::new(item, split)
    }

    /// Opens a work item for `issue`, and returns the implementer its
    /// worker runs on. Its first turn runs on a later pass.
    ///
    /// # Errors
    ///
    /// [`AddError`] when the project has `concurrency.active_items` working or one for this
    /// issue open already, the issue cannot be read or its `agent:` label names
    /// no listed implementer, or the change cannot be saved. Nothing changes then.
    pub fn add(&mut self, issue: u64) -> Result<AgentName, AddError> {
        if self.state.item(issue).is_some() {
            return Err(AddError::InFlight(vec![issue]));
        }
        if !self.slot_free() {
            return Err(AddError::InFlight(self.slot_issues()));
        }
        let found = self
            .ports
            .forge
            .issue(&self.remote, issue)
            .map_err(AddError::Forge)?;
        let (agent, note) = self
            .labelled_agent(issue, &found.labels)
            .map_err(AddError::Label)?;
        let session = new_session_id().map_err(|e| AddError::Session(e.to_string()))?;
        let mut next = self.state.clone();
        next.work_items
            .push(self.fresh(issue, found.title, agent.clone(), session));
        self.save(next).map_err(AddError::State)?;
        self.notes.extend(note);
        self.mark_held(issue, true);
        Ok(agent)
    }

    // The implementer an issue with `labels` runs on: the one its `agent:`
    // label names, or the default. Beside it, the log line an old `worker:`
    // label gets once the work item opens, since it no longer picks anything.
    fn labelled_agent(
        &self,
        issue: u64,
        labels: &[String],
    ) -> Result<(AgentName, Option<String>), LabelError> {
        let listed = self.agents.implementer_names();
        if let Some(agent) = agent_label(labels, &listed)? {
            return Ok((agent, None));
        }
        let agent = self.agents.default_implementer.name.clone();
        let note = old_worker_label(labels).map(|old| {
            format!(
                "issue #{issue} is labelled `{old}`, which kelpie no longer reads, so it runs \
                 on the default implementer, {agent}: an `agent:<name>` label picks another \
                 that `agents.implementers` lists"
            )
        });
        Ok((agent, note))
    }

    // A work item on `kelpie/<issue>` whose first turn is due, with nothing
    // recorded yet
    fn fresh(&self, issue: u64, title: String, agent: AgentName, session: SessionId) -> WorkItem {
        WorkItem {
            issue,
            title,
            branch: format!("kelpie/{issue}"),
            rework: false,
            adopted: false,
            arrived: None,
            worktree: self.paths.worktree(issue),
            build: self.paths.build(issue),
            agent,
            session,
            turn: Turn::Due,
            pull_request: None,
            phase: Phase::Implement,
            red_head: None,
            conflict: None,
            resume: None,
            review_call: ReviewCallState::default(),
            bot_reads: Default::default(),
            bots_skipped: Vec::new(),
            known: Known::default(),
            reviewed_heads: Vec::new(),
            sent_unread: Vec::new(),
            nit_fix_heads: Vec::new(),
            noted_from: None,
            late_from: None,
            claude_files_accepted: None,
            qwen: QwenTally::default(),
            merge_refused: false,
            sent_back: false,
            asked_to_commit: false,
            merge_tried: None,
            merge_queued: None,
            summon_owed: false,
            summons_owed: Default::default(),
            bots_after_ci: false,
            seat: Seat::Held,
            threads_sent: Vec::new(),
            resolve_failures: 0,
            reviewers_skipped: Vec::new(),
            unreviewed: None,
            local_failures: Default::default(),
            local_unreviewed: Vec::new(),
            local_unreviewed_by: None,
            rebased: false,
            held: Vec::new(),
            follow_ups: None,
            summary: None,
            timings: Some(Timings::starting(self.ports.clock.now())),
            attached: None,
            counts: Default::default(),
            calls: Vec::new(),
        }
    }

    // The work item the runner is working on, while it is open
    pub(super) fn current(&self) -> Option<&WorkItem> {
        self.state.item(self.focus?)
    }

    // The work item the runner is working on, in a state about to be saved
    pub(super) fn current_in<'a>(&self, next: &'a mut ProjectState) -> Option<&'a mut WorkItem> {
        next.item_mut(self.focus?)
    }

    // Works on the work item for `issue` from here on
    pub(super) fn on(&mut self, issue: Option<u64>) -> &mut Self {
        self.focus = issue;
        self
    }

    fn save(&mut self, mut next: ProjectState) -> Result<(), StateError> {
        slots::seat(&self.state, &mut next, self.active_items());
        self.charge(&mut next);
        self.note_changes(&mut next);
        self.store.save(&next)?;
        self.state = next;
        Ok(())
    }
}

/// How [`check_checkout`]'s refusal of a checkout with no `origin` ends
pub(crate) const NO_ORIGIN: &str = "has no `origin` remote to cut branches from";

/// The project's repo on the forge, once `git.checkout` is found to be a
/// git checkout with an `origin`, which the runner needs to open
///
/// The repo is `git.remote`, or the GitHub repo `origin` names when that
/// is absent.
///
/// # Errors
///
/// [`SettingsError::Invalid`] naming `git.checkout` and what is wrong with
/// it, or `git.remote` when it is absent and `origin` is not a GitHub repo.
pub(crate) fn check_checkout(settings: &Settings) -> Result<ForgeSlug, SettingsError> {
    let repo = &settings.git.checkout;
    let invalid = |reason: String| SettingsError::Invalid {
        setting: "git.checkout",
        reason,
    };
    if !repo.is_dir() {
        return Err(invalid(format!("{} is not a folder", repo.display())));
    }
    let git = |args: &[&str]| {
        crate::worktree::in_repo(repo)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| invalid(format!("cannot run git to check it: {e}")))
    };
    let output = git(&["rev-parse", "--is-inside-work-tree"])?;
    if !output.status.success() || output.stdout.trim_ascii() != b"true" {
        return Err(invalid(format!(
            "{} is not a git work tree",
            repo.display()
        )));
    }
    // Every work item's branch is cut from `origin/main`.
    let origin = git(&["remote", "get-url", "origin"])?;
    if !origin.status.success() {
        return Err(invalid(format!("{} {NO_ORIGIN}", repo.display())));
    }
    if let Some(remote) = &settings.git.remote {
        return Ok(remote.clone());
    }
    let url = String::from_utf8_lossy(&origin.stdout).trim().to_owned();
    crate::flock::forge_of(&url).ok_or_else(|| SettingsError::Invalid {
        setting: "git.remote",
        reason: "it is not set, and the checkout's `origin` is not a GitHub repo over HTTPS \
                 or SSH: set it to the repo as `owner/name`"
            .to_owned(),
    })
}

/// Whether `worker.instructions_file`, when set, can be read, which the runner needs to open
///
/// # Errors
///
/// [`SettingsError::Invalid`] naming `worker.instructions_file`.
pub(crate) fn check_instructions(settings: &Settings) -> Result<(), SettingsError> {
    instructions::read_extra(settings).map(drop)
}

// Each local reviewer's command is there, or its endpoint answers.
fn check_local(lineup: &[ListedReviewer], ports: &Ports) -> Result<(), SettingsError> {
    for reviewer in lineup {
        let Some(local) = reviewer.runs.local() else {
            continue;
        };
        ports
            .reviewer
            .check(&local)
            .map_err(|reason| SettingsError::Invalid {
                setting: "agents.reviewers",
                reason: format!("{}: {reason}", reviewer.name),
            })?;
    }
    Ok(())
}

// What a ruling's question names: its project, and where its id comes from.
#[derive(Debug, Clone)]
struct Names<'a> {
    project: &'a str,
    ids: RulingIds,
}

// Every listed review bot needs a profile.
fn check_bots(lineup: &[ListedReviewer], ports: &Ports) -> Result<(), SettingsError> {
    for reviewer in lineup {
        let Some(bot) = reviewer.bot() else {
            continue;
        };
        if !ports.review_bots.iter().any(|p| p.bot() == bot.bot) {
            return Err(SettingsError::Invalid {
                setting: "agents.reviewers",
                reason: format!("{}: kelpie has no profile for {}", reviewer.name, bot.bot),
            });
        }
    }
    Ok(())
}

// CodeRabbit's free plan reviews public repos only, so listing it for a
// repo the forge reports otherwise stops the runner.
fn check_coderabbit(
    remote: &ForgeSlug,
    lineup: &[ListedReviewer],
    ports: &Ports,
) -> Result<(), OpenError> {
    let listed = lineup
        .iter()
        .find(|r| r.bot().is_some_and(|b| b.bot == Bot::Coderabbit));
    let Some(listed) = listed else {
        return Ok(());
    };
    let visibility = match ports.forge.visibility(remote)? {
        Visibility::Public => return Ok(()),
        Visibility::Private => "private",
        Visibility::Internal => "internal",
    };
    Err(SettingsError::Invalid {
        setting: "agents.reviewers",
        reason: format!(
            "{}: {} is {visibility}, and {}'s free plan reviews public repos only",
            listed.name,
            remote.as_str(),
            Bot::Coderabbit.name()
        ),
    }
    .into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::runner::step;
    use crate::test::{Rig, git};

    // A project saved paused by an older kelpie runs once its runner starts.
    #[test]
    fn a_project_saved_paused_reads_the_board_on_its_first_pass() {
        let rig = Rig::new("reactmap");
        let epoch = Rig::EPOCH;
        let old = json!({
            "version": 9,
            "run": "paused",
            "since": epoch,
            "work_items": [],
            "rulings": [{
                "id": 3,
                "issue": null,
                "question": "q",
                "pull_request": 30,
                "kind": { "kind": "stuck", "reason": "closed" },
                "alerted": true,
            }],
            "last_ruling": 3,
            "finished": [5],
            "history": [{
                "issue": 5,
                "title": "Five",
                "pull_request": 50,
                "merged": true,
                "at": epoch,
                "wall": 100,
                "seconds": { "worker": 60, "review": 0, "ci": 40, "ruling": 0, "merge": 0, "other": 0 },
            }],
            "reworked": [],
            "adopted": [],
            "leases": [],
            "pacing": null,
            "notices": [],
            "replies": { "last": null },
            "events": [
                { "id": 1, "at": epoch, "what": "project started" },
                { "id": 2, "at": epoch, "what": "#5: PR #50 merged, work item done" },
                { "id": 3, "at": epoch, "what": "project paused" },
            ],
            "last_event": 3,
            "pm_seen": 2,
            "pm_session": "0e2c6a52-5b0e-4c5f-9a43-3c1f1d8b7e10",
        });
        let state = rig.paths().state;
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::write(&state, old.to_string()).unwrap();

        let runner = rig.open().unwrap();
        rig.forge.list_ready(7, false);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Dispatched { issue: 7, .. })
        ));
        let status = rig.ask(&runner, "status", None);
        assert_eq!(status["work_item"]["issue"], 7);
        assert_eq!((status.get("run"), status.get("since")), (None, None));
        assert_eq!(status["rulings"][0]["id"], 3);
        assert_eq!(status["history"][0]["issue"], 5);
        drop(runner);
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        assert_eq!(saved["version"], 16);
        assert_eq!(
            saved["events"][2]["what"], "project paused",
            "old events stay"
        );
        assert_eq!(saved["pm_seen"], 2);
        assert_eq!(saved["pm_session"], "0e2c6a52-5b0e-4c5f-9a43-3c1f1d8b7e10");
    }

    #[test]
    fn a_failed_save_is_reported_and_changes_nothing() {
        let rig = Rig::new("xilriws");
        let runner = rig.open().unwrap();
        let folder = rig.paths().state.parent().unwrap().to_owned();
        std::fs::remove_dir_all(&folder).unwrap();
        // A file where the folder was, which a save cannot make a folder of.
        std::fs::write(&folder, "").unwrap();
        let reply = rig.ask(&runner, "add", Some("7"));
        assert!(
            reply["error"]
                .as_str()
                .unwrap()
                .contains("cannot write state file"),
            "{reply}"
        );
        assert_eq!(rig.ask(&runner, "status", None)["work_item"], json!(null));
    }

    #[test]
    fn a_repo_that_is_not_a_git_work_tree_stops_the_runner() {
        let rig = Rig::new("chelone");
        let elsewhere = rig.home.path().join("not-a-repo");
        std::fs::create_dir(&elsewhere).unwrap();
        rig.edit_settings(|s| {
            s.replace(
                &rig.repo().display().to_string(),
                &elsewhere.display().to_string(),
            )
        });
        let err = rig.open().unwrap_err();
        assert!(
            err.to_string().starts_with("setting `git.checkout`: "),
            "{err}"
        );
        assert!(
            err.to_string()
                .ends_with("not-a-repo is not a git work tree"),
            "{err}"
        );
    }

    #[test]
    fn a_repo_without_an_origin_stops_the_runner() {
        let rig = Rig::new("koji");
        git(&rig.repo(), &["remote", "remove", "origin"]);
        let err = rig.open().unwrap_err().to_string();
        assert!(err.starts_with("setting `git.checkout`: "), "{err}");
        assert!(
            err.ends_with("koji has no `origin` remote to cut branches from"),
            "{err}"
        );
    }

    #[test]
    fn with_no_remote_set_the_repo_is_read_from_origin() {
        for url in [
            "git@github.com:shep-pm/from-origin.git",
            "https://github.com/shep-pm/from-origin",
        ] {
            let rig = Rig::new("koji");
            rig.edit_settings(|s| s.replace("remote = \"shep-pm/shep\"\n", ""));
            git(&rig.repo(), &["remote", "set-url", "origin", url]);
            let runner = rig.open().unwrap();
            let runner = runner.lock().unwrap();
            assert_eq!(runner.remote().as_str(), "shep-pm/from-origin", "{url}");
            assert_eq!(runner.settings().git.remote, None);
        }
    }

    #[test]
    fn with_no_remote_set_an_origin_off_github_stops_the_runner_naming_the_setting() {
        let rig = Rig::new("koji");
        rig.edit_settings(|s| s.replace("remote = \"shep-pm/shep\"\n", ""));
        let err = rig.open().unwrap_err().to_string();
        assert_eq!(
            err,
            "setting `git.remote`: it is not set, and the checkout's `origin` is not a GitHub \
             repo over HTTPS or SSH: set it to the repo as `owner/name`"
        );
    }

    #[test]
    fn a_repo_that_does_not_exist_stops_the_runner() {
        let rig = Rig::new("reactmap");
        let gone = rig.repo().display().to_string();
        rig.edit_settings(|s| s.replace(&gone, &format!("{gone}-gone")));
        let err = rig.open().unwrap_err().to_string();
        assert!(err.starts_with("setting `git.checkout`: "), "{err}");
        assert!(err.ends_with("reactmap-gone is not a folder"), "{err}");
    }

    #[test]
    fn coderabbit_listed_for_a_repo_that_is_not_public_stops_the_runner() {
        for (visibility, seen_by) in [
            (Visibility::Private, "private"),
            (Visibility::Internal, "internal"),
        ] {
            let rig = Rig::new("shep");
            rig.coderabbit_on();
            rig.forge.set_visibility(visibility);
            assert_eq!(
                rig.open().unwrap_err().to_string(),
                format!(
                    "setting `agents.reviewers`: coderabbit: shep-pm/shep is {seen_by}, \
                     and CodeRabbit's free plan reviews public repos only"
                )
            );
        }
    }

    #[test]
    fn coderabbit_listed_for_a_public_repo_asks_the_forge_once() {
        let rig = Rig::new("shep");
        rig.coderabbit_on();
        rig.open().unwrap();
        assert_eq!(rig.forge.calls(), 1);
    }

    #[test]
    fn a_project_listing_no_bot_never_asks_the_forge() {
        let rig = Rig::new("acme");
        rig.forge.set_visibility(Visibility::Private);
        rig.open().unwrap();
        assert_eq!(rig.forge.calls(), 0);
    }

    #[test]
    fn a_repo_github_marks_private_may_list_cubic() {
        let rig = Rig::new("acme");
        rig.reviewers(&["qwen", "claude", "cubic"]);
        rig.forge.set_visibility(Visibility::Private);
        rig.open().unwrap();
        assert_eq!(rig.forge.calls(), 0);
    }
}
