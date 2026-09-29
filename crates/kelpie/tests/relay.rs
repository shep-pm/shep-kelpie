//! The relay's commands, against the real binary and a fake shepherd, with
//! a decoy `shep` first on `PATH`: a ruling reaches the shepherd at
//! `SHEP_HOME` and the decoy never runs.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

use shep_client::shep_core::protocol::request::{ActionOutcome, ActionReply, SelectorSpec};
use shep_client::shep_core::protocol::{Envelope, Request, Response};
use shep_client::testing::{fake_daemon_answering_with_ack, sample_ack};
use tempfile::TempDir;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedReceiver;

const KELPIE: &str = env!("CARGO_BIN_EXE_kelpie");

// Bounds each run of the binary, so a hang fails by name.
const PATIENCE: Duration = Duration::from_secs(20);

struct Scene {
    root: TempDir,
}

impl Scene {
    // A shepherd home, and a `bin` folder holding a `shep` that leaves a
    // mark if anything runs it.
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("shep/run")).unwrap();
        std::fs::create_dir(root.path().join("bin")).unwrap();
        let decoy = root.path().join("bin/shep");
        let mark = root.path().join("decoy-ran");
        std::fs::write(&decoy, format!("#!/bin/sh\ntouch '{}'\n", mark.display())).unwrap();
        std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { root }
    }

    fn shep_home(&self) -> std::path::PathBuf {
        self.root.path().join("shep")
    }

    async fn shepherd(&self, version: &str) -> UnboundedReceiver<Envelope> {
        let mut ack = sample_ack();
        ack.daemon_version = version.into();
        fake_daemon_answering_with_ack(&self.shep_home().join("run/shep.sock"), ack, |_| {
            Response::Triggered(vec![ActionReply {
                id: 1,
                name: "shep".into(),
                outcome: ActionOutcome::Replied {
                    body: STATUS.into(),
                },
            }])
        })
        .await
    }

    async fn kelpie(&self, args: &[&str]) -> Output {
        let run = Command::new(KELPIE)
            .args(args)
            .env_clear()
            .env("HOME", self.root.path())
            .env("PATH", path_with(&self.root.path().join("bin")))
            .env("SHEP_HOME", self.shep_home())
            .stdin(Stdio::null())
            .output();
        tokio::time::timeout(PATIENCE, run)
            .await
            .expect("kelpie did not exit in time")
            .unwrap()
    }

    fn decoy_ran(&self) -> bool {
        self.root.path().join("decoy-ran").exists()
    }
}

fn path_with(first: &Path) -> String {
    format!("{}:/usr/bin:/bin", first.display())
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// What a runner replies to a ruling it took: its status, as JSON.
const STATUS: &str = r#"{"project":"shep","rulings":[]}"#;

fn rule_trigger(params: &str) -> Request {
    Request::Trigger {
        selector: SelectorSpec::Name("shep".into()),
        action: kelpie::runner::RELAY_RULE.into(),
        params: Some(params.into()),
    }
}

// Real sockets and a real child process, so a real clock.
#[tokio::test]
async fn relay_yes_reaches_the_shepherd_at_shep_home_not_the_shep_on_path() {
    let scene = Scene::new();
    let mut sent = scene.shepherd(kelpie::relay::rule::SHEP_VERSION).await;
    let output = scene.kelpie(&["relay-yes", "shep", "3"]).await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{STATUS}\n")
    );
    assert_eq!(sent.try_recv().unwrap().body, rule_trigger("3 yes"));
    assert!(!scene.decoy_ran(), "the shep on PATH ran");
}

#[tokio::test]
async fn relay_answer_reaches_the_shepherd_at_shep_home_not_the_shep_on_path() {
    let scene = Scene::new();
    let mut sent = scene.shepherd(kelpie::relay::rule::SHEP_VERSION).await;
    let output = scene
        .kelpie(&["relay-answer", "shep", "3 no rename the flag"])
        .await;
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        sent.try_recv().unwrap().body,
        rule_trigger("3 no rename the flag")
    );
    assert!(!scene.decoy_ran(), "the shep on PATH ran");
}

#[tokio::test]
async fn a_shepherd_on_an_older_shep_is_refused_with_the_reason() {
    let scene = Scene::new();
    let mut sent = scene.shepherd("0.8.2").await;
    let output = scene.kelpie(&["relay-yes", "shep", "3"]).await;
    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("runs shep 0.8.2"), "{stderr}");
    assert!(stderr.contains("the ruling was not sent"), "{stderr}");
    assert!(!stderr.contains("reload"), "{stderr}");
    assert!(sent.try_recv().is_err(), "the ruling was sent");
    assert!(!scene.decoy_ran(), "the shep on PATH ran");
}
