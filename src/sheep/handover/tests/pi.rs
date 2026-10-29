//! A handover through a real `execve` with a pi worker turn running behind a
//! gateway
//!
//! As in `exec`, the driver runs this binary on [`IMAGE`], the outgoing image
//! until it execs and then the incoming one. The stand-in paddock lives in the
//! driver, which sees what the two images asked of it.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::super::super::{Hooks, JOIN_BOUND, Worker};
use super::exec::{FOLDER, read, worker_of, write};
use super::*;
use crate::adapters::{Base, PiCli};
use crate::forwarder::Upstream;
use crate::ipc::blocking::Stream;
use crate::ports::{Agents, Routed};
use crate::settings::Harness;
use crate::settings::Timing;
use crate::settings::{EndpointUrl, Gateway, GatewayKind, GatewayName, Gateways, KeyVar};
use crate::test::{Answer, Hold, OpenSandbox, StandInEndpoint, waits_for, write_script};

const IMAGE: &str = "sheep::handover::tests::pi::an_image_of_the_runner";

// Names the stand-in paddock the images ask, which the driver runs.
const GATEWAY: &str = "KELPIE_HANDOVER_TEST_GATEWAY";

// The gateway's key is read from `CARGO_PKG_NAME`, which `cargo test` sets.
const KEY: &str = env!("CARGO_PKG_NAME");

const FRESH: &str = include_str!("../../../../fixtures/pi-p-fresh.jsonl");
const FRESH_ID: &str = "0b6f3c1e-7d2a-4f4e-9a51-3c8d2e6f1a07";

const WORKER: &str = "---\nrole: implementer\nharness: pi\nmodel: coder\neffort: low\n\
                      gateway: paddock\ncontext: 65536\n---\n";

fn paddock() -> StandInEndpoint {
    StandInEndpoint::start([Answer::Says("hello")]).like_paddock(KEY, &["coder"])
}

#[test]
fn a_handover_with_a_pi_worker_turn_running_ends_the_turn_on_the_new_build() {
    let (server, folder) = (paddock(), crate::test::short_tempdir());
    let ran = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            IMAGE,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(FOLDER, folder.path())
        .env(GATEWAY, server.host())
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
    assert_eq!(ledger["harness"], "pi", "{ledger}");
    assert!(ledger["usage"]["input"].as_u64() > Some(0), "{ledger}");

    // The forwarder answered the chat request made after the exec, naming the
    // lease the first image took, and the new image's own key.
    let seen = server.seen();
    let chat = seen
        .iter()
        .position(|l| l.contains("/chat/completions"))
        .expect("a chat request reached the gateway");
    let headers = &server.paddock_headers()[chat];
    assert!(
        headers.contains(&("x-paddock-lease".to_owned(), "L1".to_owned())),
        "{headers:?}"
    );
    assert_eq!(
        server.authorization_headers()[chat],
        [format!("Bearer {KEY}")]
    );
    assert!(
        read(folder.path(), "chat").contains("hello"),
        "the turn's own request got the gateway's answer"
    );

    // One take, a renewal while the call ran, and one release at the end.
    let leases: Vec<&str> = seen
        .iter()
        .map(String::as_str)
        .filter(|line| line.contains("/paddock/leases"))
        .collect();
    let takes = leases.iter().filter(|l| l.starts_with("POST")).count();
    assert_eq!(takes, 1, "no second take: {leases:?}");
    let releases = leases.iter().filter(|l| l.starts_with("DELETE")).count();
    assert_eq!(releases, 1, "{leases:?}");
    assert!(
        leases.last().is_some_and(|l| l.starts_with("DELETE")),
        "released once the call ended: {leases:?}"
    );
    let released = seen.iter().position(|l| l.starts_with("DELETE")).unwrap();
    assert!(released > chat, "released before the call asked: {seen:?}");
    // The resumed lease renews at once, so one falls before the release
    // whatever the heartbeat's timing.
    let taken = seen
        .iter()
        .position(|l| l.starts_with("POST /paddock"))
        .unwrap();
    let renewed = seen[taken..released].iter().any(|l| l.starts_with("PUT"));
    assert!(renewed, "no renewal while the call ran: {seen:?}");
}

