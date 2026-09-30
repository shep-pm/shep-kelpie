use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{Map, Value};
use shep_client::shep_core::protocol::request::{Request, SelectorSpec};

use super::*;
use crate::flock::Launch;
use crate::runner::ProjectName;
use crate::test::FakeShepherd;

// Real sockets under a real clock, so every run is bounded and a hang fails by name.
const PATIENCE: Duration = Duration::from_secs(30);

const FAST: Patience = Patience {
    poll: Duration::from_millis(10),
    start: Duration::from_secs(10),
    merge: Duration::from_secs(10),
};

const MERGING: &str = r#"{"work_items":[{"issue":7,"phase":{"state":"merge","head":"abc"}}]}"#;
const IDLE: &str = r#"{"work_items":[{"issue":7,"phase":{"state":"implement"}}]}"#;

/// A shepherd with the dog and a runner for `koji` running, a kelpie home,
/// and a folder of stand-in builds
struct Rig {
    shepherd: FakeShepherd,
    home: PathBuf,
    builds: PathBuf,
}

impl Rig {
    async fn new() -> Self {
        Self::on(crate::shepherd::SHEP_VERSION).await
    }

    async fn on(shepherd_version: &str) -> Self {
        let shepherd = FakeShepherd::on(shepherd_version).await;
        let home = shepherd.scratch("kelpie");
        let launch = Launch {
            kelpie: Layout::under(&home).installed(),
            shep_home: shepherd.home().to_owned(),
            kelpie_home: Some(home.clone()),
        };
        shepherd.holds(launch.dog(), true);
        shepherd.holds(launch.runner(&project("koji"), table()), true);
        let builds = shepherd.scratch("builds");
        Self {
            shepherd,
            home,
            builds,
        }
    }

