//! The project runner
//!
//! One runner per project, run as a sheep under kelpie's own shepherd. It
//! checks the project's settings when it starts, keeps the project's state
//! file, answers the maintainer's triggers, and runs the worker's turns.
//! Every change is saved before it takes effect in memory.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::board::{LabelError, Skip, WorkerModel, worker_override};
use crate::channels::{Channel, Channels};
use crate::pacer::Assessment;
use crate::ports::{ForgeError, Guarded, Ports, SessionId, Timestamp, Visibility};
use crate::settings::{Settings, SettingsError};
use crate::state::{ProjectState, RunState, StateError, StateStore};
use crate::webhook::{KelpieSettings, Webhook};
use crate::work_item::{
    CodeRabbitTally, Known, Phase, QwenTally, ReviewCallState, Turn, WorkItem, new_session_id,
};

mod adopt;
mod alert;
mod claude_files;
mod coderabbit;
mod dispatch;
mod gate;
mod instructions;
mod merge;
mod pace;
mod paths;
mod question;
mod replies;
mod report;
mod reread;
mod review;
mod rework;
mod ruling;
#[cfg(test)]
mod several;
mod shots;
mod trigger;
mod turn;

pub use adopt::AdoptError;
pub use merge::DropError;
pub use pace::PacerStatus;
pub use paths::{ProjectName, ProjectNameError, ProjectPaths};
pub use replies::READ_EVERY;
pub use report::StepReport;
pub use rework::ReworkError;
pub use ruling::{Answer, RuleError};
use trigger::issue_list;
pub use trigger::{ACTIONS, RELAY_RULE, Status, WorkItemStatus, answer, is_no_or_answer};
pub use trigger::{GateError, WhichItem};
pub use turn::step;

#[cfg(test)]
pub(crate) use gate::CHECKS_SETTLE;

