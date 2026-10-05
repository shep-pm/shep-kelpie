//! The project runner
//!
//! One runner per project, run as a sheep under kelpie's own shepherd. It
//! checks the project's settings when it starts, keeps the project's state
//! file, answers the maintainer's triggers, and runs the worker's turns.
//! Every change is saved before it takes effect in memory.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use crate::agents::{Agents, AgentsError};
use crate::board::{LabelError, Skip, agent_label, old_worker_label};
use crate::local_paths::LocalPaths;
use crate::pacer::Assessment;
use crate::ports::{ForgeError, Guarded, Leased, Ports, SessionId, Timestamp, Visibility};
use crate::review_bot::{Bot, Profile, Reviewers};
use crate::settings::{
    Account, AgentName, LoopReviewer, NonBlank, RoleAgents, Runs, Settings, SettingsError,
};
use crate::skills::Skills;
use crate::state::ids::RulingIds;
use crate::state::{ProjectState, RunState, StateError, StateStore};
use crate::webhook::{KelpieSettings, Webhook};
use crate::work_item::{
    CodeRabbitTally, Known, Phase, QwenTally, ReviewCallState, Timings, Turn, WorkItem,
    new_session_id,
};

mod adopt;
#[cfg(test)]
mod agents_tests;
mod alert;
mod claim;
mod claude_files;
#[cfg(test)]
mod coderabbit;
mod dispatch;
mod follow_up;
mod gate;
mod gpu;
#[cfg(test)]
mod gpu_tests;
mod guard_hooks;
mod instructions;
mod kept;
#[cfg(test)]
mod kept_tests;
#[cfg(test)]
mod limits_tests;
mod merge;
mod pace;
mod parent;
mod paths;
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
mod timings;
mod trigger;
mod turn;
mod words;

pub use crate::coderabbit::LABEL as SUMMON_LABEL;
pub use adopt::AdoptError;
pub use claim::IN_PROGRESS;
pub use gpu::GpuStatus;
pub use merge::DropError;
pub use pace::PacerStatus;
pub use paths::{ProjectName, ProjectNameError, ProjectPaths};
pub use replies::READ_EVERY;
pub use report::StepReport;
pub use rework::{HUMAN, ReworkError};
pub use ruling::{Answer, RuleError};
pub use timings::{Totals, settle};
use trigger::issue_list;
pub use trigger::{ACTIONS, Status, WorkItemStatus, answer};
pub use trigger::{GateError, WhichItem};
pub use turn::step;
pub use words::{Wants, read_answer};