#[test]
#[ignore = "an image of the runner, which the test that execs it starts"]
fn an_image_of_the_runner() {
    shep_handover::record_launch_path();
    let arrival = shep_handover::boot::<Carry>().expect("the images agree on the state");
    let (Some(folder), Ok(host)) = (std::env::var_os(FOLDER), std::env::var(GATEWAY)) else {
        return;
    };
    match arrival {
        None => outgoing(Path::new(&folder), &host),
        Some(arrival) => incoming(Path::new(&folder), &host, arrival.state),
    }
}

fn gateways(host: &str) -> Gateways {
    let gateway = Gateway {
        kind: GatewayKind::Paddock,
        url: EndpointUrl::try_from(host.to_owned()).unwrap(),
        key_env: KeyVar::try_from("CARGO_PKG_NAME".to_owned()).unwrap(),
    };
    let name = GatewayName::try_from("paddock".to_owned()).unwrap();
    // Renewed often, so the images' renewals show on either side of the exec.
    let timing = Timing {
        renew: Duration::from_millis(100),
        ..Timing::RUNNER
    };
    Gateways::new(BTreeMap::from([(name, gateway)])).with_paddock_timing(timing)
}

// pi as a script that waits for `go`, asks the gateway through its forwarder,
// and prints the recorded turn under the session it was asked for.
fn pi_script(folder: &Path) {
    write(folder, "fresh.jsonl", &FRESH.replace(FRESH_ID, "SESSION"));
    let script = format!(
        "#!/bin/sh\n\
         shared={shared}\n\
         ls \"$(dirname \"$PI_CODING_AGENT_DIR\")\"/*.sock > \"$shared/socket\"\n\
         echo \"${{PI_CODING_AGENT_DIR%.pi}}.call.json\" > \"$shared/record\"\n\
         while [ $# -gt 0 ]; do [ \"$1\" = --session-id ] && session=$2; shift; done\n\
         touch \"$shared/started\"\n\
         {waits}\n\
         curl -sS --fail --unix-socket \"$(cat \"$shared/socket\")\" -X POST \
         -H 'content-type: application/json' -d '{{\"model\":\"coder\"}}' \
         http://model.kelpie.test/v1/chat/completions > \"$shared/chat\" || exit 3\n\
         sleep 0.4\n\
         sed \"s/SESSION/$session/g\" \"$shared/fresh.jsonl\"\n",
        shared = folder.display(),
        waits = waits_for(&folder.join("go"), CALL_PATIENCE)
    );
    write_script(&folder.join("pi"), &script);
}

// A runner whose worker is a pi agent behind the gateway at `host`.
fn rig_for(rig: &Rig, host: &str) {
    rig.write_agent("pi-worker", WORKER);
    rig.agent_board.ranks(&["pi-worker"]);
    let settings = std::fs::read_to_string(rig.kelpie_settings_file()).unwrap();
    let gateway = format!("[gateways.paddock]\nurl = \"{host}\"\nkey_env = \"CARGO_PKG_NAME\"\n");
    rig.set_kelpie_settings(&format!("{settings}{gateway}"));
}

fn agents(rig: &Rig, folder: &Path, host: &str) -> (Base, Arc<Routed>) {
    let base =
        Base::default().sandboxed(Arc::new(OpenSandbox::default()), rig.home.path().to_owned());
    let pi = PiCli::on(&base)
        .with_program(folder.join("pi"))
        .with_gateways(gateways(host));
    let stand_in: Arc<dyn Agents> = Arc::new(rig.claude.clone());
    let routed = Routed::new(vec![
        (Harness::ClaudeCode, stand_in.clone()),
        (Harness::Pi, Arc::new(pi)),
        (Harness::Codex, stand_in.clone()),
        (Harness::Goose, stand_in),
    ]);
    (base, Arc::new(routed))
}

// The first image: a pi turn runs as a child, and the runner is asked to
// hand over. It returns only if that did not happen.
fn outgoing(folder: &Path, host: &str) {
    let rig = Rig::new("shep");
    write(folder, "home", &rig.home.path().display().to_string());
    rig_for(&rig, host);
    pi_script(folder);
    let (base, routed) = agents(&rig, folder, host);
    let runner = Arc::new(rig.open_agents(routed).unwrap());
    rig.ask(&runner, "add", Some("7"));
    let handing = Handing::installed_unchecked(base);
    let (_worker, wake) = worker_of(&runner, Arc::clone(&handing));
    eventually("the turn's child", || folder.join("started").exists());
    write(folder, "pid.before", &std::process::id().to_string());

    let answer = handing.ask(&wake);
    panic!("the runner did not hand over: {answer}");
}

