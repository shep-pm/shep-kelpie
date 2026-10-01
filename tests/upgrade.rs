//! `shep-kelpie upgrade`, against the real binary, a stand-in shepherd and
//! stand-in builds: each exits 0 when it finished and non-zero with a
//! message on stderr when it did not.
//!
//! Kelpie's home, the shepherd and the installed kelpie are all folders under
//! one scratch root, so nothing here touches a real install.

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shep_client::shep_core::config::{AppConfig, DogTable};
use shep_client::shep_core::protocol::request::{
    ActionOutcome, ActionReply, DogSource, ProcessInfo, SheepConfigView,
};
use shep_client::shep_core::protocol::{Envelope, Request, Response, SelectorSpec};
use shep_client::shep_core::status::ProcStatus;
use shep_client::testing::{fake_daemon_answering_with_ack, sample_ack};
use tempfile::TempDir;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedReceiver;

// The repo's own ETXTBSY-safe way to write a stand-in script.
#[path = "../src/test/script.rs"]
mod script;

const KELPIE: &str = env!("CARGO_BIN_EXE_shep-kelpie");

// Bounds each run of the binary, so a hang fails by name.
const PATIENCE: Duration = Duration::from_secs(30);

const MERGING: &str = r#"{"work_items":[{"issue":7,"phase":{"state":"merge","head":"abc"}}]}"#;
const IDLE: &str = r#"{"work_items":[]}"#;

struct Scene {
    root: TempDir,
    // What `koji`'s runner answers `status` with, in turn, the last repeating
    koji_says: Arc<Mutex<Vec<&'static str>>>,
}

impl Scene {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for folder in ["shep/run", "builds", "bin"] {
            std::fs::create_dir_all(root.path().join(folder)).unwrap();
        }
        let scene = Self {
            root,
            koji_says: Arc::new(Mutex::new(vec![IDLE])),
        };
        script::write_script(&scene.installed(), &answering("0.1.0", "0.12.0"));
        scene
    }

    fn shep_home(&self) -> PathBuf {
        self.root.path().join("shep")
    }

    fn kelpie_home(&self) -> PathBuf {
        self.root.path().join("kelpie")
    }

    // The program the dog and `koji` run, where `~/.cargo/bin/shep-kelpie` would be.
    fn installed(&self) -> PathBuf {
        self.root.path().join("bin/shep-kelpie")
    }

    // A shepherd running `version`, whose flock is the adopted dog and one
    // runner that runs `koji_runs`.
    async fn shepherd(&self, version: &str, koji_runs: &Path) -> UnboundedReceiver<Envelope> {
        let mut ack = sample_ack();
        ack.daemon_version = version.into();
        let mut dog = AppConfig::minimal("kelpie", &self.installed().display().to_string());
        dog.args = Vec::new();
        let mut koji = AppConfig::minimal("koji", &koji_runs.display().to_string());
        koji.args = vec!["runner".into(), "koji".into()];
        koji.dogs
            .insert("kelpie".into(), DogTable::from(serde_json::Map::new()));
        let flock = [dog, koji];
        let says = Arc::clone(&self.koji_says);
        fake_daemon_answering_with_ack(&self.shep_home().join("run/shep.sock"), ack, move |r| {
            answer(&flock, &says, r)
        })
        .await
    }

    // A stand-in build: a script that answers `version --json`.
    fn build(&self, file: &str, kelpie: &str, shep: &str) -> PathBuf {
        let path = self.root.path().join("builds").join(file);
        script::write_script(&path, &answering(kelpie, shep));
        path
    }

    async fn kelpie(&self, args: &[&str]) -> Output {
        let run = Command::new(KELPIE)
            .args(args)
            .env_clear()
            .env("HOME", self.root.path())
            .env("KELPIE_HOME", self.kelpie_home())
            .env("SHEP_HOME", self.shep_home())
            .stdin(Stdio::null())
            .output();
        tokio::time::timeout(PATIENCE, run)
            .await
            .expect("kelpie did not exit in time")
            .unwrap()
    }

    async fn upgrade_to(&self, build: &Path) -> Output {
        self.kelpie(&["upgrade", "--binary", build.to_str().unwrap()])
            .await
    }

    // What the installed kelpie says it is.
    async fn installed_says(&self) -> String {
        let out = Command::new(self.installed())
            .args(["version", "--json"])
            .output()
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        format!("{} for shep {}", json["kelpie"], json["shep"]).replace('"', "")
    }
}

