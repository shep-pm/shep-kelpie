//! The lease round trip through a real pinned shepherd
//!
//! Starts its own shepherd under a scratch `SHEP_HOME`, with the dog and
//! two stand-in runners as sheep, the way the experiments repo's lease
//! dog was tested. A stand-in runner is this test binary run as a sheep:
//! see `stand_in_runner`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kelpie::lease::wire::Asker;
use kelpie::lease::{Epoch, LeaseKind};
use serde_json::{Value, json};
use shep_client::Client;
use shep_client::shep_core::protocol::Request;
use shep_client::shep_core::protocol::request::{ActionOutcome, Response, SelectorSpec};

const KELPIE: &str = env!("CARGO_BIN_EXE_kelpie");
const STAND_IN: &str = "KELPIE_TEST_STAND_IN";

fn shep_binary() -> PathBuf {
    Path::new(&std::env::var_os("HOME").unwrap()).join(".kelpie/bin/shep")
}

fn now_ms() -> u64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    u64::try_from(since.as_millis()).unwrap()
}

// When this run last asked and was last granted, in Unix milliseconds.
#[derive(Default)]
struct Times {
    wanted_ms: u64,
    granted_ms: u64,
}

// A project runner reduced to its lease traffic. Triggers: `want <kind>`
// and `return <kind>` raise the totals, `grant` is the dog's, and
// `status` reports what this run holds and when it asked and was granted.
#[test]
#[ignore = "a sheep of the_lease_round_trip_runs_through_a_real_shepherd"]
fn stand_in_runner() {
    if std::env::var_os(STAND_IN).is_none() {
        return;
    }
    let shepherd = shep_channel::serve();
    let epoch = Epoch(u64::from(std::process::id()));
    let asker = Arc::new(Mutex::new(Asker::new(epoch)));
    let times = Arc::new(Mutex::new(Times::default()));
    for action in ["want", "return", "grant", "status"] {
        let (asker, times, shepherd) = (asker.clone(), times.clone(), shepherd.clone());
        shepherd.clone().on_action(action, move |params, name| {
            let mut asker = asker.lock().unwrap();
            let mut times = times.lock().unwrap();
            let kind = || LeaseKind::try_from(params.unwrap_or_default().trim()).unwrap();
            match name {
                "want" => {
                    times.wanted_ms = now_ms();
                    let (metric, value) = asker.want(&kind());
                    shepherd.metric(metric, value);
                }
                "return" => {
                    let (metric, value) = asker.give_back(&kind());
                    shepherd.metric(metric, value);
                }
                "grant" => match asker.grant(params.unwrap_or_default()) {
                    Ok(_) => times.granted_ms = now_ms(),
                    Err(e) => return e.to_string(),
                },
                _ => {}
            }
            let stand_in = LeaseKind::try_from("stand-in").unwrap();
            json!({
                "epoch": epoch.0,
                "holds": asker.holds(&stand_in),
                "wanted_ms": times.wanted_ms,
                "granted_ms": times.granted_ms,
            })
            .to_string()
        });
    }
    shepherd.ready().unwrap();
    loop {
        std::thread::park();
    }
}

// A child that is killed if the test ends before it is waited on.
struct Reaped(Option<std::process::Child>);

