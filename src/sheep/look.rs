//! A runner's look at its own tables, at start and each time it wakes
//!
//! Reading them each wake is how a change made in lookout reaches a running
//! runner: within a minute when idle, and within `INTERVAL` while it works.
//! The agent files are looked at with them, so an edit to one lands the same way.
//! A read that fails keeps the settings in effect and says so once. So does a
//! change the runner refuses, a table that fails to load, or a moved
//! `git.checkout` or `git.remote` another project runs: those are also kept
//! on the runner for `status` and posted once to the webhook. A refused change
//! is offered again each wake, so it lands once what refused it is fixed.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use crate::agents;
use crate::flock::share::{self, Taken};
use crate::runner::Runner;
use crate::settings::Settings;
use crate::settings::source::{self, Loaded};
use crate::shepherd::{self, Dial};
use crate::webhook::KelpieSettings;

/// What a look read: the project's settings, kelpie's, and the agent files' text
type Read = (Settings, KelpieSettings, Vec<(String, String)>);

// Why a read gave no settings: the shepherd could not be asked, or the table is bad.
enum Unread {
    Shepherd(String),
    Table(String),
}

/// Where a runner's settings are read from, and what it read last
pub(super) struct Look {
    dial: Arc<dyn Dial>,
    shep_home: PathBuf,
    sheep: String,
    folder: PathBuf,
    agents: PathBuf,
    home: PathBuf,
    last: Option<Read>,
    failed: Option<String>,
}

/// The folders a runner's settings and agents are read from
pub(super) struct Sources {
    /// The project's own folder, which a relative path in its settings is taken from
    pub(super) folder: PathBuf,
    /// Kelpie's agent files
    pub(super) agents: PathBuf,
}

impl Look {
    pub(super) fn new(
        dial: Arc<dyn Dial>,
        shep_home: PathBuf,
        sheep: String,
        sources: Sources,
        home: PathBuf,
    ) -> Self {
        Self {
            dial,
            shep_home,
            sheep,
            folder: sources.folder,
            agents: sources.agents,
            home,
            last: None,
            failed: None,
        }
    }

    /// Reads the tables, and remembers them
    pub(super) fn read(&mut self) -> Result<Loaded, String> {
        self.fetch().map_err(|e| match e {
            Unread::Shepherd(why) | Unread::Table(why) => why,
        })
    }

    fn fetch(&mut self) -> Result<Loaded, Unread> {
        let tables = shepherd::block_on(shepherd::read_tables(
            &*self.dial,
            &self.shep_home,
            &self.sheep,
        ))
        .map_err(Unread::Shepherd)?;
        let loaded = source::load(&tables, &self.sheep, &self.home, &self.folder)
            .map_err(|e| Unread::Table(e.to_string()))?;
        let agents = agents::snapshot(&self.agents);
        self.last = Some((loaded.settings.clone(), loaded.kelpie.clone(), agents));
        Ok(loaded)
    }

    /// The sheep whose tables this reads
    pub(super) fn sheep(&self) -> &str {
        &self.sheep
    }

    /// Whether another project runs the checkout or repo `settings` name
    pub(super) fn taken(&self, settings: &Settings) -> Result<Option<Taken>, String> {
        shepherd::block_on(async {
            let client = shepherd::connect(&*self.dial, &self.shep_home)
                .await
                .map_err(|e| e.describe(&self.shep_home))?;
            share::check(&client, &self.sheep, &self.home, settings).await
        })
    }

    // Says `line` and `why` once, until something else is said or a read holds.
    fn say(&mut self, line: &str, why: &str) {
        if self.failed.as_deref() != Some(why) {
            eprintln!("{line}: {why}");
            self.failed = Some(why.to_owned());
        }
    }

