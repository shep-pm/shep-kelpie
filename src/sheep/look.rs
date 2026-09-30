//! A runner's look at its own tables, at start and each time it wakes
//!
//! Reading them each wake is how a change made in lookout reaches a running
//! runner: within a minute when idle, and at the next wake after a step.
//! A read that fails, or a change the runner refuses, keeps the settings in
//! effect and says so once. A refused change is offered again each wake,
//! so it lands once what refused it is fixed.

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use crate::runner::Runner;
use crate::settings::Settings;
use crate::settings::source::{self, Files, Loaded};
use crate::shepherd;
use crate::webhook::KelpieSettings;

/// Where a runner's settings are read from, and what it read last
pub(super) struct Look {
    shep_home: PathBuf,
    sheep: String,
    project: String,
    settings: PathBuf,
    kelpie_settings: PathBuf,
    home: PathBuf,
    last: Option<(Settings, KelpieSettings)>,
    failed: Option<String>,
}

impl Look {
    pub(super) fn new(
        shep_home: PathBuf,
        sheep: String,
        project: String,
        settings: PathBuf,
        kelpie_settings: PathBuf,
        home: PathBuf,
    ) -> Self {
        Self {
            shep_home,
            sheep,
            project,
            settings,
            kelpie_settings,
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
        self.last = Some((loaded.settings.clone(), loaded.kelpie.clone()));
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
    use crate::settings::MergeAuthority;
    use crate::shepherd::SHEP_VERSION;
    use crate::test::{Rig, project_table};

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
            paths.settings.clone(),
            paths.kelpie_settings.clone(),
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
            paths.settings.clone(),
            paths.kelpie_settings.clone(),
            rig.home.path().to_owned(),
        );
        look.last = Some((rig.settings(), rig.kelpie_settings()));

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
            paths.settings.clone(),
            paths.kelpie_settings.clone(),
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
}
