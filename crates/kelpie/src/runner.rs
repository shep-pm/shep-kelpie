//! The project runner
//!
//! One runner per project, run as a sheep under kelpie's own shepherd. It
//! reads the project's settings when it starts, keeps the project's state
//! file, answers the maintainer's triggers, and runs the worker's turns.
//! Every change is saved before it takes effect in memory.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::board::{LabelError, WorkerModel, worker_override};
use crate::ports::{ForgeError, Ports, Visibility};
use crate::settings::{Settings, SettingsError};
use crate::state::{ProjectState, RunState, StateError, StateStore};
use crate::work_item::{Phase, Turn, WorkItem, new_session_id};

mod dispatch;
mod gate;
mod merge;
mod paths;
mod report;
mod ruling;
mod trigger;
mod turn;

pub use paths::{ProjectName, ProjectNameError, ProjectPaths};
pub use report::StepReport;
pub use ruling::{Answer, RuleError};
pub use trigger::GateError;
pub use trigger::{ACTIONS, Status, WorkItemStatus, answer};
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

impl std::error::Error for OpenError {}

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
    /// A work item is already in flight, for this issue
    InFlight(u64),
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
            Self::InFlight(issue) => write!(f, "the work item for #{issue} is in flight"),
            Self::Forge(e) => write!(f, "cannot read the issue: {e}"),
            Self::Label(e) => e.fmt(f),
            Self::Session(e) => write!(f, "cannot draw a session id: {e}"),
            Self::State(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for AddError {}

/// One project's runner
#[derive(Debug)]
pub struct Runner {
    project: ProjectName,
    settings: Settings,
    paths: ProjectPaths,
    kelpie: PathBuf,
    store: StateStore,
    state: ProjectState,
    ports: Ports,
}

impl Runner {
    /// Reads the project's settings and state and checks the settings hold
    ///
    /// `home` is the maintainer's home folder, for `~/` in settings.
    /// `kelpie` is the kelpie binary, which each worker's file-tool hook runs.
    ///
    /// # Errors
    ///
    /// [`OpenError`] naming the setting, forge call or file that failed.
    pub fn open(
        project: ProjectName,
        paths: &ProjectPaths,
        home: &Path,
        kelpie: &Path,
        ports: Ports,
    ) -> Result<Self, OpenError> {
        let settings = Settings::load(&paths.settings, home)?;
        check_repo(&settings)?;
        check_coderabbit(&settings, &ports)?;
        let store = StateStore::new(paths.state.clone());
        let state = store
            .load()?
            .unwrap_or_else(|| ProjectState::new(ports.clock.now()));
        Ok(Self {
            project,
            settings,
            paths: paths.clone(),
            kelpie: kelpie.to_owned(),
            store,
            state,
            ports,
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
            run: self.state.run,
            since: self.state.since,
            work_item: self.state.work_item.as_ref().map(WorkItemStatus::from),
            rulings: &self.state.rulings,
            leases: &self.state.leases,
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

    /// Makes `issue` the work item in flight, and returns the model and
    /// effort its worker runs on. Its first turn runs once the project is running.
    ///
    /// # Errors
    ///
    /// [`AddError`] when a work item is in flight, the issue cannot be read
    /// or its `worker:` label understood, or the change cannot be saved.
    /// Nothing changes then.
    pub fn add(&mut self, issue: u64) -> Result<WorkerModel, AddError> {
        if let Some(item) = &self.state.work_item {
            return Err(AddError::InFlight(item.issue));
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
        next.work_item = Some(WorkItem {
            issue,
            title: found.title,
            branch: format!("kelpie/{issue}"),
            worktree: self.paths.worktree(issue),
            build: self.paths.build(issue),
            worker: worker.clone(),
            session,
            turn: Turn::Due,
            pull_request: None,
            phase: Phase::Implement,
            red_head: None,
            calls: Vec::new(),
        });
        self.save(next).map_err(AddError::State)?;
        Ok(worker)
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
        rig.open().unwrap();
        assert_eq!(rig.forge.calls(), 1);
    }

    #[test]
    fn coderabbit_off_never_asks_the_forge() {
        let rig = Rig::new("zeus");
        rig.forge.set_visibility(Visibility::Private);
        rig.edit_settings(|s| s.replace("enabled = true", "enabled = false"));
        rig.open().unwrap();
        assert_eq!(rig.forge.calls(), 0);
    }

    #[test]
    fn a_missing_settings_file_stops_the_runner_naming_it() {
        let rig = Rig::new("koji");
        std::fs::remove_file(&rig.paths().settings).unwrap();
        let err = rig.open().unwrap_err().to_string();
        assert!(err.starts_with("cannot read settings file "), "{err}");
        assert!(err.contains("projects/koji/settings.toml"), "{err}");
    }
}
