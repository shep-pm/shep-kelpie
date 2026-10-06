//! `upgrade` against a fake shepherd and stand-in builds, in a scratch home
//!
//! Each test points kelpie's home and the shepherd at scratch folders, and
//! the installed kelpie at a stand-in under them: nothing here touches a real
//! install. `files` is the files an upgrade moves, `sheep` the sheep it
//! restarts, and `refusals` what it stops before doing either.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};
use shep_client::shep_core::protocol::request::{Request, SelectorSpec};

use super::*;
use crate::flock::Launch;
use crate::runner::ProjectName;
use crate::test::{FakeShepherd, write_script};

mod drain;
mod files;
mod refusals;
mod sheep;
mod source;

// Real sockets under a real clock, so every run is bounded and a hang fails by name.
const PATIENCE: Duration = Duration::from_secs(30);

const FAST: Patience = Patience {
    poll: Duration::from_millis(10),
    start: Duration::from_secs(10),
    merge: Duration::from_secs(10),
    margin: Duration::from_secs(10),
};

const MERGING: &str = r#"{"work_items":[{"issue":7,"phase":{"state":"merge","head":"abc"}}]}"#;
const IDLE: &str = r#"{"work_items":[{"issue":7,"phase":{"state":"implement"}}]}"#;

/// A shepherd whose adopted dog and a runner for `koji` run a stand-in
/// kelpie, a kelpie home, and a folder of stand-in builds
struct Rig {
    shepherd: FakeShepherd,
    home: PathBuf,
    builds: PathBuf,
    // The path the dog and the runners run
    installed: PathBuf,
}

impl Rig {
    async fn new() -> Self {
        Self::on(crate::shepherd::SHEP_VERSION, false).await
    }

    // `linked`: the installed path is a symlink to the file that holds the build.
    async fn on(shepherd_version: &str, linked: bool) -> Self {
        let shepherd = FakeShepherd::on(shepherd_version).await;
        let home = shepherd.scratch("kelpie");
        let bin = shepherd.scratch("bin");
        let builds = shepherd.scratch("builds");
        let installed = bin.join("shep-kelpie");
        if linked {
            let real = bin.join("real-kelpie");
            write_script(&real, &answering("0.1.0", "0.12.0"));
            std::os::unix::fs::symlink(&real, &installed).unwrap();
        } else {
            write_script(&installed, &answering("0.1.0", "0.12.0"));
        }
        shepherd.holds_dog_at("kelpie", &installed.display().to_string(), true);
        let rig = Self {
            shepherd,
            home,
            builds,
            installed,
        };
        rig.runs("koji", &rig.installed, true);
        rig
    }

    // A runner for `name` that runs `program`.
    fn runs(&self, name: &str, program: &Path, online: bool) {
        let launch = Launch {
            kelpie: program.to_owned(),
            shep_home: self.shepherd.home().to_owned(),
            kelpie_home: Some(self.home.clone()),
        };
        let runner = launch.runner(&ProjectName::try_from(name).unwrap(), table());
        self.shepherd.holds(runner, online);
    }

    // A stand-in build: a script that answers `version --json`.
    fn build(&self, file: &str, kelpie: &str, shep: &str) -> PathBuf {
        let path = self.builds.join(file);
        write_script(&path, &answering(kelpie, shep));
        path
    }

    fn previous(&self) -> PathBuf {
        Layout::new(&self.home, &self.installed).previous()
    }

    fn installed_says(&self) -> String {
        says(&self.installed)
    }