fn answering(kelpie: &str, shep: &str) -> String {
    let json = format!(r#"{{"kelpie":"{kelpie}","shep":"{shep}"}}"#);
    format!("#!/bin/sh\necho '{json}'\n")
}

fn row(id: u32, config: &AppConfig) -> ProcessInfo {
    let info = ProcessInfo::builder(id, &config.name, ProcStatus::Online);
    match config.name.as_str() {
        "kelpie" => info.dog(Some(DogSource::Adopted {
            path: config.script.clone(),
            channel: true,
        })),
        _ => info,
    }
    .build()
}

fn answer(flock: &[AppConfig], says: &Mutex<Vec<&'static str>>, request: &Request) -> Response {
    let named = |selector: &SelectorSpec| {
        let SelectorSpec::Name(name) = selector else {
            panic!("kelpie named no sheep: {selector:?}");
        };
        flock.iter().position(|s| &s.name == name)
    };
    match request {
        Request::ListFlock => Response::Flock(
            flock
                .iter()
                .enumerate()
                .map(|(i, s)| row(u32::try_from(i).unwrap(), s))
                .collect(),
        ),
        Request::DogSheepSettings { dog } => Response::DogSheepSettings {
            tables: flock
                .iter()
                .filter_map(|s| Some((s.name.clone(), s.dogs.get(dog)?.clone())))
                .collect(),
        },
        Request::SheepConfig { name } => {
            let sheep = flock.iter().find(|s| &s.name == name).unwrap();
            Response::SheepConfig(Box::new(SheepConfigView::new(
                sheep.clone(),
                Vec::new(),
                Vec::new(),
            )))
        }
        Request::Restart { selector } => {
            let at = named(selector).unwrap();
            Response::Restarted {
                accepted: vec![row(u32::try_from(at).unwrap(), &flock[at])],
                refused: Vec::new(),
            }
        }
        Request::Trigger { selector, .. } => {
            let at = named(selector).unwrap();
            let body = match flock[at].name.as_str() {
                "koji" => {
                    let mut says = says.lock().unwrap();
                    match says.len() {
                        1 => says[0],
                        _ => says.remove(0),
                    }
                    .to_owned()
                }
                _ => r#"{"leases":[]}"#.to_owned(),
            };
            Response::Triggered(vec![ActionReply {
                id: u32::try_from(at).unwrap(),
                name: flock[at].name.clone(),
                outcome: ActionOutcome::Replied { body },
            }])
        }
        other => panic!("kelpie asked {other:?}"),
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// Every request since the last look, in the order kelpie sent them.
fn sent(sent: &mut UnboundedReceiver<Envelope>) -> Vec<Request> {
    std::iter::from_fn(|| sent.try_recv().ok())
        .map(|e| e.body)
        .collect()
}

fn restarts(requests: &[Request]) -> Vec<&str> {
    requests
        .iter()
        .filter_map(|r| match r {
            Request::Restart {
                selector: SelectorSpec::Name(name),
            } => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_build_answers_version_json_with_its_kelpie_and_its_shep() {
    let scene = Scene::new();
    let output = scene.kelpie(&["version", "--json"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["kelpie"], env!("CARGO_PKG_VERSION"));
    assert_eq!(json["shep"], shep_kelpie::shepherd::SHEP_VERSION);
}

// Real sockets and real child processes, so a real clock.
#[tokio::test]
async fn an_upgrade_installs_and_restarts_and_a_rollback_puts_the_build_back() {
    let scene = Scene::new();
    let mut seen = scene.shepherd("0.12.0", &scene.installed()).await;
    let (middle, new) = (
        scene.build("middle", "0.2.0", "0.12.0"),
        scene.build("new", "0.3.0", "0.12.0"),
    );

    let output = scene.upgrade_to(&middle).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let output = scene.upgrade_to(&new).await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(scene.installed_says().await, "0.3.0 for shep 0.12.0");
    assert_eq!(
        restarts(&sent(&mut seen)),
        ["kelpie", "koji", "kelpie", "koji"]
    );

    let output = scene.kelpie(&["upgrade", "--rollback"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(scene.installed_says().await, "0.2.0 for shep 0.12.0");
    assert_eq!(restarts(&sent(&mut seen)), ["kelpie", "koji"]);
    assert!(
        scene.kelpie_home().join("builds").is_dir(),
        "the replaced build is kept in kelpie's home"
    );
}

#[tokio::test]
async fn an_upgrade_waits_out_a_merge_and_asks_before_it_restarts() {
    let scene = Scene::new();
    let mut seen = scene.shepherd("0.12.0", &scene.installed()).await;
    *scene.koji_says.lock().unwrap() = vec![MERGING, MERGING, IDLE];
    let new = scene.build("new", "0.3.0", "0.12.0");
    let output = scene.upgrade_to(&new).await;
    assert!(output.status.success(), "{}", stderr(&output));
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(said.contains("waiting: `koji` is merging #7"), "{said}");

    let requests = sent(&mut seen);
    assert_eq!(restarts(&requests), ["kelpie", "koji"]);
    let first_restart = requests
        .iter()
        .position(|r| matches!(r, Request::Restart { .. }))
        .unwrap();
    let looked_at_koji = |r: &&Request| {
        matches!(r, Request::Trigger { selector: SelectorSpec::Name(n), action, .. }
            if n == "koji" && action == "status")
    };
    assert_eq!(
        requests[..first_restart]
            .iter()
            .filter(looked_at_koji)
            .count(),
        3,
        "`status` was asked until the merge ended, and before the first restart: {requests:?}"
    );
}

#[tokio::test]
async fn a_minor_mismatch_fails_with_the_steps_before_anything_restarts() {
    let scene = Scene::new();
    let mut seen = scene.shepherd("0.12.0", &scene.installed()).await;
    let new = scene.build("new", "0.4.0", "0.13.0");
    let output = scene.upgrade_to(&new).await;
    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("shep 0.13.0"), "{stderr}");
    assert!(stderr.contains("shep 0.12.0"), "{stderr}");
    assert!(stderr.contains("To go on:"), "{stderr}");
    assert!(stderr.contains("reload its shepherd"), "{stderr}");
    assert!(stderr.contains("the new build's own upgrade"), "{stderr}");
    assert_eq!(restarts(&sent(&mut seen)), Vec::<&str>::new());
    assert_eq!(scene.installed_says().await, "0.1.0 for shep 0.12.0");
}

#[tokio::test]
async fn a_sheep_on_another_program_is_named_before_anything_changes() {
    let scene = Scene::new();
    let mut seen = scene
        .shepherd("0.12.0", Path::new("/opt/kelpie/bin/kelpie"))
        .await;
    let new = scene.build("new", "0.3.0", "0.12.0");
    let output = scene.upgrade_to(&new).await;
    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(
        stderr.contains("`koji` runs /opt/kelpie/bin/kelpie"),
        "{stderr}"
    );
    assert_eq!(restarts(&sent(&mut seen)), Vec::<&str>::new());
    assert_eq!(scene.installed_says().await, "0.1.0 for shep 0.12.0");
}

#[tokio::test]
async fn upgrade_without_a_form_prints_usage_and_exits_2() {
    let scene = Scene::new();
    for args in [
        &["upgrade"][..],
        &["upgrade", "--ref"],
        &["upgrade", "--both", "x"],
    ] {
        let output = scene.kelpie(args).await;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(stderr(&output).contains("--rollback"), "{args:?}");
    }
}
