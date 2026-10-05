//! A runner's look at its own tables, at start and each time it wakes
//!
//! Reading them each wake is how a change made in lookout reaches a running
//! runner: within a minute when idle, and at the next wake after a step.
//! The agent files are looked at with them, so an edit to one lands the same way.
//! A read that fails, or a change the runner refuses, keeps the settings in
//! effect and says so once. A refused change is offered again each wake,
//! so it lands once what refused it is fixed.

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use crate::agents;
use crate::runner::Runner;
use crate::settings::Settings;
use crate::settings::source::{self, Files, Loaded};
use crate::shepherd;
use crate::webhook::KelpieSettings;

/// What a look read: the project's settings, kelpie's, and the agent files' text
type Read = (Settings, KelpieSettings, Vec<(String, String)>);

/// Where a runner's settings are read from, and what it read last
pub(super) struct Look {
    shep_home: PathBuf,
    sheep: String,
    project: String,
    settings: PathBuf,
    kelpie_settings: PathBuf,
    agents: PathBuf,
    home: PathBuf,
    last: Option<Read>,
    failed: Option<String>,
}

/// The files a runner's settings are read from
pub(super) struct Sources {
    /// The project's settings file from before its table
    pub(super) settings: PathBuf,
    /// Kelpie's settings file from before its section
    pub(super) kelpie_settings: PathBuf,
    /// Kelpie's agent files
    pub(super) agents: PathBuf,
}

impl Look {
    pub(super) fn new(
        shep_home: PathBuf,
        sheep: String,
        project: String,
        sources: Sources,
        home: PathBuf,
    ) -> Self {
        Self {
            shep_home,
            sheep,
            project,
            settings: sources.settings,
            kelpie_settings: sources.kelpie_settings,
            agents: sources.agents,
            home,
            last: None,
            failed: None,
        }
    }

    /// Reads the tables, or the files standing in for them, and remembers them
    pub(super) fn read(&mut self) -> Result<Loaded, String> {
        let tables = shepherd::block_on(shepherd::read_tables(&self.shep_home, &self.sheep))?;
        let files = Files {
            project: &self.project,
            sheep: &self.sheep,
            settings: &self.settings,
            kelpie_settings: &self.kelpie_settings,
        };
        let loaded = source::load(&tables, files, &self.home).map_err(|e| e.to_string())?;
        let agents = agents::snapshot(&self.agents);
        self.last = Some((loaded.settings.clone(), loaded.kelpie.clone(), agents));
        Ok(loaded)
    }