#[cfg(test)]
pub(crate) use gate::CHECKS_SETTLE;

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
    /// The project has `max_items` open, or one for this issue already: the
    /// issues of those in flight
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
    // The pull request reviewers kelpie's own settings define
    reviewers: Reviewers,
    // The review's reviewers, in order, from the project's list
    lineup: Vec<LoopReviewer>,
    // The project's implementers, and the model and effort each review role runs on
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
    // The turns this process runs, kept in memory only
    live_turns: timings::LiveTurns,
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
        let folders = [home, paths.kelpie_home.as_path(), settings.repo.as_path()];
        let names = settings.private_names.iter().map(NonBlank::as_str);
        let local = LocalPaths::new(folders, names);
        ports.forge = Box::new(Guarded::new(ports.forge, local));
        let leases = Arc::clone(&ports.local_leases);
        ports.agents = Arc::new(Leased::new(Arc::clone(&ports.agents), leases));
        let reviewers = kelpie_settings.reviewers;
        let gpu = gpu::GpuWatch::start(
            Arc::clone(&ports.gpu),
            kelpie_settings.gpu_metrics_url.clone(),
        );
        let store = StateStore::new(paths.state.clone());
        let book = Agents::load(&paths.agents)?;
        let (book, mut notes) = kept::keep_old_agents(&store, &paths.agents, book)?;
        notes.extend(book.skipped());
        let agents = settings.role_agents(&book)?;
        let lineup = settings.lineup(&kelpie_settings, &book, home)?;
        let webhook = kelpie_settings.webhook;
        if webhook.is_none() {
            eprintln!(
                "kelpie's settings set no webhook, so rulings reach you only in the log, \
                 `status` and `shep kelpie rule`"
            );
        }
        let totp = replies::authenticator(webhook.as_ref(), &paths.totp)?;
        check_repo(&settings)?;
        let [worktrees, _] = paths.owned();
        if let Err(e) = crate::worktree::repair(&settings.repo, &worktrees) {
            eprintln!(
                "cannot repair git's links to the worktrees in {}: {e}",
                worktrees.display()
            );
        }
        let extra_instructions = instructions::read_extra(&settings)?;
        let env_home = std::env::var_os("HOME").map(PathBuf::from);
        guard_hooks::check(
            &settings,
            env_home.as_deref(),
            std::env::var_os("PATH").as_deref(),
        )?;
        check_reviewers(&settings, &reviewers, &ports)?;
        crate::skills::check(&settings.skills, &paths.skills)?;
        let skills = Skills::load(&settings.skills, &paths.skills);
        check_coderabbit(&settings, &ports)?;
        check_local(&settings, &lineup, &ports)?;
        let mut state = store
            .load()?
            .unwrap_or_else(|| ProjectState::new(ports.clock.now()));
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
            reviewers,
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
            live_turns: timings::LiveTurns::default(),
        };
        runner.settle_labels();
        Ok(runner)
    }

    /// The project's settings, as read when the runner started
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    // The profile of `bot`, which a start checks every listed bot has.
    fn profile(&self, bot: Bot) -> std::sync::Arc<dyn Profile> {
        let found = self.ports.review_bots.iter().find(|p| p.bot() == bot);
        std::sync::Arc::clone(found.expect("a listed review bot has a profile"))
    }

    /// A log line for each step whose skill could not load
    pub fn skill_notices(&self) -> impl Iterator<Item = String> + '_ {
        self.skills.notices()
    }

    fn names(&self) -> Names<'_> {
        let listed = self.settings.reviewers().into_iter();
        let names: Vec<String> = listed
            .map(|bot| self.profile(bot).name().to_owned())
            .collect();
        Names {
            project: self.project.as_str(),
            bot: names.join("/"),
            ids: RulingIds::under(&self.paths.kelpie_home),
        }
    }

    /// The project's state as `status` reports it
    pub fn status(&self) -> Status<'_> {
        let now = self.ports.clock.now();
        Status {
            project: self.project.as_str(),
            merge_authority: self.settings.merge_authority,
            run: self.state.run,
            since: self.state.since,
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
            max_items: self.settings.max_items.get(),
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
        }
    }

    fn item_status<'a>(&self, item: &'a WorkItem, now: Timestamp) -> WorkItemStatus<'a> {
        let split = item.split(now, self.timing_phase(item));
        WorkItemStatus::new(item, split)
    }

    /// Lets the project take work. Starting a running project changes nothing.
    ///
    /// # Errors
    ///
    /// [`StateError::Write`] when the change cannot be saved. Nothing changes then.
    pub fn start(&mut self) -> Result<(), StateError> {
        self.set_run(RunState::Running)
    }

    /// Stops the project taking work. Pausing a paused project changes nothing.
    ///
    /// # Errors
    ///
    /// [`StateError::Write`] when the change cannot be saved. Nothing changes then.
    pub fn pause(&mut self) -> Result<(), StateError> {
        self.set_run(RunState::Paused)
    }

    /// Opens a work item for `issue`, and returns the implementer its
    /// worker runs on. Its first turn runs once the project is running.
    ///
    /// # Errors
    ///
    /// [`AddError`] when the project has `max_items` open or one for this
    /// issue already, the issue cannot be read or its `agent:` label names
    /// no listed implementer, or the change cannot be saved. Nothing changes then.
    pub fn add(&mut self, issue: u64) -> Result<AgentName, AddError> {
        if self.state.item(issue).is_some() {
            return Err(AddError::InFlight(vec![issue]));
        }
        if !self.slot_free() {
            return Err(AddError::InFlight(self.state.open_issues()));
        }
        let found = self
            .ports
            .forge
            .issue(&self.settings.forge, issue)
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
            coderabbit: CodeRabbitTally::default(),
            known: Known::default(),
            claude_files_accepted: None,
            qwen: QwenTally::default(),
            merge_refused: false,
            sent_back: false,
            asked_to_commit: false,
            merge_tried: None,
            merge_queued: None,
            summon_owed: false,
            threads_sent: Vec::new(),
            resolve_failures: 0,
            reviewers_skipped: Vec::new(),
            local_failures: Default::default(),
            local_unreviewed: Vec::new(),
            local_unreviewed_by: None,
            rebased: false,
            held: Vec::new(),
            follow_ups: None,
            timings: Some(Timings::starting(self.ports.clock.now())),
            calls: Vec::new(),
        }
    }

    // Whether another work item may open, under `max_items`
    pub(super) fn slot_free(&self) -> bool {
        let max = usize::try_from(self.settings.max_items.get()).unwrap_or(usize::MAX);
        self.state.work_items.len() < max
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

    fn set_run(&mut self, run: RunState) -> Result<(), StateError> {
        if self.state.run == run {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.run = run;
        next.since = self.ports.clock.now();
        self.save(next)
    }

    fn save(&mut self, mut next: ProjectState) -> Result<(), StateError> {
        self.charge(&mut next);
        self.store.save(&next)?;
        self.state = next;
        Ok(())
    }
}

/// Whether `settings.repo` is a git checkout with an `origin`, which the runner needs to open
///
/// # Errors
///
/// [`SettingsError::Invalid`] naming `repo` and what is wrong with it.
pub(crate) fn check_repo(settings: &Settings) -> Result<(), SettingsError> {
    let repo = &settings.repo;
    let invalid = |reason: String| SettingsError::Invalid {
        setting: "repo",
        reason,
    };
    if !repo.is_dir() {
        return Err(invalid(format!("{} is not a folder", repo.display())));
    }
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo)
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
    if !git(&["remote", "get-url", "origin"])?.status.success() {
        return Err(invalid(format!(
            "{} has no `origin` remote to cut branches from",
            repo.display()
        )));
    }
    Ok(())
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
fn check_local(
    settings: &Settings,
    lineup: &[LoopReviewer],
    ports: &Ports,
) -> Result<(), SettingsError> {
    let setting = match settings.review.reviewers.is_empty() {
        true => "review.local",
        false => "review.reviewers",
    };
    for reviewer in lineup {
        let Runs::Local(local) = &reviewer.runs else {
            continue;
        };
        ports
            .reviewer
            .check(local)
            .map_err(|reason| SettingsError::Invalid { setting, reason })?;
    }
    Ok(())
}

// What a ruling's question names: its project, and the review bots it may
// be about, as one name, and where its id comes from.
#[derive(Debug, Clone)]
struct Names<'a> {
    project: &'a str,
    bot: String,
    ids: RulingIds,
}

