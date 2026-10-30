//! A handover through a real `execve` with a Codex worker turn running
//!
//! As in `exec`, the driver runs this binary on [`IMAGE`], the outgoing image
//! until it execs and then the incoming one. Codex holds no lease and goes
//! through no forwarder, so the images share only files.

use std::path::Path;
use std::process::Command;

use super::super::super::{Hooks, Worker};
use super::exec::{FOLDER, read, worker_of, write};
use super::*;
use crate::adapters::{Base, CodexCli};
use crate::ports::{Agents, Routed};
use crate::settings::Harness;
use crate::test::{OpenSandbox, waits_for, write_script};

const IMAGE: &str = "sheep::handover::tests::codex::an_image_of_the_runner";

const FRESH: &str = include_str!("../../../../fixtures/codex-exec-fresh.jsonl");

/// The Codex session the recording ran in
const THREAD: &str = "01a0f7fd-4ed5-7773-80ea-165edc292d8d";

const WORKER: &str =
    "---\nrole: implementer\nharness: codex\nmodel: gpt-6-sol\neffort: medium\n---\n";

#[test]
fn a_handover_with_a_codex_worker_turn_running_ends_the_turn_on_the_new_build() {
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
    assert!(ran.status.success(), "the images failed:\n{said}");
    let (before, after) = (
        read(folder.path(), "pid.before"),
        read(folder.path(), "pid.after"),
    );
    assert!(!before.is_empty(), "the first image never ran");
    assert_eq!(before, after, "the runner kept its pid");
    assert_eq!(read(folder.path(), "turn"), "ended");

    // The usage line and the thread record are what `run` would have written.
    let ledger: serde_json::Value = serde_json::from_str(&read(folder.path(), "ledger")).unwrap();
    assert_eq!(ledger["harness"], "codex", "{ledger}");
    assert_eq!(ledger["usage"]["input"], 6959, "{ledger}");
    assert_eq!(ledger["usage"]["cache_read"], 10752, "{ledger}");
    assert_eq!(ledger["usage"]["output"], 44, "{ledger}");
    let thread: serde_json::Value = serde_json::from_str(&read(folder.path(), "thread")).unwrap();
    assert_eq!(thread["id"], THREAD, "{thread}");
    assert_eq!(thread["spent"]["output_tokens"], 44, "{thread}");
    let _ = std::fs::remove_dir_all(read(folder.path(), "home").trim());
}

#[test]
#[ignore = "an image of the runner, which the test that execs it starts"]
fn an_image_of_the_runner() {
    shep_handover::record_launch_path();
    let arrival = shep_handover::boot::<Carry>().expect("the images agree on the state");
    let Some(folder) = std::env::var_os(FOLDER) else {
        return;
    };
    match arrival {
        None => outgoing(Path::new(&folder)),
        Some(arrival) => incoming(Path::new(&folder), arrival.state),
    }
}

// Codex as a script that waits for `go` and prints the recorded turn.
fn codex_script(folder: &Path) {
    write(folder, "fresh.jsonl", FRESH);
    let script = format!(
        "#!/bin/sh\n\
         shared={shared}\n\
         echo \"${{CODEX_HOME%.codex}}.call.json\" > \"$shared/record\"\n\
         touch \"$shared/started\"\n\
         {waits}\n\
         cat \"$shared/fresh.jsonl\"\n",
        shared = folder.display(),
        waits = waits_for(&folder.join("go"), CALL_PATIENCE)
    );
    write_script(&folder.join("codex"), &script);
}

fn agents(rig: &Rig, folder: &Path) -> (Base, Arc<Routed>) {
    let base =
        Base::default().sandboxed(Arc::new(OpenSandbox::default()), rig.home.path().to_owned());
    let codex =
        CodexCli::on(&base, rig.home.path().join("login")).with_program(folder.join("codex"));
    let stand_in: Arc<dyn Agents> = Arc::new(rig.claude.clone());
    let routed = Routed::new(vec![
        (Harness::ClaudeCode, stand_in.clone()),
        (Harness::Pi, stand_in.clone()),
        (Harness::Codex, Arc::new(codex)),
        (Harness::Goose, stand_in),
    ]);
    (base, Arc::new(routed))
}

fn rig_for(rig: &Rig) {
    rig.write_agent("codex-worker", WORKER);
    rig.agent_board.ranks(&["codex-worker"]);
}

// The first image: a Codex turn runs as a child, and the runner is asked to
// hand over. It returns only if that did not happen.
fn outgoing(folder: &Path) {
    let rig = Rig::new("shep");
    write(folder, "home", &rig.home.path().display().to_string());
    rig_for(&rig);
    codex_script(folder);
    let (base, routed) = agents(&rig, folder);
    let runner = Arc::new(rig.open_agents(routed).unwrap());
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
    let (_base, routed) = agents(&rig, folder);
    let runner = Arc::new(rig.open_agents(routed).unwrap());
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
    let before = calls_of_7(&rig, &runner);
    write(folder, "go", "");
    see_the_turn_end(&rig, &runner, before);
    // What the wait saw, as the worker may move #7 on before a second read
    write(folder, "turn", "ended");

    let lines = crate::usage::read(&rig.paths().folder.join(crate::usage::FILE)).unwrap();
    let turn = lines.first().expect("a usage line for the turn");
    write(folder, "ledger", &serde_json::to_string(turn).unwrap());
    let record = read(folder, "record");
    let threads = Path::new(record.trim())
        .with_extension("")
        .with_extension("threads");
    let mut files = std::fs::read_dir(threads).unwrap();
    let thread = files.next().expect("a thread record").unwrap().path();
    write(folder, "thread", &std::fs::read_to_string(thread).unwrap());
    assert!(worker.stop(CALL_PATIENCE, || {}));
}

#[test]
fn a_codex_turn_whose_record_is_gone_refuses_the_handover_and_the_old_image_keeps_its_call() {
    let shared = crate::test::short_tempdir();
    let folder = shared.path();
    let rig = Rig::new("shep");
    rig_for(&rig);
    codex_script(folder);
    let (_base, routed) = agents(&rig, folder);
    let runner = Arc::new(rig.open_agents(routed).unwrap());
    rig.ask(&runner, "add", Some("7"));
    let (handing, tried) = refusing("unused", &Arc::default());
    let (worker, wake) = worker_of(&runner, Arc::clone(&handing));
    eventually("the turn's child", || folder.join("started").exists());
    let record = read(folder, "record");
    eventually("the call's record", || Path::new(record.trim()).exists());
    std::fs::remove_file(record.trim()).unwrap();

    let answer = refused_once_the_wait_is_up(&rig, &runner, &handing, wake);

    let error = answer["error"].as_str().unwrap();
    assert!(
        error.starts_with("handover refused: the turn of #7 cannot be carried: the call's record "),
        "{error}"
    );
    assert!(error.contains("cannot be read"), "{error}");
    assert_eq!(tried.execs.load(Ordering::SeqCst), 0, "it execed anyway");
    assert_eq!(turn_of_7(&rig, &runner), "running");
    let before = calls_of_7(&rig, &runner);
    write(folder, "go", "");
    see_the_turn_end(&rig, &runner, before);
    assert!(worker.stop(CALL_PATIENCE, || {}));
}