impl Reaped {
    fn wait(mut self) -> Output {
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}

impl Drop for Reaped {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// The scratch shepherd, killed however the test ends.
struct Shepherd {
    home: tempfile::TempDir,
}

impl Drop for Shepherd {
    fn drop(&mut self) {
        let _ = self.shep(&["kill"]);
    }
}

impl Shepherd {
    // Under /tmp: a socket path longer than 104 bytes is refused on macOS.
    fn start() -> Self {
        let home = tempfile::Builder::new()
            .prefix("kd")
            .tempdir_in("/tmp")
            .unwrap();
        let shepherd = Self { home };
        let flockfile = shepherd.home.path().join("flock.toml");
        std::fs::write(&flockfile, shepherd.flockfile()).unwrap();
        let out = shepherd.shep(&["start", flockfile.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        shepherd
    }

    fn flockfile(&self) -> String {
        let home = self.home.path().display();
        let me = std::env::current_exe().unwrap();
        let runner = |name: &str| {
            format!(
                "[[app]]\nname = {name:?}\nscript = {me:?}\n\
                 args = [\"--exact\", \"stand_in_runner\", \"--ignored\", \"--nocapture\"]\n\
                 channel = true\nautorestart = false\nenv = {{ {STAND_IN} = \"1\" }}\n\n"
            )
        };
        format!(
            "[[app]]\nname = \"kelpie\"\nscript = {KELPIE:?}\nargs = [\"dog\"]\n\
             channel = true\nshutdown_with_message = true\nautorestart = false\n\
             env = {{ SHEP_HOME = \"{home}\", TMPDIR = \"{home}\" }}\n\n{}{}",
            runner("koji"),
            runner("reactmap")
        )
    }

    fn shep_ok(&self, args: &[&str]) {
        let out = self.shep(args);
        assert!(
            out.status.success(),
            "shep {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn shep(&self, args: &[&str]) -> Output {
        Command::new(shep_binary())
            .args(args)
            .env("SHEP_HOME", self.home.path())
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn kelpie(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(KELPIE);
        cmd.args(args)
            .env("SHEP_HOME", self.home.path())
            .env("TMPDIR", self.home.path())
            .stdin(Stdio::null());
        cmd
    }

    async fn client(&self) -> Client {
        let socket = self.home.path().join("run/shep.sock");
        Client::connect(&socket).await.unwrap()
    }
}

// Sends one trigger and returns its reply body as JSON, or `null` when
// the sheep did not answer.
async fn trigger(client: &Client, sheep: &str, action: &str, params: Option<&str>) -> Value {
    let reply = client
        .request(Request::Trigger {
            selector: SelectorSpec::Name(sheep.into()),
            action: action.into(),
            params: params.map(Into::into),
        })
        .await
        .unwrap();
    let Response::Triggered(rows) = reply else {
        panic!("a trigger answered {reply:?}");
    };
    match rows.into_iter().next().map(|row| row.outcome) {
        Some(ActionOutcome::Replied { body }) => serde_json::from_str(&body).unwrap_or(json!(body)),
        _ => Value::Null,
    }
}

async fn until(what: &str, mut probe: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !probe().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn holds(client: &Client, runner: &str) -> bool {
    trigger(client, runner, "status", None).await["holds"] == true
}

async fn book(client: &Client) -> Value {
    stand_in(&trigger(client, "kelpie", "status", None).await)
}

fn stand_in(status: &Value) -> Value {
    let leases = status["leases"].as_array().expect("a leases list");
    let line = leases.iter().find(|l| l["kind"] == "stand-in");
    line.cloned()
        .unwrap_or_else(|| panic!("no stand-in line in {status}"))
}

#[tokio::test]
#[ignore = "needs the pinned shepherd at ~/.kelpie/bin/shep"]
async fn the_lease_round_trip_runs_through_a_real_shepherd() {
    let shepherd = Shepherd::start();
    let client = shepherd.client().await;
    until("the dog and both runners", async || {
        trigger(&client, "kelpie", "status", None).await["leases"].is_array()
            && trigger(&client, "koji", "status", None).await.is_object()
            && trigger(&client, "reactmap", "status", None)
                .await
                .is_object()
    })
    .await;

    // A free lease is granted.
    trigger(&client, "koji", "want", Some("stand-in")).await;
    until("koji's grant", async || holds(&client, "koji").await).await;
    let koji = trigger(&client, "koji", "status", None).await;
    let round_trip = koji["granted_ms"]
        .as_u64()
        .unwrap()
        .saturating_sub(koji["wanted_ms"].as_u64().unwrap());

    // A held one queues.
    trigger(&client, "reactmap", "want", Some("stand-in")).await;
    until("reactmap in the queue", async || {
        book(&client).await["queue"] == json!([{ "runner": "reactmap" }])
    })
    .await;
    assert!(!holds(&client, "reactmap").await);

    // A runner that dies loses its lease to the next waiter.
    let killed = now_ms();
    shepherd.shep_ok(&["signal", "koji", "SIGKILL"]);
    until("the reclaim", async || holds(&client, "reactmap").await).await;
    let reclaim = trigger(&client, "reactmap", "status", None).await["granted_ms"]
        .as_u64()
        .unwrap()
        .saturating_sub(killed);

    // The maintainer waits without preempting, and goes ahead of a runner
    // that asked first.
    let take = Reaped(Some(
        shepherd
            .kelpie(&["lease", "take", "stand-in"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    until("the maintainer in the queue", async || {
        book(&client).await["queue"] == json!(["maintainer"])
    })
    .await;
    shepherd.shep_ok(&["restart", "koji"]);
    until("koji back", async || {
        trigger(&client, "koji", "status", None).await.is_object()
    })
    .await;
    trigger(&client, "koji", "want", Some("stand-in")).await;
    until("koji behind the maintainer", async || {
        book(&client).await["queue"] == json!(["maintainer", { "runner": "koji" }])
    })
    .await;
    assert!(holds(&client, "reactmap").await, "reactmap lost its lease");

    // A runner that restarts loses its lease; the maintainer is next.
    shepherd.shep_ok(&["restart", "reactmap"]);
    let take = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || take.wait()),
    )
    .await
    .expect("take was never granted")
    .unwrap();
    assert!(
        take.status.success(),
        "{}",
        String::from_utf8_lossy(&take.stderr)
    );
    assert!(String::from_utf8_lossy(&take.stdout).contains("stand-in is yours"));
    assert_eq!(book(&client).await["holder"], "maintainer");

    // The maintainer's return goes to the runner waiting behind.
    let back = shepherd
        .kelpie(&["lease", "return", "stand-in"])
        .output()
        .unwrap();
    assert!(
        back.status.success(),
        "{}",
        String::from_utf8_lossy(&back.stderr)
    );
    until("koji's grant", async || holds(&client, "koji").await).await;

    // Status names each holder, the GPU's read from its lock.
    let status = shepherd.kelpie(&["lease", "status"]).output().unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    let lock = shepherd.home.path().join("qwen-review/gpu.lock");
    let leases = status["leases"].as_array().unwrap();
    let gpu = leases
        .iter()
        .find(|l| l["kind"] == "gpu")
        .expect("a GPU line");
    assert_eq!(gpu["lock"], lock.display().to_string());
    assert_eq!(stand_in(&status)["holder"], json!({ "runner": "koji" }));
    assert!(stand_in(&status)["since"].as_u64().is_some());

    eprintln!("want to grant {round_trip} ms, killed to next grant {reclaim} ms");
}
