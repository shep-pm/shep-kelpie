//! The lease round trip through a real pinned shepherd
//!
//! Starts its own shepherd under a scratch `SHEP_HOME`, with two stand-in
//! runners as sheep and kelpie adopted as its dog, the way the experiments
//! repo's lease dog was tested. A stand-in runner is this test binary run
//! as a sheep: see `stand_in_runner`. The shepherd runs with `HOME` set to
//! its scratch home, which is all an adopted dog inherits, so the dog's
//! book is never the maintainer's.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use shep_client::Client;
use shep_client::shep_core::config::AppConfig;
use shep_client::shep_core::protocol::Request;
use shep_client::shep_core::protocol::request::{ActionOutcome, DogSource, Response, SelectorSpec};
use shep_client::shep_core::status::ProcStatus;
use shep_kelpie::lease::wire::Asker;
use shep_kelpie::lease::{Epoch, LeaseKind};

const KELPIE: &str = env!("CARGO_BIN_EXE_shep-kelpie");
const STAND_IN: &str = "KELPIE_TEST_STAND_IN";

const DOG: &str = "kelpie";

// `KELPIE_TEST_SHEP`, or the pinned shepherd kelpie runs under.
fn shep_binary() -> PathBuf {
    std::env::var_os("KELPIE_TEST_SHEP").map_or_else(
        || Path::new(&std::env::var_os("HOME").unwrap()).join(".kelpie/bin/shep"),
        PathBuf::from,
    )
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
        // No unwrap: a panic here while a failed test unwinds would abort.
        let _ = self.command(&["kill"]).output();
    }
}

impl Shepherd {
    // Two stand-in runners, with kelpie adopted and running as the dog.
    fn start() -> Self {
        let shepherd = Self::runners();
        shepherd.shep_ok(&["adopt", KELPIE, "--name", DOG]);
        shepherd
    }

    // The two stand-in runners alone.
    // Under /tmp: a socket path longer than 104 bytes is refused on macOS.
    fn runners() -> Self {
        let home = tempfile::Builder::new()
            .prefix("kd")
            .tempdir_in("/tmp")
            .unwrap();
        let shepherd = Self { home };
        let flockfile = shepherd.home.path().join("flock.toml");
        std::fs::write(&flockfile, shepherd.flockfile()).unwrap();
        shepherd.shep_ok(&["start", flockfile.to_str().unwrap()]);
        shepherd
    }

    fn flockfile(&self) -> String {
        let me = std::env::current_exe().unwrap();
        let runner = |name: &str| {
            format!(
                "[[app]]\nname = {name:?}\nscript = {me:?}\n\
                 args = [\"--exact\", \"stand_in_runner\", \"--ignored\", \"--nocapture\"]\n\
                 channel = true\nautorestart = false\nenv = {{ {STAND_IN} = \"1\" }}\n\n"
            )
        };
        format!("{}{}", runner("koji"), runner("reactmap"))
    }

    // The book the adopted dog keeps, under the `HOME` the shepherd gives it.
    fn book(&self) -> PathBuf {
        self.home.path().join(".kelpie/dog/book.json")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(shep_binary());
        cmd.args(args)
            .env("SHEP_HOME", self.home.path())
            .env("HOME", self.home.path())
            .stdin(Stdio::null());
        cmd
    }