// Every listed reviewer needs a definition in kelpie's settings and a profile.
fn check_reviewers(
    settings: &Settings,
    reviewers: &Reviewers,
    ports: &Ports,
) -> Result<(), SettingsError> {
    let invalid = |reason: String| SettingsError::Invalid {
        setting: "pull_request_reviewers",
        reason,
    };
    for bot in settings.reviewers() {
        if reviewers.window(bot).is_none() {
            return Err(invalid(format!(
                "{bot} is not defined: kelpie's own settings need a [reviewers.{bot}] table"
            )));
        }
        if !ports.review_bots.iter().any(|p| p.bot() == bot) {
            return Err(invalid(format!("kelpie has no profile for {bot}")));
        }
    }
    Ok(())
}

fn check_coderabbit(settings: &Settings, ports: &Ports) -> Result<(), OpenError> {
    let listed = settings.reviewers().contains(&Bot::Coderabbit);
    if !settings.coderabbit.enabled || !listed {
        return Ok(());
    }
    let visibility = match ports.forge.visibility(&settings.forge)? {
        Visibility::Public => return Ok(()),
        Visibility::Private => "private",
        Visibility::Internal => "internal",
    };
    Err(SettingsError::Invalid {
        setting: "coderabbit.enabled",
        reason: format!(
            "{} is {visibility}, and {}'s free plan reviews public repos only",
            settings.forge.as_str(),
            Bot::Coderabbit.name()
        ),
    }
    .into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test::{Rig, git};

    #[test]
    fn start_and_pause_survive_a_restart() {
        let rig = Rig::new("reactmap");
        let runner = rig.open().unwrap();
        rig.clock.advance(60);
        assert_eq!(rig.ask(&runner, "start", None)["run"], "running");
        drop(runner);

        let runner = rig.open().unwrap();
        let status = rig.ask(&runner, "status", None);
        assert_eq!(
            (&status["run"], &status["since"]),
            (&json!("running"), &json!(Rig::EPOCH + 60))
        );
        rig.clock.advance(60);
        assert_eq!(rig.ask(&runner, "pause", None)["run"], "paused");
        drop(runner);

        let runner = rig.open().unwrap();
        let status = rig.ask(&runner, "status", None);
        assert_eq!(
            (&status["run"], &status["since"]),
            (&json!("paused"), &json!(Rig::EPOCH + 120))
        );
        assert_eq!(rig.claude.calls(), [], "starting and pausing spend nothing");
    }

    #[test]
    fn starting_a_running_project_keeps_its_since() {
        let rig = Rig::new("golbat");
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.clock.advance(3600);
        assert_eq!(rig.ask(&runner, "start", None)["since"], Rig::EPOCH);
    }

    #[test]
    fn a_failed_save_is_reported_and_changes_nothing() {
        let rig = Rig::new("xilriws");
        let runner = rig.open().unwrap();
        let folder = rig.paths().state.parent().unwrap().to_owned();
        std::fs::remove_dir_all(&folder).unwrap();
        // A file where the folder was, which a save cannot make a folder of.
        std::fs::write(&folder, "").unwrap();
        let reply = rig.ask(&runner, "start", None);
        assert!(
            reply["error"]
                .as_str()
                .unwrap()
                .contains("cannot write state file"),
            "{reply}"
        );
        assert_eq!(rig.ask(&runner, "status", None)["run"], "paused");
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
        assert!(err.to_string().starts_with("setting `repo`: "), "{err}");
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
        assert!(err.starts_with("setting `repo`: "), "{err}");
        assert!(
            err.ends_with("koji has no `origin` remote to cut branches from"),
            "{err}"
        );
    }

    #[test]
    fn a_repo_that_does_not_exist_stops_the_runner() {
        let rig = Rig::new("reactmap");
        let gone = rig.repo().display().to_string();
        rig.edit_settings(|s| s.replace(&gone, &format!("{gone}-gone")));
        let err = rig.open().unwrap_err().to_string();
        assert!(err.starts_with("setting `repo`: "), "{err}");
        assert!(err.ends_with("reactmap-gone is not a folder"), "{err}");
    }

    #[test]
    fn coderabbit_on_for_a_repo_that_is_not_public_stops_the_runner() {
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
                    "setting `coderabbit.enabled`: shep-pm/shep is {seen_by}, \
                     and CodeRabbit's free plan reviews public repos only"
                )
            );
        }
    }

    #[test]
    fn coderabbit_on_for_a_public_repo_asks_the_forge_once() {
        let rig = Rig::new("shep");
        rig.coderabbit_on();
        rig.open().unwrap();
        assert_eq!(rig.forge.calls(), 1);
    }

    #[test]
    fn coderabbit_off_never_asks_the_forge() {
        let rig = Rig::new("zeus");
        rig.forge.set_visibility(Visibility::Private);
        rig.open().unwrap();
        assert_eq!(rig.forge.calls(), 0);
    }
}