    // A stand-in build: a script that answers `version --json`.
    fn build(&self, file: &str, kelpie: &str, shep: &str) -> PathBuf {
        let path = self.builds.join(file);
        let script = format!("#!/bin/sh\necho '{{\"kelpie\":\"{kelpie}\",\"shep\":\"{shep}\"}}'\n");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn installed(&self) -> PathBuf {
        Layout::under(&self.home).installed()
    }

    fn installed_says(&self) -> String {
        let build = Build::of(&self.installed()).unwrap();
        format!("{} for shep {}", build.kelpie, build.shep)
    }

    async fn upgrade(&self, action: Action) -> (Result<(), String>, Vec<String>) {
        let scene = Scene {
            kelpie_home: &self.home,
            shep_home: self.shepherd.home(),
            repo: "/nowhere",
            patience: FAST,
        };
        let mut said = Vec::new();
        let ran = tokio::time::timeout(PATIENCE, run(&scene, &action, &mut |line| said.push(line)))
            .await
            .expect("the upgrade neither ended nor failed in time");
        (ran, said)
    }

    async fn install(&self, binary: &Path) -> (Result<(), String>, Vec<String>) {
        self.upgrade(Action::Install(Source::Binary(binary.to_owned())))
            .await
    }

    /// The restarts sent since the last look, by sheep, among the triggers that came before
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

fn project(name: &str) -> ProjectName {
    ProjectName::try_from(name).unwrap()
}

fn table() -> Map<String, Value> {
    let mut table = Map::new();
    table.insert("forge".into(), Value::String("shep-pm/koji".into()));
    table
}

#[tokio::test]
async fn an_upgrade_installs_the_build_and_restarts_the_dog_then_the_runner() {
    let mut scene = Rig::new().await;
    let new = scene.build("new", "0.3.0", "0.11.0");
    let (ran, said) = scene.install(&new).await;
    ran.unwrap();
    assert_eq!(scene.installed_says(), "0.3.0 for shep 0.11.0");
    assert_eq!(scene.restarts(), ["kelpie-dog", "koji"]);
    assert!(
        said.iter().any(|l| l.contains("restarted `koji`")),
        "{said:?}"
    );
    assert!(
        !Layout::under(&scene.home).previous().exists(),
        "a first install replaced nothing"
    );
}

#[tokio::test]
async fn the_build_an_upgrade_replaces_is_kept_as_the_previous() {
    let scene = Rig::new().await;
    let (old, new) = (
        scene.build("old", "0.2.0", "0.11.0"),
        scene.build("new", "0.3.0", "0.11.0"),
    );
    scene.install(&old).await.0.unwrap();
    let (ran, said) = scene.install(&new).await;
    ran.unwrap();
    assert_eq!(scene.installed_says(), "0.3.0 for shep 0.11.0");
    let previous = Build::of(&Layout::under(&scene.home).previous()).unwrap();
    assert_eq!(previous.kelpie, "0.2.0");
    assert!(said[0].contains("kelpie 0.2.0"), "{said:?}");
    assert!(said[0].contains("--rollback"), "{said:?}");
}

#[tokio::test]
async fn an_upgrade_waits_out_a_merge_in_flight_before_any_restart() {
    let mut scene = Rig::new().await;
    // Two looks at a merge under way, then it is over.
    scene.shepherd.says("koji", &[MERGING, MERGING, IDLE]);
    let new = scene.build("new", "0.3.0", "0.11.0");
    let (ran, said) = scene.install(&new).await;
    ran.unwrap();

    let writes = scene.shepherd.writes();
    let first_restart = writes
        .iter()
        .position(|w| matches!(w, Request::Restart { .. }))
        .expect("a restart");
    let looks = writes[..first_restart]
        .iter()
        .filter(|w| matches!(w, Request::Trigger { action, .. } if action == "status"))
        .count();
    assert_eq!(
        looks, 3,
        "the runner was looked at until its merge ended: {writes:?}"
    );
    assert!(
        said.iter().any(|l| l == "waiting: `koji` is merging #7"),
        "{said:?}"
    );
    assert!(
        said.iter().any(|l| l.contains("restarted `koji`")),
        "{said:?}"
    );
}

#[tokio::test]
async fn the_dog_goes_down_only_when_no_runner_is_merging() {
    let mut scene = Rig::new().await;
    let launch = Launch {
        kelpie: scene.installed(),
        shep_home: scene.shepherd.home().to_owned(),
        kelpie_home: Some(scene.home.clone()),
    };
    scene
        .shepherd
        .holds(launch.runner(&project("aria"), table()), true);
    // `koji` is free and `aria` merging for a while.
    scene.shepherd.says("aria", &[MERGING, MERGING, IDLE]);
    let new = scene.build("new", "0.3.0", "0.11.0");
    scene.install(&new).await.0.unwrap();

    let writes = scene.shepherd.writes();
    let restarted: Vec<_> = writes
        .iter()
        .filter_map(|w| match w {
            Request::Restart {
                selector: SelectorSpec::Name(name),
            } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(restarted, ["kelpie-dog", "aria", "koji"]);
    let dog = writes
        .iter()
        .position(|w| matches!(w, Request::Restart { .. }))
        .unwrap();
    let looks_at_aria = writes[..dog]
        .iter()
        .filter(|w| matches!(w, Request::Trigger { selector: SelectorSpec::Name(n), .. } if n == "aria"))
        .count();
    assert_eq!(looks_at_aria, 3, "{writes:?}");
}

#[tokio::test]
async fn an_upgrade_gives_up_on_a_merge_that_does_not_end_and_restarts_nothing() {
    let mut scene = Rig::new().await;
    scene.shepherd.says("koji", &[MERGING]);
    let new = scene.build("new", "0.3.0", "0.11.0");
    let patience = Patience {
        merge: Duration::from_millis(100),
        ..FAST
    };
    let scene_ = Scene {
        kelpie_home: &scene.home,
        shep_home: scene.shepherd.home(),
        repo: "/nowhere",
        patience,
    };
    let action = Action::Install(Source::Binary(new));
    let err = tokio::time::timeout(PATIENCE, run(&scene_, &action, &mut |_| {}))
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.contains("`koji` is merging #7"), "{err}");
    assert!(err.contains("nothing was restarted"), "{err}");
    assert_eq!(scene.restarts(), Vec::<String>::new());
}

#[tokio::test]
async fn a_build_for_another_shep_minor_stops_before_anything_changes() {
    let mut scene = Rig::new().await;
    let old = scene.build("old", "0.2.0", "0.11.0");
    scene.install(&old).await.0.unwrap();
    scene.restarts();
    let new = scene.build("new", "0.4.0", "0.12.0");

    let (ran, said) = scene.install(&new).await;
    let err = ran.unwrap_err();
    assert!(err.contains("shep 0.12.0"), "{err}");
    assert!(err.contains("runs shep 0.11.0"), "{err}");
    assert!(err.contains("Nothing was installed or restarted"), "{err}");
    assert!(
        err.contains("muster"),
        "a newer build says to move the shepherd first: {err}"
    );
    assert_eq!(said, Vec::<String>::new());
    assert_eq!(scene.restarts(), Vec::<String>::new());
    assert_eq!(scene.installed_says(), "0.2.0 for shep 0.11.0");
    let bin: Vec<_> = std::fs::read_dir(scene.home.join("bin"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(bin, ["shep-kelpie"], "no staged copy is left behind");
}

#[tokio::test]
async fn a_build_for_an_older_minor_than_the_shepherd_says_to_install_another_kelpie() {
    let scene = Rig::on("0.12.1").await;
    let new = scene.build("new", "0.3.0", "0.11.0");
    let err = scene.install(&new).await.0.unwrap_err();
    assert!(err.contains("install a kelpie built for"), "{err}");
    assert!(!scene.installed().exists());
}

#[tokio::test]
async fn a_newer_patch_of_the_same_minor_is_taken() {
    let scene = Rig::on("0.11.3").await;
    let new = scene.build("new", "0.3.0", "0.11.0");
    scene.install(&new).await.0.unwrap();
    assert_eq!(scene.installed_says(), "0.3.0 for shep 0.11.0");
}

#[tokio::test]
async fn a_rollback_restores_the_previous_build_and_restarts_onto_it() {
    let mut scene = Rig::new().await;
    let (old, new) = (
        scene.build("old", "0.2.0", "0.11.0"),
        scene.build("new", "0.3.0", "0.11.0"),
    );
    scene.install(&old).await.0.unwrap();
    scene.install(&new).await.0.unwrap();
    scene.restarts();

    let (ran, said) = scene.upgrade(Action::Rollback).await;
    ran.unwrap();
    assert_eq!(scene.installed_says(), "0.2.0 for shep 0.11.0");
    assert_eq!(scene.restarts(), ["kelpie-dog", "koji"]);
    assert!(said[0].contains("kelpie 0.2.0"), "{said:?}");

    // A second rollback puts the upgrade back.
    scene.upgrade(Action::Rollback).await.0.unwrap();
    assert_eq!(scene.installed_says(), "0.3.0 for shep 0.11.0");
}

#[tokio::test]
async fn a_rollback_with_no_previous_build_changes_nothing() {
    let mut scene = Rig::new().await;
    let only = scene.build("only", "0.2.0", "0.11.0");
    scene.install(&only).await.0.unwrap();
    scene.restarts();
    let err = scene.upgrade(Action::Rollback).await.0.unwrap_err();
    assert!(err.contains("no previous build"), "{err}");
    assert_eq!(scene.restarts(), Vec::<String>::new());
    assert_eq!(scene.installed_says(), "0.2.0 for shep 0.11.0");
}

#[tokio::test]
async fn a_rollback_to_a_build_the_shepherd_would_refuse_stops_there() {
    // The shepherd moved to 0.12 after the upgrade to a 0.12 build.
    let mut scene = Rig::on("0.12.0").await;
    let (old, new) = (
        scene.build("old", "0.2.0", "0.11.0"),
        scene.build("new", "0.4.0", "0.12.0"),
    );
    let layout = Layout::under(&scene.home);
    std::fs::create_dir_all(scene.home.join("bin")).unwrap();
    std::fs::copy(&old, layout.previous()).unwrap();
    std::fs::copy(&new, layout.installed()).unwrap();

    let err = scene.upgrade(Action::Rollback).await.0.unwrap_err();
    assert!(err.contains("shep 0.11.0"), "{err}");
    assert!(err.contains("runs shep 0.12.0"), "{err}");
    assert_eq!(scene.installed_says(), "0.4.0 for shep 0.12.0");
    assert_eq!(scene.restarts(), Vec::<String>::new());
}

#[tokio::test]
async fn running_the_same_build_again_keeps_the_real_previous_build() {
    let scene = Rig::new().await;
    let (old, new) = (
        scene.build("old", "0.2.0", "0.11.0"),
        scene.build("new", "0.3.0", "0.11.0"),
    );
    scene.install(&old).await.0.unwrap();
    scene.install(&new).await.0.unwrap();
    let (ran, said) = scene.install(&new).await;
    ran.unwrap();
    assert!(said[0].contains("installed already"), "{said:?}");
    let previous = Build::of(&Layout::under(&scene.home).previous()).unwrap();
    assert_eq!(previous.kelpie, "0.2.0");
}

#[tokio::test]
async fn a_binary_that_does_not_say_its_versions_is_refused_and_not_installed() {
    let scene = Rig::new().await;
    let liar = scene.builds.join("liar");
    std::fs::write(&liar, "#!/bin/sh\necho hello\n").unwrap();
    std::fs::set_permissions(&liar, std::fs::Permissions::from_mode(0o755)).unwrap();
    let err = scene.install(&liar).await.0.unwrap_err();
    assert!(err.contains("did not answer `version --json`"), "{err}");
    assert!(!scene.installed().exists());

    let err = scene
        .install(&scene.builds.join("missing"))
        .await
        .0
        .unwrap_err();
    assert!(err.contains("is not a file"), "{err}");
}

#[tokio::test]
async fn a_sheep_that_is_stopped_stays_stopped() {
    let mut scene = Rig::new().await;
    let launch = Launch {
        kelpie: scene.installed(),
        shep_home: scene.shepherd.home().to_owned(),
        kelpie_home: Some(scene.home.clone()),
    };
    scene
        .shepherd
        .holds(launch.runner(&project("paused"), table()), false);
    let new = scene.build("new", "0.3.0", "0.11.0");
    let (ran, said) = scene.install(&new).await;
    ran.unwrap();
    assert_eq!(scene.restarts(), ["kelpie-dog", "koji"]);
    assert!(!scene.shepherd.sheep("paused").unwrap().1);
    assert!(
        said.iter().any(|l| l.contains("`paused` is not running")),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_sheep_that_runs_another_binary_is_restarted_and_named() {
    let scene = Rig::new().await;
    scene.shepherd.holds(
        Launch {
            kelpie: "/opt/kelpie/bin/kelpie".into(),
            shep_home: scene.shepherd.home().to_owned(),
            kelpie_home: None,
        }
        .runner(&project("old"), table()),
        true,
    );
    let new = scene.build("new", "0.3.0", "0.11.0");
    let (ran, said) = scene.install(&new).await;
    ran.unwrap();
    assert!(
        said.iter()
            .any(|l| l.starts_with("`old` runs /opt/kelpie/bin/kelpie")),
        "{said:?}"
    );
}

#[tokio::test]
async fn no_shepherd_means_no_upgrade() {
    let home = tempfile::tempdir().unwrap();
    let scene = Scene {
        kelpie_home: home.path(),
        shep_home: Path::new("/nowhere/shep"),
        repo: "/nowhere",
        patience: FAST,
    };
    let err = run(&scene, &Action::Rollback, &mut |_| {})
        .await
        .unwrap_err();
    assert!(err.contains("cannot reach kelpie's shepherd"), "{err}");
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

#[test]
fn a_ref_is_built_where_it_stands_in_the_repo() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let write = |file: &str, text: &str| std::fs::write(repo.join(file), text).unwrap();
    write(
        "Cargo.toml",
        "[package]\nname = \"shep-kelpie\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = \"shep-kelpie\"\nversion = \"0.1.0\"\n",
    );
    let main = |version: &str| {
        format!(
            "fn main() {{ println!(\"{{{{\\\"kelpie\\\":\\\"{version}\\\",\\\"shep\\\":\\\"0.11.0\\\"}}}}\"); }}\n"
        )
    };
    let git = |args: &[&str]| {
        let ran = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(ran.status.success(), "git {args:?}: {ran:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    write("src/main.rs", &main("0.1.0"));
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "first"]);
    git(&["tag", "v0.1.0"]);
    write("src/main.rs", &main("0.2.0"));
    git(&["commit", "-q", "-am", "second"]);

    let work = dir.path().join("work");
    let repo = repo.to_str().unwrap();
    for (reference, version) in [("v0.1.0", "0.1.0"), ("main", "0.2.0")] {
        let binary = fetch::build_ref(&work, repo, reference).unwrap();
        assert_eq!(Build::of(&binary).unwrap().kelpie, version, "{reference}");
    }
    let err = fetch::build_ref(&work, repo, "nope").unwrap_err();
    assert!(err.contains("has no ref nope"), "{err}");
}