    // Why the git settings in `settings`, where they moved from the runner's,
    // are another project's, or why that cannot be told.
    fn taken_by_another(
        &self,
        runner: &Mutex<Runner>,
        settings: &Settings,
    ) -> Result<Option<String>, String> {
        let moved = {
            let runner = runner.lock().unwrap_or_else(PoisonError::into_inner);
            let have = &runner.settings().git;
            let mut moved = Vec::new();
            if settings.git.checkout != have.checkout {
                moved.push("git.checkout");
            }
            if settings.git.remote != have.remote {
                moved.push("git.remote");
            }
            moved.join(" and ")
        };
        if moved.is_empty() {
            return Ok(None);
        }
        let taken = self.taken(settings).map_err(|e| {
            format!("cannot tell whether another project runs the new {moved}: {e}")
        })?;
        Ok(taken.map(|taken| format!("{moved}: {}", taken.clash())))
    }

    // A refusal of the stored table, which `status` shows and the webhook is told of.
    fn refuse(&mut self, runner: &Mutex<Runner>, line: &str, why: &str) {
        self.say(line, why);
        (runner.lock())
            .unwrap_or_else(PoisonError::into_inner)
            .refuse(why);
    }

    /// Reads again, and hands `runner` whatever changed since the last read
    pub(super) fn again(&mut self, runner: &Mutex<Runner>) {
        const KEPT: &str = "cannot read the settings again, so they stay as they are";
        const WAIT: &str = "a git settings change waits, so they stay as they are";
        const BAD: &str = "the settings table does not load, so they stay as they are";
        const REFUSED: &str = "a settings change was refused, so they stay as they are";
        let before = self.last.clone();
        let loaded = match self.fetch() {
            Ok(loaded) => loaded,
            Err(Unread::Shepherd(why)) => return self.say(KEPT, &why),
            Err(Unread::Table(why)) => return self.refuse(runner, BAD, &why),
        };
        if before == self.last {
            self.failed = None;
            // The repo's plugins come from its default branch, which no
            // setting of kelpie's announces.
            let mut runner = runner.lock().unwrap_or_else(PoisonError::into_inner);
            runner.accept();
            if let Some(line) = runner.reread_plugins() {
                eprintln!("{line}");
            }
            return;
        }
        match self.taken_by_another(runner, &loaded.settings) {
            Ok(None) => {}
            Ok(Some(why)) => {
                self.last = before;
                return self.refuse(runner, REFUSED, &why);
            }
            // The one-runner rule is not skipped: the change waits, and the
            // next wake asks again.
            Err(why) => {
                self.last = before;
                return self.say(WAIT, &why);
            }
        }
        let mut runner = runner.lock().unwrap_or_else(PoisonError::into_inner);
        match runner.reread(loaded.settings, loaded.kelpie) {
            Ok(line) => {
                self.failed = None;
                runner.accept();
                if let Some(line) = line {
                    eprintln!("{line}");
                }
            }
            Err(e) => {
                self.last = before;
                let why = e.to_string();
                self.say(REFUSED, &why);
                runner.refuse(&why);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use serde_json::{Map, Value};
    use shep_client::shep_core::config::DogTable;
    use shep_client::shep_core::protocol::Request;
    use shep_client::shep_core::protocol::request::Response;

    use super::*;
    use crate::home::ProjectName;
    use crate::runner::{StepReport, step};
    use crate::settings::{AgentName, Merging};
    use crate::shepherd::SHEP_VERSION;
    use crate::test::{FakeShepherd, Memory, Rig, Scripted, project_table};

    const SECTION: &str = "[webhook]\nkind = \"ntfy\"\nurl = \"https://ntfy.example/t\"\n";

    // `agents.allow` naming `names`
    fn allow(names: &[&str]) -> crate::settings::Allow {
        let name = |n: &&str| AgentName::try_from((*n).to_owned()).unwrap();
        names.iter().map(name).collect::<Vec<_>>().into()
    }

    // A shepherd in memory, holding whatever `table` holds when asked.
    fn shepherd(home: &std::path::Path, table: Arc<Mutex<Map<String, Value>>>) -> Arc<dyn Dial> {
        let (shepherd, _sent) =
            Memory::answering(home, SHEP_VERSION, move |request| match request {
                Request::DogSheepSettings { .. } => Response::DogSheepSettings {
                    tables: BTreeMap::from([(
                        "shep".to_owned(),
                        DogTable::from(table.lock().unwrap().clone()),
                    )]),
                },
                Request::DogConfigResolved { .. } => Response::DogSection {
                    toml: SECTION.to_owned().into(),
                },
                other => panic!("the runner asked {other:?}"),
            });
        Arc::new(shepherd)
    }

    #[test]
    fn a_read_that_fails_keeps_the_settings_in_effect() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let before = runner.lock().unwrap().settings().clone();
        let no_shepherd = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let paths = rig.paths();
        let mut look = Look::new(
            shepherd(elsewhere.path(), Arc::default()),
            no_shepherd.path().to_owned(),
            "shep".into(),
            Sources {
                folder: paths.folder.clone(),
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
        let mut settings = rig.settings();
        settings.agents.allow = Some(allow(&["sonnet-high", "claude"]));
        let runner = rig.open_settings(settings.clone()).unwrap();
        let entry = std::fs::read_to_string(rig.settings_file()).unwrap();
        let mut on = project_table(&entry);
        let named = ["sonnet-high", "claude", "gone"].map(|n| Value::String(n.into()));
        on["agents"]["allow"] = Value::Array(named.to_vec());
        let table = Arc::new(Mutex::new(on));
        let shep_home = tempfile::tempdir().unwrap();
        let dial = shepherd(shep_home.path(), Arc::clone(&table));
        let paths = rig.paths();
        let mut look = Look::new(
            dial,
            shep_home.path().to_owned(),
            "shep".into(),
            Sources {
                folder: paths.folder.clone(),
                agents: paths.agents.clone(),
            },
            rig.home.path().to_owned(),
        );
        let agents = agents::snapshot(&paths.agents);
        look.last = Some((settings, rig.kelpie_settings(), agents));

        // A reviewer the project names whose command is not there refuses the change.
        let script = rig.home.path().join("bin/gone");
        let agent_file = format!(
            "---\nrole: reviewer\nharness: command\ncommand: {}\n---\n",
            script.display()
        );
        rig.write_agent("gone", &agent_file);
        let allows_gone = |runner: &std::sync::Mutex<crate::runner::Runner>| {
            let settings = runner.lock().unwrap().settings().clone();
            (settings.agents.allow).is_some_and(|allow| allow.holds(&AgentName::kelpies("gone")))
        };
        look.again(&runner);
        assert!(!allows_gone(&runner));
        let failed = look.failed.clone().unwrap();
        assert!(failed.contains("agents.allow"), "{failed}");
        assert!(failed.contains("gone: cannot run"), "{failed}");

        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        crate::test::write_script(&script, "#!/bin/sh\nexit 0\n");
        look.again(&runner);
        assert!(allows_gone(&runner));
        assert_eq!(look.failed, None);
    }

    #[test]
    fn a_table_changed_in_lookout_reaches_the_running_runner() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let entry = std::fs::read_to_string(rig.settings_file()).unwrap();
        let table = Arc::new(Mutex::new(project_table(&entry)));
        let shep_home = tempfile::tempdir().unwrap();
        let dial = shepherd(shep_home.path(), Arc::clone(&table));
        let paths = rig.paths();
        let mut look = Look::new(
            dial,
            shep_home.path().to_owned(),
            "shep".into(),
            Sources {
                folder: paths.folder.clone(),
                agents: paths.agents.clone(),
            },
            rig.home.path().to_owned(),
        );
        look.read().unwrap();

        let mut table = table.lock().unwrap();
        let git = table.get_mut("git").and_then(Value::as_object_mut).unwrap();
        git.insert("merging".into(), Value::String("auto".into()));
        drop(table);
        look.again(&runner);

        let runner = runner.lock().unwrap();
        assert_eq!(runner.settings().git.merging, Merging::Auto);
    }

    #[test]
    fn a_domain_the_sandbox_would_refuse_keeps_the_settings_and_is_named() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let before = runner.lock().unwrap().settings().clone();
        let entry = std::fs::read_to_string(rig.settings_file()).unwrap();
        let table = Arc::new(Mutex::new(project_table(&entry)));
        let shep_home = tempfile::tempdir().unwrap();
        let dial = shepherd(shep_home.path(), Arc::clone(&table));
        let paths = rig.paths();
        let mut look = Look::new(
            dial,
            shep_home.path().to_owned(),
            "shep".into(),
            Sources {
                folder: paths.folder.clone(),
                agents: paths.agents.clone(),
            },
            rig.home.path().to_owned(),
        );
        look.read().unwrap();

        let bad = "productionresultssa*.blob.core.windows.net";
        let mut table = table.lock().unwrap();
        let worker = table.get_mut("worker").and_then(Value::as_object_mut);
        worker
            .unwrap()
            .insert("allowed_domains".into(), Value::Array(vec![bad.into()]));
        drop(table);
        look.again(&runner);

        assert_eq!(runner.lock().unwrap().settings(), &before);
        let failed = look.failed.unwrap();
        assert!(failed.contains(&format!("`{bad}`")), "{failed}");
    }

    // A look through a shepherd holding the rig's table with `pm_alone` as
    // `kinds`, a list the runner refuses; the table is returned to change it.
    fn pm_alone(
        rig: &Rig,
        kinds: &[&str],
    ) -> (Look, Arc<Mutex<Map<String, Value>>>, tempfile::TempDir) {
        let entry = std::fs::read_to_string(rig.settings_file()).unwrap();
        let table = Arc::new(Mutex::new(project_table(&entry)));
        set_pm_alone(&table, kinds);
        let shep_home = tempfile::tempdir().unwrap();
        let dial = shepherd(shep_home.path(), Arc::clone(&table));
        let paths = rig.paths();
        let mut look = Look::new(
            dial,
            shep_home.path().to_owned(),
            "shep".into(),
            Sources {
                folder: paths.folder.clone(),
                agents: paths.agents.clone(),
            },
            rig.home.path().to_owned(),
        );
        let agents = agents::snapshot(&paths.agents);
        look.last = Some((rig.settings(), rig.kelpie_settings(), agents));
        (look, table, shep_home)
    }

    fn set_pm_alone(table: &Mutex<Map<String, Value>>, kinds: &[&str]) {
        let mut table = table.lock().unwrap();
        let rulings = table.entry("rulings").or_insert_with(|| Map::new().into());
        let kinds = kinds.iter().map(|kind| Value::from(*kind)).collect();
        rulings["pm_alone"] = Value::Array(kinds);
    }

    #[test]
    fn a_refused_reread_alerts_once_per_reason_and_keeps_the_old_settings() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let before = runner.lock().unwrap().settings().clone();
        let (mut look, table, _home) = pm_alone(&rig, &["merge"]);

        look.again(&runner);
        look.again(&runner);
        assert_eq!(runner.lock().unwrap().settings(), &before);
        let [(_, alert)] = rig.alerts.posts().try_into().unwrap();
        assert_eq!(alert.title, "kelpie: shep settings change refused");
        for said in [
            "rulings.pm_alone",
            "a merge ruling is never decided by the project manager alone",
            "keeps its old settings until its table is fixed",
        ] {
            assert!(alert.text.contains(said), "{}", alert.text);
        }

        // Another reason is another alert.
        set_pm_alone(&table, &["security"]);
        look.again(&runner);
        look.again(&runner);
        assert_eq!(runner.lock().unwrap().settings(), &before);
        let posts = rig.alerts.posts();
        assert_eq!(posts.len(), 2);
        assert!(
            posts[1].1.text.contains("a contribution"),
            "{}",
            posts[1].1.text
        );
    }

    #[test]
    fn a_table_that_fails_to_load_is_refused_and_alerted_too() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let (mut look, table, _home) = pm_alone(&rig, &[]);
        let bad = "productionresultssa*.blob.core.windows.net";
        let mut held = table.lock().unwrap();
        let worker = held.get_mut("worker").and_then(Value::as_object_mut);
        worker
            .unwrap()
            .insert("allowed_domains".into(), Value::Array(vec![bad.into()]));
        drop(held);

        look.again(&runner);
        look.again(&runner);
        let [(_, alert)] = rig.alerts.posts().try_into().unwrap();
        assert!(alert.text.contains(bad), "{}", alert.text);
        let status = rig.ask(&runner, "status", None);
        assert!(
            status["settings_refused"]["reason"]
                .as_str()
                .unwrap()
                .contains(bad)
        );
    }