    /// Reads again, and hands `runner` whatever changed since the last read
    pub(super) fn again(&mut self, runner: &Mutex<Runner>) {
        let before = self.last.clone();
        let loaded = match self.read() {
            Ok(loaded) => loaded,
            Err(e) => {
                if self.failed.as_ref() != Some(&e) {
                    eprintln!("cannot read the settings again, so they stay as they are: {e}");
                    self.failed = Some(e);
                }
                return;
            }
        };
        if before == self.last {
            self.failed = None;
            return;
        }
        let mut runner = runner.lock().unwrap_or_else(PoisonError::into_inner);
        match runner.reread(loaded.settings, loaded.kelpie) {
            Ok(line) => {
                self.failed = None;
                if let Some(line) = line {
                    eprintln!("{line}");
                }
            }
            Err(e) => {
                let e = e.to_string();
                if self.failed.as_ref() != Some(&e) {
                    eprintln!("a settings change was refused, so they stay as they are: {e}");
                    self.failed = Some(e);
                }
                self.last = before;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use serde_json::{Map, Value};
    use shep_client::shep_core::config::DogTable;
    use shep_client::shep_core::protocol::Request;
    use shep_client::shep_core::protocol::request::Response;
    use shep_client::testing::{fake_daemon_answering_with_ack, sample_ack};

    use super::*;
    use crate::runner::{StepReport, step};
    use crate::settings::MergeAuthority;
    use crate::shepherd::SHEP_VERSION;
    use crate::test::{Rig, Scripted, project_table};

    const SECTION: &str = "[webhook]\nkind = \"ntfy\"\nurl = \"https://ntfy.example/t\"\n";

    // A shepherd on a thread of its own, holding whatever `table` holds when
    // asked. It answers until the returned sender is dropped.
    fn shepherd(
        home: &std::path::Path,
        table: Arc<Mutex<Map<String, Value>>>,
    ) -> tokio::sync::oneshot::Sender<()> {
        std::fs::create_dir_all(home.join("run")).unwrap();
        let socket = home.join("run/shep.sock");
        let mut ack = sample_ack();
        ack.daemon_version = SHEP_VERSION.into();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (up, is_up) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let answer = move |request: &Request| match request {
                    Request::DogSheepSettings { .. } => Response::DogSheepSettings {
                        tables: BTreeMap::from([(
                            "shep".to_owned(),
                            DogTable::from(table.lock().unwrap().clone()),
                        )]),
                    },
                    Request::DogConfig { .. } => Response::DogSection {
                        toml: SECTION.to_owned().into(),
                    },
                    other => panic!("the runner asked {other:?}"),
                };
                let _sent = fake_daemon_answering_with_ack(&socket, ack, answer).await;
                up.send(()).unwrap();
                let _ = stopped.await;
            });
        });
        is_up.recv().unwrap();
        stop
    }

    #[test]
    fn a_read_that_fails_keeps_the_settings_in_effect() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let before = runner.lock().unwrap().settings().clone();
        let no_shepherd = tempfile::tempdir().unwrap();
        let paths = rig.paths();
        let mut look = Look::new(
            no_shepherd.path().to_owned(),
            "shep".into(),
            "shep".into(),
            Sources {
                settings: paths.settings.clone(),
                kelpie_settings: paths.kelpie_settings.clone(),
                agents: paths.agents.clone(),
            },
            rig.home.path().to_owned(),
        );
        look.again(&runner);
        look.again(&runner);
        assert_eq!(runner.lock().unwrap().settings(), &before);
        assert!(look.failed.as_deref().unwrap().starts_with("cannot reach"));
    }

    #[test]
    fn a_refused_change_lands_once_what_refused_it_is_fixed() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let entry = std::fs::read_to_string(rig.paths().settings).unwrap();
        let mut on = project_table(&entry);
        on["coderabbit"]["enabled"] = Value::Bool(true);
        let table = Arc::new(Mutex::new(on));
        let shep_home = tempfile::tempdir().unwrap();
        let _shepherd = shepherd(shep_home.path(), Arc::clone(&table));
        let paths = rig.paths();
        let mut look = Look::new(
            shep_home.path().to_owned(),
            "shep".into(),
            "shep".into(),
            Sources {
                settings: paths.settings.clone(),
                kelpie_settings: paths.kelpie_settings.clone(),
                agents: paths.agents.clone(),
            },
            rig.home.path().to_owned(),
        );
        let agents = agents::snapshot(&paths.agents);
        look.last = Some((rig.settings(), rig.kelpie_settings(), agents));

        rig.forge.set_visibility(crate::ports::Visibility::Private);
        look.again(&runner);
        assert!(!runner.lock().unwrap().settings().coderabbit.enabled);
        assert!(
            look.failed
                .as_deref()
                .unwrap()
                .contains("coderabbit.enabled")
        );

        rig.forge.set_visibility(crate::ports::Visibility::Public);
        look.again(&runner);
        assert!(runner.lock().unwrap().settings().coderabbit.enabled);
        assert_eq!(look.failed, None);
    }

    #[test]
    fn a_table_changed_in_lookout_reaches_the_running_runner() {
        let rig = Rig::new("shep");
        // The older form of the local round, which the rig's project sets, is a notice.
        rig.deep_review();
        let runner = rig.open().unwrap();
        let entry = std::fs::read_to_string(rig.paths().settings).unwrap();
        let table = Arc::new(Mutex::new(project_table(&entry)));
        let shep_home = tempfile::tempdir().unwrap();
        let _shepherd = shepherd(shep_home.path(), Arc::clone(&table));
        let paths = rig.paths();
        let mut look = Look::new(
            shep_home.path().to_owned(),
            "shep".into(),
            "shep".into(),
            Sources {
                settings: paths.settings.clone(),
                kelpie_settings: paths.kelpie_settings.clone(),
                agents: paths.agents.clone(),
            },
            rig.home.path().to_owned(),
        );
        assert!(look.read().unwrap().notices.is_empty());

        table
            .lock()
            .unwrap()
            .insert("merge_authority".into(), Value::String("auto".into()));
        look.again(&runner);

        let runner = runner.lock().unwrap();
        assert_eq!(runner.settings().merge_authority, MergeAuthority::Auto);
    }

    const MINE: &str = "---\nrole: implementer\nharness: claude-code\nmodel: claude-mine-1\n\
                        effort: low\n---\n";

    // A look at the rig's settings as they stand, through a shepherd that
    // holds them, read once so the next look sees only what changes after.
    fn looking(rig: &Rig) -> (Look, tokio::sync::oneshot::Sender<()>, tempfile::TempDir) {
        let entry = std::fs::read_to_string(rig.paths().settings).unwrap();
        let table = Arc::new(Mutex::new(project_table(&entry)));
        let shep_home = tempfile::tempdir().unwrap();
        let shepherd = shepherd(shep_home.path(), table);
        let paths = rig.paths();
        let mut look = Look::new(
            shep_home.path().to_owned(),
            "shep".into(),
            "shep".into(),
            Sources {
                settings: paths.settings.clone(),
                kelpie_settings: paths.kelpie_settings.clone(),
                agents: paths.agents.clone(),
            },
            rig.home.path().to_owned(),
        );
        look.read().unwrap();
        (look, shepherd, shep_home)
    }

    // Issue 7 on the agent `mine`, its first turn due.
    fn on_mine(rig: &Rig) -> Mutex<Runner> {
        rig.write_agent("mine", MINE);
        rig.implementers(&["sonnet-high", "mine"]);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.forge.label(7, "agent:mine");
        rig.ask(&runner, "add", Some("7"));
        runner
    }

    #[test]
    fn an_agent_file_edited_while_its_item_runs_reaches_the_next_turn() {
        let rig = Rig::new("shep");
        let runner = on_mine(&rig);
        let (mut look, _shepherd, _home) = looking(&rig);
        rig.claude.script([Scripted::Say("done")]);
        step(&runner).unwrap();

        rig.write_agent("mine", &MINE.replace("claude-mine-1", "claude-mine-2"));
        look.again(&runner);
        assert_eq!(look.failed, None);
        rig.claude.script([Scripted::Say("done")]);
        step(&runner).unwrap();
        let models: Vec<String> = rig.claude.calls().into_iter().map(|c| c.model).collect();
        assert_eq!(models, ["claude-mine-1", "claude-mine-2"]);
    }

    #[test]
    fn an_agent_file_written_back_lets_a_yes_retry_its_items_failed_turn() {
        let rig = Rig::new("shep");
        drop(on_mine(&rig));
        std::fs::remove_file(rig.paths().agents.join("mine.md")).unwrap();
        rig.implementers(&["sonnet-high"]);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Failed { .. })
        ));
        step(&runner).unwrap(); // the alert
        let (mut look, _shepherd, _home) = looking(&rig);

        rig.write_agent("mine", MINE);
        look.again(&runner);
        assert_eq!(look.failed, None);
        rig.ask(&runner, "rule", Some("1 yes"));
        rig.claude.script([Scripted::Say("done")]);
        step(&runner).unwrap();
        let [call] = rig.claude.calls().try_into().unwrap();
        assert_eq!(call.model, "claude-mine-1");
    }
}
