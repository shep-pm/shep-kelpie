//! A handover through a real `execve`
//!
//! The test binary runs itself: the driver starts it on [`IMAGE`], which is
//! the outgoing image until it execs the binary again, and then the incoming
//! one, and the two tell what they saw through files in a folder the driver
//! made. Both begin with `boot`, which also answers the handover's probe.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::super::super::{Hooks, Worker};
use super::*;
use crate::adapters::{Base, ClaudeCli, LoginToken};
use crate::test::{CALL_PATIENCE, OpenSandbox, waits_for, write_script};

const IMAGE: &str = "sheep::handover::tests::exec::an_image_of_the_runner";

// Names the folder the images share, and so that one is the driver's to run.
pub(super) const FOLDER: &str = "KELPIE_HANDOVER_TEST_FOLDER";

const RESULT: &str = include_str!("../../../../fixtures/claude-p-result.json");

#[test]
fn a_handover_with_a_worker_turn_running_ends_the_turn_on_the_new_build() {
    let folder = crate::test::short_tempdir();
    let ran = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            IMAGE,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(FOLDER, folder.path())
        .output()
        .unwrap();
    let said = format!(
        "{}\n{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    let home = read(folder.path(), "home");
    let _ = std::fs::remove_dir_all(home.trim());

    assert!(ran.status.success(), "the images failed:\n{said}");
    let (before, after) = (
        read(folder.path(), "pid.before"),
        read(folder.path(), "pid.after"),
    );
    assert!(!before.is_empty(), "the first image never ran");
    assert_eq!(before, after, "the runner kept its pid");
    assert_eq!(read(folder.path(), "turn"), "ended");
    let ledger: serde_json::Value = serde_json::from_str(&read(folder.path(), "ledger")).unwrap();
    assert_eq!(ledger["cost_usd"], 0.0176483, "the turn's cost: {ledger}");
}

#[test]
#[ignore = "an image of the runner, which the test that execs it starts"]
fn an_image_of_the_runner() {
    shep_handover::record_launch_path();
    let arrival = shep_handover::boot::<Carry>().expect("the images agree on the state");
    let Some(folder) = std::env::var_os(FOLDER).map(PathBuf::from) else {
        return;
    };
    match arrival {
        None => outgoing(&folder),
        Some(arrival) => incoming(&folder, arrival.state),
    }
}

pub(super) fn read(folder: &Path, name: &str) -> String {
    std::fs::read_to_string(folder.join(name)).unwrap_or_default()
}

pub(super) fn write(folder: &Path, name: &str, text: &str) {
    std::fs::write(folder.join(name), text).unwrap();
}

fn agents(rig: &Rig, folder: &Path) -> (Base, ClaudeCli) {
    let base =
        Base::default().sandboxed(Arc::new(OpenSandbox::default()), rig.home.path().to_owned());
    let login = LoginToken::new("token".to_owned());
    let claude = (ClaudeCli::on(&base).with_program(folder.join("claude"))).with_login(login);
    (base, claude)
}

pub(super) fn worker_of(
    runner: &Arc<Mutex<Runner>>,
    handover: Arc<Handing>,
) -> (Worker, Sender<()>) {
    let (wake, woken) = mpsc::channel();
    let (died, _) = mpsc::channel();
    let hooks = Hooks {
        on_wake: Box::new(|_| {}),
        finished: Box::new(|_| {}),
        handover,
    };
    let worker = Worker::spawn(Arc::clone(runner), hooks, wake.clone(), woken, died);
    (worker, wake)
}

// The first image: a turn runs as a child, and the runner is asked to hand
// over. It returns only if that did not happen.
fn outgoing(folder: &Path) {
    let rig = Rig::new("shep");
    write(folder, "home", &rig.home.path().display().to_string());
    write(folder, "result.json", RESULT);
    let script = format!(
        "#!/bin/sh\ntouch {0}/started\n{1}\ncat {0}/result.json\n",
        folder.display(),
        waits_for(&folder.join("go"), CALL_PATIENCE)
    );
    write_script(&folder.join("claude"), &script);
    let (base, claude) = agents(&rig, folder);
    let runner = Arc::new(rig.open_agents(Arc::new(claude)).unwrap());
    rig.ask(&runner, "add", Some("7"));
    let handing = Handing::installed_unchecked(base);
    let (_worker, wake) = worker_of(&runner, Arc::clone(&handing));
    eventually("the turn's child", || folder.join("started").exists());
    write(folder, "pid.before", &std::process::id().to_string());

    let answer = handing.ask(&wake);
    panic!("the runner did not hand over: {answer}");
}

// The second image: it finishes the turn the first left running.
fn incoming(folder: &Path, carry: Carry) {
    write(folder, "pid.after", &std::process::id().to_string());
    let rig = Rig::over("shep", Path::new(read(folder, "home").trim()));
    let runner = Arc::new(rig.open_agents(Arc::new(agents(&rig, folder).1)).unwrap());
    let (wake, woken) = mpsc::channel();
    {
        let mut runner = runner.lock().unwrap();
        runner.wake_with(wake.clone());
        runner.adopt_calls(carry.into_turns());
    }
    let (died, _) = mpsc::channel();
    let hooks = Hooks {
        on_wake: Box::new(|_| {}),
        finished: Box::new(|_| {}),
        handover: Handing::never(),
    };
    let worker = Worker::spawn(Arc::clone(&runner), hooks, wake, woken, died);
    write(folder, "go", "");
    eventually("the turn's end", || turn_of_7(&rig, &runner) == "ended");
    // What the wait saw, as the worker may move #7 on before a second read
    write(folder, "turn", "ended");

    let lines = crate::usage::read(&rig.paths().folder.join(crate::usage::FILE)).unwrap();
    let turn = lines.first().expect("a usage line for the turn");
    write(folder, "ledger", &serde_json::to_string(turn).unwrap());
    assert!(worker.stop(CALL_PATIENCE, || {}));
}