    #[test]
    fn a_refused_reread_whose_post_fails_is_posted_again_at_the_next_wake() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let (mut look, _table, _home) = pm_alone(&rig, &["merge"]);

        rig.alerts.set_down(true);
        look.again(&runner);
        rig.alerts.set_down(false);
        look.again(&runner);
        look.again(&runner);
        assert_eq!(rig.alerts.posts().len(), 2, "one failed and one landed");
    }

    #[test]
    fn a_good_reread_after_a_refused_one_clears_the_refusal_from_status() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let (mut look, table, _home) = pm_alone(&rig, &["merge"]);
        assert_eq!(
            rig.ask(&runner, "status", None).get("settings_refused"),
            None
        );

        rig.clock.advance(90);
        look.again(&runner);
        let refused = &rig.ask(&runner, "status", None)["settings_refused"];
        let reason = refused["reason"].as_str().unwrap();
        assert!(
            reason.contains("never decided by the project manager alone"),
            "{reason}"
        );
        assert_eq!(refused["at"], Rig::EPOCH + 90);

        // Still refused a minute on, and it is still since the first time.
        rig.clock.advance(60);
        look.again(&runner);
        assert_eq!(
            rig.ask(&runner, "status", None)["settings_refused"]["at"],
            Rig::EPOCH + 90
        );

        set_pm_alone(&table, &[]);
        look.again(&runner);
        assert_eq!(
            rig.ask(&runner, "status", None).get("settings_refused"),
            None
        );
    }

    #[test]
    fn a_reread_moving_git_checkout_waits_while_the_flock_cannot_be_listed() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let theirs = rig.home.path().join("theirs");
        crate::test::git(
            rig.home.path(),
            &["clone", "--quiet", rig.repo().to_str().unwrap(), "theirs"],
        );
        let entry = std::fs::read_to_string(rig.settings_file()).unwrap();
        let mut moved = project_table(&entry);
        moved["git"]["checkout"] = theirs.display().to_string().into();
        let table = Arc::new(Mutex::new(moved));
        let listed = Arc::new(AtomicBool::new(false));
        let (up, held) = (Arc::clone(&listed), Arc::clone(&table));
        let shep_home = tempfile::tempdir().unwrap();
        let (memory, _sent) =
            Memory::answering(
                shep_home.path(),
                SHEP_VERSION,
                move |request| match request {
                    Request::DogSheepSettings { .. } => Response::DogSheepSettings {
                        tables: BTreeMap::from([(
                            "shep".to_owned(),
                            DogTable::from(held.lock().unwrap().clone()),
                        )]),
                    },
                    Request::DogConfigResolved { .. } => Response::DogSection {
                        toml: SECTION.to_owned().into(),
                    },
                    Request::ListFlock if up.load(Ordering::SeqCst) => Response::Flock(Vec::new()),
                    Request::ListFlock => Response::DogSection {
                        toml: String::new().into(),
                    },
                    other => panic!("the runner asked {other:?}"),
                },
            );
        let paths = rig.paths();
        let mut look = Look::new(
            Arc::new(memory),
            shep_home.path().to_owned(),
            "shep".into(),
            Sources {
                folder: paths.folder.clone(),
                agents: paths.agents.clone(),
            },
            rig.home.path().to_owned(),
        );
        let agents = agents::snapshot(&paths.agents);
        look.last = Some((rig.settings(), rig.kelpie_settings(), agents));
        let before = look.last.clone();
        look.again(&runner);
        look.again(&runner);
        assert_eq!(look.last, before, "the change is not taken as read");
        assert!(rig.alerts.posts().is_empty(), "no refusal to tell of");
        assert_eq!(
            rig.ask(&runner, "status", None).get("settings_refused"),
            None
        );
        // Once the flock can be listed the move is checked, and goes through.
        listed.store(true, Ordering::SeqCst);
        look.again(&runner);
        assert_ne!(look.last, before);
        assert!(rig.alerts.posts().is_empty());
    }

    #[test]
    fn a_reread_moving_git_checkout_onto_another_projects_alerts_at_once() {
        let rig = Rig::new("shep");
        let runner = rig.open().unwrap();
        let before = runner.lock().unwrap().settings().clone();
        // A second checkout of the same repo, which `older` runs from and the
        // table moves this project onto.
        let theirs = rig.home.path().join("theirs");
        crate::test::git(
            rig.home.path(),
            &["clone", "--quiet", rig.repo().to_str().unwrap(), "theirs"],
        );
        let shepherd = FakeShepherd::new();
        shepherd.holds_section(SECTION);
        let launch = crate::flock::Launch {
            kelpie: "/opt/kelpie".into(),
            shep_home: shepherd.home().to_owned(),
            kelpie_home: None,
        };
        let entry = std::fs::read_to_string(rig.settings_file()).unwrap();
        let mut table = project_table(&entry);
        table["git"]["checkout"] = theirs.display().to_string().into();
        let shep = ProjectName::try_from("shep").unwrap();
        shepherd.holds(launch.runner(&shep, table.clone()), true);
        table["git"]["remote"] = "someone/else".into();
        let older = ProjectName::try_from("older").unwrap();
        shepherd.holds(launch.runner(&older, table), true);
        let paths = rig.paths();
        let mut look = Look::new(
            shepherd.dial(),
            shepherd.home().to_owned(),
            "shep".into(),
            Sources {
                folder: paths.folder.clone(),
                agents: paths.agents.clone(),
            },
            rig.home.path().to_owned(),
        );
        let agents = agents::snapshot(&paths.agents);
        look.last = Some((rig.settings(), rig.kelpie_settings(), agents));

        look.again(&runner);
        look.again(&runner);
        assert_eq!(runner.lock().unwrap().settings(), &before);
        let [(_, alert)] = rig.alerts.posts().try_into().unwrap();
        for said in [
            "git.checkout",
            "project `older` already runs this checkout",
            "keeps its old settings until its table is fixed",
        ] {
            assert!(alert.text.contains(said), "{}", alert.text);
        }
    }

    const MINE: &str = "---\nrole: implementer\nharness: claude-code\nmodel: claude-mine-1\n\
                        effort: low\n---\n";

    // A look at the rig's settings as they stand, through a shepherd that
    // holds them, read once so the next look sees only what changes after.
    fn looking(rig: &Rig) -> (Look, tempfile::TempDir) {
        let entry = std::fs::read_to_string(rig.settings_file()).unwrap();
        let table = Arc::new(Mutex::new(project_table(&entry)));
        let shep_home = tempfile::tempdir().unwrap();
        let dial = shepherd(shep_home.path(), table);
        let paths = rig.paths();
        let mut look = Look::new(
            dial,
            shep_home.path().to_owned(),
            "shep".into(),
            Sources {
                folder: paths.folder.clone(),
                agents: paths.agents.clone(),
            },
            rig.home.path().to_owned(),
        );
        look.read().unwrap();
        (look, shep_home)
    }

    // Issue 7 on the agent `mine`, its first turn due.
    fn on_mine(rig: &Rig) -> Mutex<Runner> {
        rig.write_agent("mine", MINE);
        rig.agent_board.ranks(&["sonnet-high", "mine"]);
        let runner = rig.open().unwrap();
        rig.forge.label(7, "agent:mine!");
        rig.ask(&runner, "add", Some("7"));
        runner
    }

    #[test]
    fn an_agent_file_edited_while_its_item_runs_reaches_the_next_turn() {
        let rig = Rig::new("shep");
        let runner = on_mine(&rig);
        let (mut look, _home) = looking(&rig);
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
        rig.agent_board.ranks(&["sonnet-high"]);
        let runner = rig.open().unwrap();
        assert!(matches!(
            step(&runner).unwrap(),
            Some(StepReport::Failed { .. })
        ));
        step(&runner).unwrap(); // the alert
        let (mut look, _home) = looking(&rig);

        rig.write_agent("mine", MINE);
        look.again(&runner);
        assert_eq!(look.failed, None);
        rig.ask(&runner, "rule", Some("7 yes"));
        rig.claude.script([Scripted::Say("done")]);
        step(&runner).unwrap();
        let [call] = rig.claude.calls().try_into().unwrap();
        assert_eq!(call.model, "claude-mine-1");
    }
}
