//! A handover through a real `execve` with a goose worker turn running behind
//! a gateway
//!
//! As in `pi`, the driver runs this binary on [`IMAGE`], the outgoing image
//! until it execs and then the incoming one. The stand-in paddock lives in the
//! driver, which sees what the two images asked of it. The images run once,
//! and the tests below read what that run left.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use super::super::super::{Hooks, Worker};
use super::exec::{FOLDER, read, worker_of, write};
use super::*;
use crate::adapters::{Base, GooseCli};
use crate::ipc::blocking::Stream;
use crate::ports::{Agents, Routed};
use crate::settings::Harness;
use crate::settings::Timing;
use crate::settings::{EndpointUrl, Gateway, GatewayKind, GatewayName, Gateways, KeyVar};
use crate::test::{Answer, OpenSandbox, StandInEndpoint, waits_for, write_script};

const IMAGE: &str = "sheep::handover::tests::goose::an_image_of_the_runner";

// Names the stand-in paddock the images ask, which the driver runs.
const GATEWAY: &str = "KELPIE_HANDOVER_TEST_GATEWAY";

// The gateway's key is read from `CARGO_PKG_NAME`, which `cargo test` sets.
const KEY: &str = env!("CARGO_PKG_NAME");

const FRESH: &str = include_str!("../../../../fixtures/goose-run-fresh.jsonl");

const WORKER: &str = "---\nrole: implementer\nharness: goose\nmodel: coder\neffort: low\n\
                      gateway: paddock\ncontext: 65536\n---\n";

fn paddock() -> StandInEndpoint {
    StandInEndpoint::start([Answer::Says("hello")]).like_paddock(KEY, &["coder"])
}

// What the two images and the stand-in paddock left behind.
struct Crossing {
    said: String,
    ok: bool,
    pid_before: String,
    pid_after: String,
    turn: String,
    ledger: String,
    chat: String,
    seen: Vec<String>,
    lease_headers: Vec<Vec<(String, String)>>,
    authorization: Vec<Vec<String>>,
}

// Runs the images once, however many tests ask.
fn crossing() -> &'static Crossing {
    static CROSSED: OnceLock<Crossing> = OnceLock::new();
    CROSSED.get_or_init(|| {
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
        let home = read(folder.path(), "home");
        let _ = std::fs::remove_dir_all(home.trim());
        let at = |name: &str| read(folder.path(), name);
        Crossing {
            said: format!(
                "{}\n{}",
                String::from_utf8_lossy(&ran.stdout),
                String::from_utf8_lossy(&ran.stderr)
            ),
            ok: ran.status.success(),
            pid_before: at("pid.before"),
            pid_after: at("pid.after"),
            turn: at("turn"),
            ledger: at("ledger"),
            chat: at("chat"),
            seen: server.seen(),
            lease_headers: server.paddock_headers(),
            authorization: server.authorization_headers(),
        }
    })
}

#[test]
fn a_handover_with_a_goose_worker_turn_running_ends_the_turn_on_the_new_build() {
    let crossed = crossing();

    assert!(crossed.ok, "the images failed:\n{}", crossed.said);
    assert!(!crossed.pid_before.is_empty(), "the first image never ran");
    assert_eq!(
        crossed.pid_before, crossed.pid_after,
        "the runner kept its pid"
    );
    assert_eq!(crossed.turn, "ended");
    let ledger: serde_json::Value = serde_json::from_str(&crossed.ledger).unwrap();
    assert_eq!(ledger["harness"], "goose", "{ledger}");
    assert_eq!(ledger["usage"]["input"], 20, "{ledger}");
    assert_eq!(ledger["usage"]["output"], 3, "{ledger}");

    // The forwarder answered the chat request made after the exec, naming the
    // lease the first image took, and the new image's own key.
    let chat = (crossed.seen.iter())
        .position(|l| l.contains("/chat/completions"))
        .expect("a chat request reached the gateway");
    assert!(
        crossed.lease_headers[chat].contains(&("x-paddock-lease".to_owned(), "L1".to_owned())),
        "{:?}",
        crossed.lease_headers[chat]
    );
    assert_eq!(crossed.authorization[chat], [format!("Bearer {KEY}")]);
    assert!(
        crossed.chat.contains("hello"),
        "the turn's own request got the gateway's answer"
    );
}