// The second image: it finishes the turn the first left running, once its
// forwarder answers on the socket the first one bound.
fn incoming(folder: &Path, host: &str, carry: Carry) {
    write(folder, "pid.after", &std::process::id().to_string());
    let rig = Rig::over("shep", Path::new(read(folder, "home").trim()));
    let (_base, routed) = agents(&rig, folder, host);
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
    let socket = PathBuf::from(read(folder, "socket").trim());
    eventually("the forwarder answering again", || {
        Stream::connect(&socket).is_ok()
    });
    write(folder, "go", "");
    eventually("the turn's end", || turn_of_7(&rig, &runner) == "ended");
    // What the wait saw, as the worker may move #7 on before a second read
    write(folder, "turn", "ended");

    let lines = crate::usage::read(&rig.paths().folder.join(crate::usage::FILE)).unwrap();
    let turn = lines.first().expect("a usage line for the turn");
    write(folder, "ledger", &serde_json::to_string(turn).unwrap());
    assert!(worker.stop(JOIN_BOUND, || {}));
}

// A model server that sends a first part of its reply, holds the rest until
// `go`, and then sends it.
fn two_part_server() -> (String, mpsc::Sender<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (go, wait) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut seen = Vec::new();
        let mut byte = [0u8; 1];
        while !seen.ends_with(b"\r\n\r\n{}") {
            stream.read_exact(&mut byte).unwrap();
            seen.push(byte[0]);
        }
        let first = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ndata: one\n\n";
        stream.write_all(first.as_bytes()).unwrap();
        let _ = wait.recv_timeout(Duration::from_secs(30));
        let _ = stream.write_all(b"data: two\n\n");
    });
    (url, go)
}

#[test]
fn a_handover_waits_out_a_reply_the_forwarder_is_relaying() {
    waits_out_a_relayed_reply();
}

// A forwarder relaying a model's reply holds a handover until the reply ends.
// The forwarder is the same type for every harness that has one, so goose's
// test runs this too.
pub(super) fn waits_out_a_relayed_reply() {
    let rig = Rig::new("shep");
    let hold = Hold::default();
    let (url, go) = two_part_server();
    let dir = crate::test::short_tempdir();
    let folder = dir.path().canonicalize().unwrap();
    let base = EndpointUrl::try_from(url).unwrap();
    let forwarder =
        crate::adapters::forwarder::Forwarder::open(&folder, Upstream::new(&base).unwrap())
            .unwrap();
    let mut stream = Stream::connect(&forwarder.socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let chat = "POST http://model.kelpie.test/v1/chat/completions HTTP/1.1\r\n\
                host: model.kelpie.test\r\ncontent-length: 2\r\n\r\n{}";
    stream.write_all(chat.as_bytes()).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    while !String::from_utf8_lossy(&got).contains("data: one") {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "ended early: {:?}", String::from_utf8_lossy(&got));
        got.extend(&buf[..n]);
    }

    let execs = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&execs);
    let handing = Handing::with(
        Box::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            "stand-in".to_owned()
        }),
        Box::new(|| Some(Box::new(()))),
        Box::new(crate::adapters::feeding),
    );
    let (_runner, worker, wake) = in_a_turn(&rig, &hold, Arc::clone(&handing));

    let answer: serde_json::Value = serde_json::from_str(&handing.ask(&wake)).unwrap();

    assert_eq!(answer["handover"], "waiting for the runner's quiet point");
    assert_eq!(execs.load(Ordering::SeqCst), 0, "it did not wait");
    go.send(()).unwrap();
    stream.read_to_end(&mut got).unwrap();
    eventually("the handover once the reply ended", || {
        execs.load(Ordering::SeqCst) == 1
    });
    hold.release();
    assert!(worker.stop(JOIN_BOUND, || {}));
}

struct PiTurn {
    rig: Rig,
    runner: Arc<Mutex<Runner>>,
    handing: Arc<Handing>,
    tried: Tried,
    worker: Worker,
    wake: Sender<()>,
}

