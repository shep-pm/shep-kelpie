//! The project runner
//!
//! One runner per project, run as a sheep under kelpie's own shepherd. It
//! reads the project's settings when it starts, keeps the project's state
//! file, and answers the maintainer's `status`, `start` and `pause`
//! triggers. Every change is saved before it takes effect in memory.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

use serde::Serialize;

use crate::ports::{ForgeError, Ports, Timestamp, Visibility};
use crate::settings::{Settings, SettingsError};
use crate::state::{LeaseHeld, ProjectState, Ruling, RunState, StateError, StateStore, WorkItem};

/// The triggers a runner answers
pub const ACTIONS: [&str; 3] = ["status", "start", "pause"];

/// A project's name, which is also its sheep's name
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectName(String);

impl ProjectName {
    /// The name as written
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for ProjectName {
    type Error = ProjectNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        if value.is_empty() || value.starts_with('.') || !value.chars().all(allowed) {
            return Err(ProjectNameError(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }
}

/// A name that is not one plain path component, carrying the name
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectNameError(pub String);

impl fmt::Display for ProjectNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} is not a project name: use letters, digits, - _ .",
            self.0
        )
    }
}

impl std::error::Error for ProjectNameError {}

/// Where a project's files live under kelpie's home
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPaths {
    /// The settings file
    pub settings: PathBuf,
    /// The state file
    pub state: PathBuf,
}

impl ProjectPaths {
    /// `<kelpie home>/projects/<project>/`
    pub fn under(kelpie_home: &Path, project: &ProjectName) -> Self {
        let folder = kelpie_home.join("projects").join(project.as_str());
        Self {
            settings: folder.join("settings.toml"),
            state: folder.join("state.json"),
        }
    }
}

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
    pub work_item: Option<&'a WorkItem>,
    /// Rulings waiting on the maintainer, oldest first
    pub rulings: &'a [Ruling],
    /// Leases held
    pub leases: &'a [LeaseHeld],
}

/// One project's runner
#[derive(Debug)]
pub struct Runner {
    project: ProjectName,
    settings: Settings,
    store: StateStore,
    state: ProjectState,
    ports: Ports,
}

impl Runner {
    /// Reads the project's settings and state and checks the settings hold
    ///
    /// `home` is the maintainer's home folder, for `~/` in settings.
    ///
    /// # Errors
    ///
    /// [`OpenError`] naming the setting, forge call or file that failed.
    pub fn open(
        project: ProjectName,
        paths: &ProjectPaths,
        home: &Path,
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
            store,
            state,
            ports,
        })
    }

    /// The project's settings
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The project's state as `status` reports it
    pub fn status(&self) -> Status<'_> {
        Status {
            project: self.project.as_str(),
            run: self.state.run,
            since: self.state.since,
            work_item: self.state.work_item.as_ref(),
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

    fn set_run(&mut self, run: RunState) -> Result<(), StateError> {
        if self.state.run == run {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.run = run;
        next.since = self.ports.clock.now();
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
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| invalid(format!("cannot run git to check it: {e}")))?;
    if output.status.success() && output.stdout.trim_ascii() == b"true" {
        return Ok(());
    }
    Err(invalid(format!(
        "{} is not a git work tree",
        repo.display()
    )))
}

fn check_coderabbit(settings: &Settings, ports: &Ports) -> Result<(), OpenError> {
    if !settings.coderabbit.enabled {
        return Ok(());
    }
    let seen_by = match ports.forge.visibility(&settings.forge)? {
        Visibility::Public => return Ok(()),
        Visibility::Private => "private",
        Visibility::Internal => "internal",
    };
    Err(SettingsError::Invalid {
        setting: "coderabbit.enabled",
        reason: format!(
            "{} is {seen_by}, and CodeRabbit's free plan reviews public repos only",
            settings.forge.as_str()
        ),
    }
    .into())
}

/// Answers one trigger with a JSON body: the status, or `{"error": ...}`
pub fn answer(runner: &Mutex<Runner>, action: &str, params: Option<&str>) -> String {
    let error = |message: String| serde_json::json!({ "error": message }).to_string();
    if params.is_some_and(|p| !p.trim().is_empty()) {
        return error(format!("`{action}` takes no params"));
    }
    // Memory changes only after a save succeeds, so a panicked holder
    // cannot have left the runner half changed.
    let mut runner = runner.lock().unwrap_or_else(PoisonError::into_inner);
    let changed = match action {
        "status" => Ok(()),
        "start" => runner.start(),
        "pause" => runner.pause(),
        _ => return error(format!("unknown action `{action}`")),
    };
    match changed {
        Ok(()) => serde_json::to_string(&runner.status()).expect("status serializes to JSON"),
        Err(e) => error(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test::Rig;

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
    fn every_registered_action_is_answered_and_no_other() {
        let rig = Rig::new("koji");
        let runner = rig.open().unwrap();
        for action in ACTIONS {
            assert_eq!(
                rig.ask(&runner, action, None)["project"],
                "koji",
                "{action}"
            );
        }
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

    #[test]
    fn a_project_name_is_one_path_component() {
        for bad in ["", "a/b", "..", ".hidden", "sp ace"] {
            assert!(ProjectName::try_from(bad).is_err(), "{bad:?}");
        }
        assert!(ProjectName::try_from("shep-kelpie_2.0").is_ok());
    }
}