#[test]
fn the_stand_in_gateway_sees_the_lease_renewed_across_the_exec_with_no_release_and_no_second_take()
{
    let crossed = crossing();
    assert!(crossed.ok, "the images failed:\n{}", crossed.said);
    let seen = &crossed.seen;

    // The carried call's lease: one take, a renewal while the call ran, and a
    // release at the end. What the runner does once the turn has ended is not
    // the handover's: a worker with no pull request is sent back, and that
    // call takes a lease of its own before the test stops the worker.
    let released = (seen.iter())
        .position(|line| line.starts_with("DELETE"))
        .expect("the lease was released once the call ended");
    let leases: Vec<&str> = seen[..=released]
        .iter()
        .map(String::as_str)
        .filter(|line| line.contains("/paddock/leases"))
        .collect();
    let takes = leases.iter().filter(|l| l.starts_with("POST")).count();
    assert_eq!(takes, 1, "no second take: {leases:?}");
    assert!(
        leases.last().is_some_and(|l| l.starts_with("DELETE")),
        "released once the call ended: {leases:?}"
    );
    let chat = (seen.iter())
        .position(|l| l.contains("/chat/completions"))
        .expect("a chat request reached the gateway");
    assert!(released > chat, "released before the call asked: {seen:?}");
    // The resumed lease renews at once, so one falls before the release
    // whatever the heartbeat's timing.
    let taken = (seen.iter())
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

// goose as a script that waits for `go`, asks the gateway through its
// forwarder, and prints the recorded turn. The forwarder's socket is the
// newest in the call's folder, since it binds just before goose starts.
fn goose_script(folder: &Path) {
    write(folder, "fresh.jsonl", FRESH);
    let script = format!(
        "#!/bin/sh\n\
         shared={shared}\n\
         ls -t \"$(dirname \"$GOOSE_PATH_ROOT\")\"/*.sock | head -n 1 > \"$shared/socket\"\n\
         echo \"${{GOOSE_PATH_ROOT%.goose}}.call.json\" > \"$shared/record\"\n\
         touch \"$shared/started\"\n\
         {waits}\n\
         curl -sS --fail --unix-socket \"$(cat \"$shared/socket\")\" -X POST \
         -H 'content-type: application/json' -d '{{\"model\":\"coder\"}}' \
         http://model.kelpie.test/v1/chat/completions > \"$shared/chat\" || exit 3\n\
         sleep 0.4\n\
         cat \"$shared/fresh.jsonl\"\n",
        shared = folder.display(),
        waits = waits_for(&folder.join("go"), CALL_PATIENCE)
    );
    write_script(&folder.join("goose"), &script);
}

// A runner whose worker is a goose agent behind the gateway at `host`.
fn rig_for(rig: &Rig, host: &str) {
    rig.write_agent("goose-worker", WORKER);
    rig.agent_board.ranks(&["goose-worker"]);
    let settings = std::fs::read_to_string(rig.kelpie_settings_file()).unwrap();
    let gateway = format!("[gateways.paddock]\nurl = \"{host}\"\nkey_env = \"CARGO_PKG_NAME\"\n");
    rig.set_kelpie_settings(&format!("{settings}{gateway}"));
}

fn agents(rig: &Rig, folder: &Path, host: &str) -> (Base, Arc<Routed>) {
    let base =
        Base::default().sandboxed(Arc::new(OpenSandbox::default()), rig.home.path().to_owned());
    let goose = GooseCli::on(&base)
        .with_program(folder.join("goose"))
        .with_gateways(gateways(host));
    let stand_in: Arc<dyn Agents> = Arc::new(rig.claude.clone());
    let routed = Routed::new(vec![
        (Harness::ClaudeCode, stand_in.clone()),
        (Harness::Pi, stand_in.clone()),
        (Harness::Codex, stand_in),
        (Harness::Goose, Arc::new(goose)),
    ]);
    (base, Arc::new(routed))
}

// The first image: a goose turn runs as a child, and the runner is asked to
// hand over. It returns only if that did not happen.
fn outgoing(folder: &Path, host: &str) {
    let rig = Rig::new("shep");
    write(folder, "home", &rig.home.path().display().to_string());
    rig_for(&rig, host);
    goose_script(folder);
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
    let before = calls_of_7(&rig, &runner);
    write(folder, "go", "");
    see_the_turn_end(&rig, &runner, before);
    // What the wait saw, as the worker may move #7 on before a second read
    write(folder, "turn", "ended");

    let lines = crate::usage::read(&rig.paths().folder.join(crate::usage::FILE)).unwrap();
    let turn = lines.first().expect("a usage line for the turn");
    write(folder, "ledger", &serde_json::to_string(turn).unwrap());
    assert!(worker.stop(CALL_PATIENCE, || {}));
}

#[test]
fn a_handover_waits_out_a_reply_the_forwarder_is_relaying() {
    super::pi::waits_out_a_relayed_reply();
}

#[test]
fn a_goose_turn_whose_record_is_gone_refuses_the_handover_and_the_old_image_keeps_its_call() {
    let (server, shared) = (paddock(), crate::test::short_tempdir());
    let folder = shared.path();
    let rig = Rig::new("shep");
    rig_for(&rig, server.host());
    goose_script(folder);
    let (_base, routed) = agents(&rig, folder, server.host());
    let runner = Arc::new(rig.open_agents(routed).unwrap());
    rig.ask(&runner, "add", Some("7"));
    let (handing, tried) = refusing("unused", &Arc::default());
    let (worker, wake) = worker_of(&runner, Arc::clone(&handing));
    eventually("the turn's child", || folder.join("started").exists());
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
    let before = calls_of_7(&rig, &runner);
    write(folder, "go", "");
    see_the_turn_end(&rig, &runner, before);
    assert!(worker.stop(CALL_PATIENCE, || {}));
}