// A runner with a pi turn running on issue 7, and a handing that refuses at the exec
// so a test sees whether it was tried.
fn in_a_pi_turn(shared: &Path, server: &StandInEndpoint) -> PiTurn {
    let rig = Rig::new("shep");
    rig_for(&rig, server.host());
    pi_script(shared);
    let (_base, routed) = agents(&rig, shared, server.host());
    let runner = Arc::new(rig.open_agents(routed).unwrap());
    rig.ask(&runner, "add", Some("7"));
    let (handing, tried) = refusing("stand-in", &Arc::default());
    let (worker, wake) = worker_of(&runner, Arc::clone(&handing));
    eventually("the turn's child", || shared.join("started").exists());
    PiTurn {
        rig,
        runner,
        handing,
        tried,
        worker,
        wake,
    }
}

// The call's process is over when its record is gone, and the loop has not heard it.
#[test]
fn a_pi_turn_whose_record_is_gone_keeps_the_handover_queued_until_its_end_is_heard() {
    let (server, shared) = (paddock(), crate::test::short_tempdir());
    let folder = shared.path();
    let PiTurn {
        rig,
        runner,
        handing,
        tried,
        worker,
        wake,
    } = in_a_pi_turn(folder, &server);
    std::fs::remove_file(read(folder, "record").trim()).unwrap();

    let answer: serde_json::Value = serde_json::from_str(&handing.ask(&wake)).unwrap();

    assert_eq!(answer["handover"], "waiting for the runner's quiet point");
    assert_eq!(tried.execs.load(Ordering::SeqCst), 0, "it execed anyway");
    assert!(handing.pending());
    assert_eq!(turn_of_7(&rig, &runner), "running");
    write(folder, "go", "");
    eventually("the handover once the end was heard", || {
        tried.execs.load(Ordering::SeqCst) == 1
    });
    assert!(tried.carried.lock().unwrap().is_empty());
    assert!(!handing.pending());
    eventually("the turn's end", || turn_of_7(&rig, &runner) == "ended");
    // The turn reads `ended` once the runner has saved it, and the pass that
    // heard it goes on to move #7 with git calls. A stop waits out that pass,
    // which a loaded runner can hold past `JOIN_BOUND`, so this one waits as
    // long as any other call, and a thread that never stops still fails it.
    assert!(worker.stop(CALL_PATIENCE, || {}));
}

#[test]
fn a_pi_turn_whose_record_stays_gone_refuses_the_handover_and_the_old_image_keeps_its_call() {
    let (server, shared) = (paddock(), crate::test::short_tempdir());
    let folder = shared.path();
    let PiTurn {
        rig,
        runner,
        handing,
        tried,
        worker,
        wake,
    } = in_a_pi_turn(folder, &server);
    std::fs::remove_file(read(folder, "record").trim()).unwrap();
    let answer = refused_once_the_wait_is_up(&rig, &runner, &handing, wake);
    let error = answer["error"].as_str().unwrap();
    assert!(
        error.starts_with("handover refused: the turn of #7 cannot be carried: the call's record "),
        "{error}"
    );
    assert!(error.contains("cannot be read"), "{error}");
    assert_eq!(tried.execs.load(Ordering::SeqCst), 0, "it execed anyway");
    assert_eq!(turn_of_7(&rig, &runner), "running");
    write(folder, "go", "");
    eventually("the turn's end", || turn_of_7(&rig, &runner) == "ended");
    assert!(worker.stop(JOIN_BOUND, || {}));
}

#[test]
fn a_pi_turn_whose_record_is_there_and_not_valid_refuses_the_handover_at_once() {
    let (server, shared) = (paddock(), crate::test::short_tempdir());
    let folder = shared.path();
    let PiTurn {
        rig,
        runner,
        handing,
        tried,
        worker,
        wake,
    } = in_a_pi_turn(folder, &server);
    write_record(folder, "{ not a record");

    let answer: serde_json::Value = serde_json::from_str(&handing.ask(&wake)).unwrap();

    let error = answer["error"].as_str().unwrap();
    assert!(
        error.starts_with("handover refused: the turn of #7 cannot be carried: the call's record "),
        "{error}"
    );
    assert_eq!(tried.execs.load(Ordering::SeqCst), 0, "it execed anyway");
    assert_eq!(turn_of_7(&rig, &runner), "running");
    write(folder, "go", "");
    eventually("the turn's end", || turn_of_7(&rig, &runner) == "ended");
    assert!(worker.stop(JOIN_BOUND, || {}));
}

// Replaces the running call's record with `text`.
fn write_record(folder: &Path, text: &str) {
    std::fs::write(read(folder, "record").trim(), text).unwrap();
}