/// Why a runner could not start
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// A setting is missing, malformed or does not hold
    Settings(SettingsError),
    /// The forge could not be asked about the project's repo
    Forge(ForgeError),
    /// The state file could not be read
    State(StateError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Settings(e) => e.fmt(f),
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
    /// The issue's `worker:` label cannot be used
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
    paths: ProjectPaths,
    kelpie: PathBuf,
    store: StateStore,
    state: ProjectState,
    ports: Ports,
    // The pacer's last reading of usage and when it was read, kept in memory only
    pacing: Option<(Timestamp, Assessment)>,
    // What the board passed over on its last poll, kept in memory only
    skipped: Vec<Skip>,
    // None when rulings do not go to the webhook
    webhook: Option<Webhook>,
    channels: Channels,
    // The last failed webhook post, kept in memory so a restart tries at once
    retry: Option<alert::Retry>,
    // When the relay was last cleared, kept in memory only: a restart may
    // clear a session sooner than a full day, never later.
    relay_cleared: Option<Timestamp>,
    // Notices for the relay of rulings settled without it, kept in memory
    // only: one lost to a restart leaves the question up, and `rule`
    // refuses a tap on it.
    relay_notices: Vec<alert::SettledNotice>,
    // The ruling whose relay send is out, which an answer can settle first
    relaying: Option<u64>,
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
        let local = [home, paths.kelpie_home.as_path(), settings.repo.as_path()];
        ports.forge = Box::new(Guarded::new(ports.forge, local));
        let (channels, webhook) = ruling_channels(&settings, kelpie_settings)?;
        let totp = replies::authenticator(webhook.as_ref(), &paths.totp)?;
        check_repo(&settings)?;
        let extra_instructions = instructions::read_extra(&settings)?;
        check_coderabbit(&settings, &ports)?;
        check_local(&settings, &ports)?;
        let store = StateStore::new(paths.state.clone());
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
                item.review_call = ReviewCallState::Idle;
            }
            store.save(&state)?;
        }
        // A dev server the last run's worker left behind holds its port.
        for item in &state.work_items {
            ports
                .shots
                .stop_left(&paths.shots(item.issue).join(crate::shots::SERVER_PID));
        }
        // A new run is a new epoch, and the dog reclaims what the old one held.
        if !state.leases.is_empty() {
            state.leases.clear();
            store.save(&state)?;
        }
        Ok(Self {
            project,
            settings,
            extra_instructions,
            paths: paths.clone(),
            kelpie: kelpie.to_owned(),
            store,
            state,
            ports,
            pacing: None,
            skipped: Vec::new(),
            webhook,
            channels,
            retry: None,
            relay_cleared: None,
            relay_notices: Vec::new(),
            relaying: None,
            reading: replies::Reading::default(),
            totp,
            viewer: None,
            focus: None,
            last_acted: None,
        })
    }

    /// The project's settings, as read when the runner started
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The project's state as `status` reports it
    pub fn status(&self) -> Status<'_> {
        Status {
            project: self.project.as_str(),
            merge_authority: self.settings.merge_authority,
            run: self.state.run,
            since: self.state.since,
            work_item: self.state.work_items.first().map(WorkItemStatus::from),
            work_items: self
                .state
                .work_items
                .iter()
                .map(WorkItemStatus::from)
                .collect(),
            max_items: self.settings.max_items.get(),
            adopted: &self.state.adopted,
            skipped: &self.skipped,
            rulings: &self.state.rulings,
            leases: &self.state.leases,
            pacer: self.pacer_status(self.ports.clock.now()),
        }
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

    /// Opens a work item for `issue`, and returns the model and effort its
    /// worker runs on. Its first turn runs once the project is running.
    ///
    /// # Errors
    ///
    /// [`AddError`] when the project has `max_items` open or one for this
    /// issue already, the issue cannot be read or its `worker:` label
    /// understood, or the change cannot be saved. Nothing changes then.
    pub fn add(&mut self, issue: u64) -> Result<WorkerModel, AddError> {
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
        let worker = worker_override(&found.labels)
            .map_err(AddError::Label)?
            .unwrap_or_else(|| WorkerModel::from(&self.settings.models.worker));
        let session = new_session_id().map_err(|e| AddError::Session(e.to_string()))?;
        let mut next = self.state.clone();
        next.work_items
            .push(self.fresh(issue, found.title, worker.clone(), session));
        self.save(next).map_err(AddError::State)?;
        Ok(worker)
    }

    // A work item on `kelpie/<issue>` whose first turn is due, with nothing
    // recorded yet
    fn fresh(
        &self,
        issue: u64,
        title: String,
        worker: WorkerModel,
        session: SessionId,
    ) -> WorkItem {
        WorkItem {
            issue,
            title,
            branch: format!("kelpie/{issue}"),
            rework: false,
            adopted: false,
            arrived: None,
            worktree: self.paths.worktree(issue),
            build: self.paths.build(issue),
            worker,
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
            merge_tried: None,
            summon_owed: false,
            rebased: false,
            shots: None,
            shots_comment: None,
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

    fn save(&mut self, next: ProjectState) -> Result<(), StateError> {
        self.store.save(&next)?;
        self.state = next;
        Ok(())
    }
}

fn check_repo(settings: &Settings) -> Result<(), SettingsError> {
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

// The project's channels, else kelpie's, else every one; and the webhook
// they need, which only a project that posts to the webhook cannot do without.
fn ruling_channels(
    settings: &Settings,
    kelpie: KelpieSettings,
) -> Result<(Channels, Option<Webhook>), SettingsError> {
    let channels = (settings.ruling_channels.clone())
        .or(kelpie.ruling_channels)
        .unwrap_or_default();
    if !channels.has(Channel::Webhook) {
        return Ok((channels, None));
    }
    match kelpie.webhook {
        Some(webhook) => Ok((channels, Some(webhook))),
        None => Err(SettingsError::Invalid {
            setting: "ruling_channels",
            reason: "rulings go to the webhook, and kelpie's settings name none: add a \
                     `webhook` table to its [kelpie] section of dogs.toml, or drop `webhook` \
                     from `ruling_channels`"
                .to_owned(),
        }),
    }
}

// The local round's command is there, or its endpoint answers.
fn check_local(settings: &Settings, ports: &Ports) -> Result<(), SettingsError> {
    ports
        .reviewer
        .check(&settings.review.local)
        .map_err(|reason| SettingsError::Invalid {
            setting: "review.local",
            reason,
        })
}

fn check_coderabbit(settings: &Settings, ports: &Ports) -> Result<(), OpenError> {
    if !settings.coderabbit.enabled {
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
            "{} is {visibility}, and CodeRabbit's free plan reviews public repos only",
            settings.forge.as_str()
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
        std::fs::remove_dir_all(rig.paths().state.parent().unwrap()).unwrap();
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