    fn shep_ok(&self, args: &[&str]) {
        ran(
            &format!("shep {args:?}"),
            &self.command(args).output().unwrap(),
        );
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
    stand_in(&trigger(client, DOG, "status", None).await)
}

fn stand_in(status: &Value) -> Value {
    let leases = status["leases"].as_array().expect("a leases list");
    let line = leases.iter().find(|l| l["kind"] == "stand-in");
    line.cloned()
        .unwrap_or_else(|| panic!("no stand-in line in {status}"))
}

#[tokio::test]
#[ignore = "needs a shepherd at KELPIE_TEST_SHEP or ~/.kelpie/bin/shep"]
async fn the_lease_round_trip_runs_through_a_real_shepherd() {
    let shepherd = Shepherd::start();
    let client = shepherd.client().await;
    until("the dog and both runners", async || {
        trigger(&client, DOG, "status", None).await["leases"].is_array()
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
    // shep hands an adopted dog no TMPDIR, so its lock is under the
    // user's own temp folder, never the scratch home.
    let leases = status["leases"].as_array().unwrap();
    let gpu = leases
        .iter()
        .find(|l| l["kind"] == "gpu")
        .expect("a GPU line");
    let lock = gpu["lock"].as_str().unwrap();
    assert!(lock.ends_with("/qwen-review/gpu.lock"), "{lock}");
    assert_eq!(stand_in(&status)["holder"], json!({ "runner": "koji" }));
    assert!(stand_in(&status)["since"].as_u64().is_some());

    eprintln!("want to grant {round_trip} ms, killed to next grant {reclaim} ms");
}

// A dog that restarts while a runner holds a lease keeps its book: the
// holder keeps the lease, the waiter keeps its place, and a holder whose
// sheep restarted while the dog was down is reclaimed on start.
#[tokio::test]
#[ignore = "needs a shepherd at KELPIE_TEST_SHEP or ~/.kelpie/bin/shep"]
async fn a_restarted_dog_keeps_its_book() {
    let shepherd = Shepherd::start();
    let client = shepherd.client().await;
    let dog_up = async || trigger(&client, DOG, "status", None).await["leases"].is_array();
    until("the dog and both runners", async || {
        dog_up().await
            && trigger(&client, "koji", "status", None).await.is_object()
            && trigger(&client, "reactmap", "status", None)
                .await
                .is_object()
    })
    .await;
    trigger(&client, "koji", "want", Some("stand-in")).await;
    until("koji's grant", async || holds(&client, "koji").await).await;
    trigger(&client, "reactmap", "want", Some("stand-in")).await;
    until("reactmap in the queue", async || {
        book(&client).await["queue"] == json!([{ "runner": "reactmap" }])
    })
    .await;
    let before = book(&client).await;

    shepherd.shep_ok(&["restart", DOG]);
    until("the dog back", dog_up).await;
    assert_eq!(book(&client).await, before, "the book came back whole");
    assert!(holds(&client, "koji").await);
    assert!(!holds(&client, "reactmap").await, "no second grant");

    // koji restarts while the dog is down: its lease goes to reactmap.
    shepherd.shep_ok(&["disable", DOG]);
    shepherd.shep_ok(&["restart", "koji"]);
    shepherd.shep_ok(&["enable", DOG]);
    until("reactmap's grant", async || {
        holds(&client, "reactmap").await
    })
    .await;
    assert_eq!(
        book(&client).await["holder"],
        json!({ "runner": "reactmap" })
    );
    assert_eq!(book(&client).await["queue"], json!([]));
}

// The names in the flock, and whether each runs.
async fn flock(client: &Client) -> Vec<(String, bool)> {
    let Response::Flock(rows) = client.request(Request::ListFlock).await.unwrap() else {
        panic!("the flock listing");
    };
    rows.into_iter()
        .map(|r| (r.name, r.status == ProcStatus::Online))
        .collect()
}

fn ran(what: &str, out: &Output) {
    assert!(
        out.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// A flock set up before the adopted kelpie was the dog: a `kelpie-dog`
// sheep, and the book it kept. The adopted start removes the sheep and
// takes the book as it stands.
#[tokio::test]
#[ignore = "needs a shepherd at KELPIE_TEST_SHEP or ~/.kelpie/bin/shep"]
async fn an_adopted_start_removes_a_left_over_kelpie_dog_and_keeps_its_book() {
    let shepherd = Shepherd::start();
    let client = shepherd.client().await;
    until("the dog", async || {
        trigger(&client, DOG, "status", None).await["leases"].is_array()
    })
    .await;
    ran(
        "lease take",
        &shepherd
            .kelpie(&["lease", "take", "stand-in"])
            .output()
            .unwrap(),
    );
    shepherd.shep_ok(&["disable", DOG]);
    let kept = std::fs::read(shepherd.book()).unwrap();

    let mut left_over = AppConfig::minimal("kelpie-dog", KELPIE);
    left_over.args = vec!["dog".into()];
    left_over.channel = true;
    left_over.autorestart = false;
    let added = client.request(Request::Add {
        apps: vec![left_over],
    });
    assert!(matches!(added.await, Ok(Response::Added(_))));
    assert!(
        flock(&client)
            .await
            .iter()
            .any(|(name, _)| name == "kelpie-dog")
    );

    shepherd.shep_ok(&["enable", DOG]);
    until("the dog", async || {
        trigger(&client, DOG, "status", None).await["leases"].is_array()
    })
    .await;
    let names = flock(&client).await;
    assert!(
        !names.iter().any(|(name, _)| name == "kelpie-dog"),
        "{names:?}"
    );
    assert_eq!(
        std::fs::read(shepherd.book()).unwrap(),
        kept,
        "the book changed"
    );
    assert_eq!(book(&client).await["holder"], "maintainer");
}

// A dog adopted and running: `shep kelpie lease` reaches it by its name.
#[tokio::test]
#[ignore = "needs a shepherd at KELPIE_TEST_SHEP or ~/.kelpie/bin/shep"]
async fn shep_kelpie_lease_take_reaches_the_adopted_dog() {
    let shepherd = Shepherd::start();
    let client = shepherd.client().await;
    until("the dog", async || {
        trigger(&client, DOG, "status", None).await["leases"].is_array()
    })
    .await;
    let take = shepherd
        .kelpie(&["lease", "take", "stand-in"])
        .output()
        .unwrap();
    ran("lease take", &take);
    assert!(String::from_utf8_lossy(&take.stdout).contains("stand-in is yours"));
    assert_eq!(book(&client).await["holder"], "maintainer");
    let names = flock(&client).await;
    assert!(names.contains(&(DOG.to_owned(), true)), "{names:?}");
}

// A shepherd with no dog named `kelpie`, as one still running the dog as
// `kelpie-dog` is: shep refuses the trigger with `NotFound`, and the lease
// commands say the dog is not enabled. A fake shepherd cannot send that.
#[tokio::test]
#[ignore = "needs a shepherd at KELPIE_TEST_SHEP or ~/.kelpie/bin/shep"]
async fn shep_kelpie_lease_take_names_a_dog_that_is_not_enabled() {
    let shepherd = Shepherd::runners();
    let take = shepherd
        .kelpie(&["lease", "take", "stand-in"])
        .output()
        .unwrap();
    assert!(!take.status.success());
    let said = String::from_utf8_lossy(&take.stderr);
    assert!(said.contains("kelpie's dog is not enabled"), "{said}");
}

// Adopted with no arguments, kelpie runs the dog; before it held the
// leases it exited 2 on every start.
#[tokio::test]
#[ignore = "needs a shepherd at KELPIE_TEST_SHEP or ~/.kelpie/bin/shep"]
async fn an_adopted_start_runs_the_dog_instead_of_exiting() {
    let shepherd = Shepherd::start();
    let client = shepherd.client().await;
    until("the dog", async || {
        trigger(&client, DOG, "status", None).await["leases"].is_array()
    })
    .await;
    let rows = match client.request(Request::ListFlock).await.unwrap() {
        Response::Flock(rows) => rows,
        other => panic!("{other:?}"),
    };
    let dog = rows.iter().find(|r| r.name == DOG).expect("the dog's row");
    assert_eq!(dog.status, ProcStatus::Online);
    assert_eq!(dog.restarts, 0, "the dog restarted");
    assert!(
        matches!(dog.dog, Some(DogSource::Adopted { channel: true, .. })),
        "{:?}",
        dog.dog
    );
}

// The `--version` answer shep reads at `shep adopt` asks for the channel.
#[test]
fn kelpie_s_version_answer_asks_for_the_shepherd_channel() {
    let out = Command::new(KELPIE).arg("--version").output().unwrap();
    ran("--version", &out);
    let answer = String::from_utf8(out.stdout).unwrap();
    let parsed = shep_client::shep_core::dogs::parse_version_answer(&answer).unwrap();
    assert!(parsed.channel, "{answer}");
    assert_eq!(parsed.version, env!("CARGO_PKG_VERSION"));
}