    fn scene(&self, patience: Patience) -> Scene<'_> {
        Scene {
            kelpie_home: &self.home,
            shep_home: self.shepherd.home(),
            repo: "/nowhere",
            patience,
            now: false,
            interrupt: Interrupt::Never,
        }
    }

    async fn upgrade_within(
        &self,
        action: Action,
        patience: Patience,
    ) -> (Result<(), String>, Vec<String>) {
        self.upgrade_in(self.scene(patience), action).await
    }

    async fn upgrade_in(
        &self,
        scene: Scene<'_>,
        action: Action,
    ) -> (Result<(), String>, Vec<String>) {
        let mut said = Vec::new();
        let ran = tokio::time::timeout(PATIENCE, run(&scene, &action, &mut |line| said.push(line)))
            .await
            .expect("the upgrade neither ended nor failed in time");
        (ran, said)
    }

    async fn upgrade(&self, action: Action) -> (Result<(), String>, Vec<String>) {
        self.upgrade_within(action, FAST).await
    }

    async fn install(&self, binary: &Path) -> (Result<(), String>, Vec<String>) {
        self.upgrade(Action::Install(Source::Binary(binary.to_owned())))
            .await
    }

    /// The restarts sent since the last look, by sheep
    fn restarts(&mut self) -> Vec<String> {
        self.shepherd
            .writes()
            .into_iter()
            .filter_map(|w| match w {
                Request::Restart {
                    selector: SelectorSpec::Name(name),
                } => Some(name),
                _ => None,
            })
            .collect()
    }
}

// A stand-in build that answers `version --json`.
fn answering(kelpie: &str, shep: &str) -> String {
    let json = format!(r#"{{"kelpie":"{kelpie}","shep":"{shep}"}}"#);
    format!("#!/bin/sh\necho '{json}'\n")
}

// What the build at `path` says it is.
fn says(path: &Path) -> String {
    let build = Build::of(path).unwrap();
    format!("{} for shep {}", build.kelpie, build.shep)
}

fn table() -> Map<String, Value> {
    let mut table = Map::new();
    table.insert("forge".into(), Value::String("shep-pm/koji".into()));
    table
}

#[test]
fn the_four_forms_are_read_and_nothing_else() {
    let args = |a: &[&str]| parse(&a.iter().map(|&s| s.to_owned()).collect::<Vec<_>>());
    assert_eq!(
        args(&["--ref", "main"]),
        Ok(Action::Install(Source::Ref("main".into())))
    );
    assert_eq!(
        args(&["--release", "0.3.0"]),
        Ok(Action::Install(Source::Release("0.3.0".into())))
    );
    assert_eq!(
        args(&["--binary", "/tmp/k"]),
        Ok(Action::Install(Source::Binary("/tmp/k".into())))
    );
    assert_eq!(args(&["--rollback"]), Ok(Action::Rollback));
    for bad in [
        &[][..],
        &["--ref"],
        &["--rollback", "x"],
        &["--ref", "a", "b"],
        &["main"],
    ] {
        assert_eq!(args(bad), Err(USAGE.to_owned()), "{bad:?}");
    }
}

#[test]
fn a_merge_in_flight_is_a_work_item_being_merged_or_just_merged() {
    let phase = |phase: &str| format!(r#"{{"work_items":[{{"issue":4,"phase":{phase}}}]}}"#);
    for (phase_json, merging_now) in [
        (r#"{"state":"merge","head":"a"}"#, true),
        (r#"{"state":"done","merged":true}"#, true),
        (r#"{"state":"done","merged":false}"#, false),
        (r#"{"state":"ruling","id":1}"#, false),
        (r#"{"state":"implement"}"#, false),
        (r#""merge""#, true),
    ] {
        let expected: Vec<u64> = if merging_now { vec![4] } else { vec![] };
        assert_eq!(
            restart::merging(&phase(phase_json)),
            expected,
            "{phase_json}"
        );
    }
    // A status from before `work_items`, and ones that say nothing.
    assert_eq!(
        restart::merging(r#"{"work_item":{"issue":9,"phase":{"state":"merge","head":"a"}}}"#),
        [9]
    );
    for status in [r#"{"work_item":null}"#, "unknown action: status", "", "[]"] {
        assert_eq!(restart::merging(status), Vec::<u64>::new(), "{status}");
    }
}
