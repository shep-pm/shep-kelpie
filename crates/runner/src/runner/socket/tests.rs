use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::runner::trigger::tests::fixtures;
use crate::runner::{answer, request_of, step};
use crate::test::{Rig, short_tempdir};

// Long enough for a thread to answer, short enough that a hang fails by name
const PATIENCE: Duration = Duration::from_secs(10);

// Short bounds, for the tests of what a silent connection does
const QUICK: Limits = Limits {
    hello_within: Duration::from_millis(300),
    idle_within: Duration::from_millis(300),
    most_connections: MOST_CONNECTIONS,
};

// A runner serving a socket under a short folder, with what the loop would hold
struct Served {
    // Kept so the folder outlives the socket
    _dir: TempDir,
    socket: PathBuf,
    runner: Arc<Mutex<Runner>>,
    postings: Arc<Posted>,
    woken: Receiver<()>,
    serving: Option<Serving>,
}

impl Drop for Served {
    fn drop(&mut self) {
        if let Some(serving) = self.serving.take() {
            serving.end();
        }
    }
}

// The frame a client writes for `action` and `params`, as `shep trigger` would read them
fn frame(action: &str, params: Option<&str>) -> String {
    let request = request_of(action, params).expect("the test asks a request that reads");
    shep_kelpie_protocol::frame::encode(&request)
        .unwrap()
        .trim_end()
        .to_owned()
}

// The socket's path: short, so it binds under a worker's long `TMPDIR`
fn socket_in(dir: &TempDir) -> PathBuf {
    dir.path().join("run").join("koji.sock")
}

fn handles(runner: Mutex<Runner>) -> (Handles, Receiver<()>) {
    let runner = Arc::new(runner);
    let postings = runner.lock().unwrap().postings();
    let (wake, woken) = mpsc::channel();
    (
        Handles {
            runner,
            postings,
            wake,
        },
        woken,
    )
}

fn served_with(runner: Mutex<Runner>, limits: Limits) -> Served {
    let dir = short_tempdir();
    let socket = socket_in(&dir);
    let (handles, woken) = handles(runner);
    let serving = serve(&socket, handles.clone(), limits).unwrap();
    Served {
        _dir: dir,
        socket,
        runner: handles.runner,
        postings: handles.postings,
        woken,
        serving: Some(serving),
    }
}

// A runner whose loop has posted its status, as it has by its first pass
fn served(rig: &Rig) -> Served {
    let runner = rig.open().unwrap();
    rig.ask(&runner, "status", None);
    served_with(runner, Limits::default())
}

// One connection, as a client's side of the protocol
struct Peer {
    write: Stream,
    read: BufReader<Stream>,
}

impl Peer {
    fn connect(socket: &Path) -> Self {
        let write = Stream::connect(socket).unwrap();
        write.set_read_timeout(Some(PATIENCE)).unwrap();
        let read = BufReader::new(write.try_clone().unwrap());
        Self { write, read }
    }

    // Connected, and past the hello
    fn hello(socket: &Path) -> Self {
        let mut peer = Self::connect(socket);
        let hello = peer
            .ask(r#"{"protocol":1}"#)
            .expect("the runner answers a hello");
        assert_eq!(hello, json!({ "protocol": PROTOCOL }));
        peer
    }

    fn send(&mut self, line: &str) {
        self.write.write_all(line.as_bytes()).unwrap();
        self.write.write_all(b"\n").unwrap();
    }

    // The next line, or `None` where the runner closed the connection
    fn line(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.read.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(serde_json::from_str(&line).expect("a line is JSON")),
        }
    }

    fn ask(&mut self, line: &str) -> Option<Value> {
        self.send(line);
        self.line()
    }

    // Connected, past the hello, and watching
    fn watching(socket: &Path) -> Self {
        let mut peer = Self::hello(socket);
        let watch = shep_kelpie_protocol::frame::encode(&Request::Watch).unwrap();
        peer.send(watch.trim_end());
        peer
    }

    // The status of the next event
    #[track_caller]
    fn status(&mut self) -> Value {
        let event = self.line().expect("an event");
        assert_eq!(event["event"], "status", "{event}");
        event["status"].clone()
    }

    // Whether the runner closed the connection, and did not just fall silent
    fn closed(&mut self) -> bool {
        let mut line = String::new();
        loop {
            line.clear();
            match self.read.read_line(&mut line) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
    }
}

// Waits for `done`, which a thread of the runner's makes true
fn until(what: &str, done: impl Fn() -> bool) {
    let start = std::time::Instant::now();
    while !done() {
        assert!(start.elapsed() < PATIENCE, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
#[test]
fn the_socket_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let rig = Rig::new("koji");
    let served = served(&rig);
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&served.socket), 0o600);
    assert_eq!(mode(served.socket.parent().unwrap()), 0o700);

    // A `run/` made wider by something else is narrowed
    let dir = short_tempdir();
    let socket = socket_in(&dir);
    std::fs::create_dir(socket.parent().unwrap()).unwrap();
    std::fs::set_permissions(socket.parent().unwrap(), PermissionsExt::from_mode(0o755)).unwrap();
    let (handles, _woken) = handles(Rig::new("koji").open().unwrap());
    let serving = serve(&socket, handles, Limits::default()).unwrap();
    assert_eq!(mode(socket.parent().unwrap()), 0o700);
    serving.end();
}

// The other half of the test below: binds the socket and exits without removing it
#[test]
#[ignore = "run by the test that starts it"]
fn binder() {
    let Some(socket) = std::env::var_os("KELPIE_TEST_SOCKET") else {
        return;
    };
    let _left = Listener::bind(PathBuf::from(socket)).unwrap();
    std::process::exit(0);
}

#[cfg(unix)]
#[test]
fn a_dead_runners_socket_is_replaced_and_a_live_ones_is_not() {
    let dir = short_tempdir();
    let socket = socket_in(&dir);
    std::fs::create_dir(socket.parent().unwrap()).unwrap();
    // A socket file nothing listens at, as a runner that was killed leaves. A
    // child binds it and exits, so no fork of this process can hold the listener.
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "runner::socket::tests::binder", "--ignored"])
        .env("KELPIE_TEST_SOCKET", &socket)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(socket.exists());
    assert!(Stream::connect(&socket).is_err(), "nothing listens there");

    let rig = Rig::new("koji");
    let (handles, _woken) = handles(rig.open().unwrap());
    let serving = serve(&socket, handles.clone(), Limits::default()).unwrap();
    assert_eq!(
        Peer::hello(&socket).ask(r#"{"action":"status"}"#).unwrap()["project"],
        "koji",
        "the new runner answers where the dead one's socket was"
    );

    let error = serve(&socket, handles, Limits::default()).unwrap_err();
    assert!(error.contains("another runner answers"), "{error}");
    assert_eq!(
        Peer::hello(&socket).ask(r#"{"action":"status"}"#).unwrap()["project"],
        "koji",
        "the live runner still answers"
    );
    serving.end();
    assert!(
        !socket.exists(),
        "a runner that ends takes its socket with it"
    );
}

#[cfg(unix)]
#[test]
fn a_path_too_long_is_refused_before_the_bind_and_the_runner_goes_on() {
    let dir = short_tempdir();
    let socket = dir
        .path()
        .join("run")
        .join(format!("{}.sock", "k".repeat(100)));
    let rig = Rig::new("koji");
    let (handles, _woken) = handles(rig.open().unwrap());

    let error = serve(&socket, handles.clone(), Limits::default()).unwrap_err();
    assert!(error.contains("longer than"), "{error}");
    assert!(
        !socket.parent().unwrap().exists(),
        "nothing was made for it"
    );
    assert!(start(&socket, handles.clone()).is_none());

    // The runner is the same runner, answering the shepherd's trigger as before
    let reply = answer(&handles.runner, &handles.postings, "status", None);
    assert_eq!(
        serde_json::from_str::<Value>(&reply).unwrap()["project"],
        "koji"
    );
}

#[test]
fn the_hello_is_answered_with_the_protocol() {
    let rig = Rig::new("koji");
    let served = served(&rig);
    // A client on a later protocol is answered with the runner's own
    let mut peer = Peer::connect(&served.socket);
    assert_eq!(
        peer.ask(r#"{"protocol":7}"#),
        Some(json!({ "protocol": PROTOCOL }))
    );
    // A first line that is no hello ends the connection
    let mut peer = Peer::connect(&served.socket);
    assert_eq!(peer.ask(r#"{"action":"status"}"#), None);
}

#[test]
fn an_unknown_request_gets_a_typed_error_and_the_connection_goes_on() {
    let rig = Rig::new("koji");
    let served = served(&rig);
    let mut peer = Peer::hello(&served.socket);

    let unknown = peer.ask(r#"{"action":"subscribe","since":12}"#).unwrap();
    assert_eq!(
        unknown,
        json!({ "request": "subscribe", "version": Build::this().kelpie })
    );
    // A request it has, with params that do not read, is a refusal and not "unknown"
    let refused = peer.ask(r#"{"action":"add","params":"x"}"#).unwrap();
    assert!(
        refused["error"]
            .as_str()
            .is_some_and(|e| e.contains("does not read")),
        "{refused}"
    );
    let status = peer.ask(r#"{"action":"status"}"#).unwrap();
    assert_eq!(status["project"], "koji", "the connection stayed open");
}

// The socket's answer to `line`, and `shep trigger`'s to `action`, each marked out as the fixtures are
fn both(served: &Served, action: &str, head: &str) -> (Value, Value) {
    let mut over_socket = Peer::hello(&served.socket)
        .ask(&frame(action, None))
        .unwrap();
    fixtures::settled(&mut over_socket, head);
    let body = answer(&served.runner, &served.postings, action, None);
    let mut over_trigger: Value = serde_json::from_str(&body).unwrap();
    fixtures::settled(&mut over_trigger, head);
    (over_socket, over_trigger)
}

#[test]
fn every_fixture_action_answers_the_json_shep_trigger_answers() {
    // `status`: the fixture was recorded after a `drain` and a `finish`
    let (rig, runner, head) = Rig::parked_set("koji", fixtures::rich);
    let runner = Arc::new(runner);
    let postings = runner.lock().unwrap().postings();
    answer(&runner, &postings, "drain", None);
    answer(&runner, &postings, "finish", None);
    let (handles, _woken) = handles_of(&runner, &postings);
    let dir = short_tempdir();
    let socket = socket_in(&dir);
    let serving = serve(&socket, handles, Limits::default()).unwrap();
    let mut over_socket = Peer::hello(&socket).ask(r#"{"action":"status"}"#).unwrap();
    fixtures::settled(&mut over_socket, &head);
    assert_eq!(over_socket, fixtures::status(), "status");
    serving.end();
    drop(rig);

    // `timings`, over the three work items the fixture's history lists
    let rig = Rig::new("koji");
    fixtures::seed(&rig, 3);
    let served = served(&rig);
    let (over_socket, over_trigger) = both(&served, "timings", "");
    assert_eq!(over_socket, fixtures::timings(), "timings");
    assert_eq!(over_socket, over_trigger, "timings");

    // `board`, with two ready issues an open one blocks
    let rig = Rig::new("koji");
    fixtures::ready(&rig);
    let runner = rig.open().unwrap();
    step(&runner).unwrap();
    let served = served_with(runner, Limits::default());
    let (over_socket, over_trigger) = both(&served, "board", "");
    assert_eq!(over_socket, fixtures::board(), "board");
    assert_eq!(over_socket, over_trigger, "board");

    // A refusal, which is the one answer every request can give
    let (rig, runner, _) = Rig::with_pull_request("rotom");
    let served = served_with(runner, Limits::default());
    let (over_socket, over_trigger) = both(&served, "gate", "");
    assert_eq!(over_socket, fixtures::refusal(), "refusal");
    assert_eq!(over_socket, over_trigger, "refusal");
    drop(rig);
}

fn handles_of(runner: &Arc<Mutex<Runner>>, postings: &Arc<Posted>) -> (Handles, Receiver<()>) {
    let (wake, woken) = mpsc::channel();
    (
        Handles {
            runner: Arc::clone(runner),
            postings: Arc::clone(postings),
            wake,
        },
        woken,
    )
}

#[test]
fn status_is_answered_while_a_pass_holds_the_lock() {
    let rig = Rig::new("koji");
    let served = served(&rig);
    let _pass = served.runner.lock().unwrap();
    let mut peer = Peer::hello(&served.socket);
    let status = peer
        .ask(r#"{"action":"status"}"#)
        .expect("status waited for the pass's lock");
    assert_eq!(status["project"], "koji");
}

#[test]
fn a_request_wakes_the_loop_as_a_trigger_does() {
    let rig = Rig::new("koji");
    let served = served(&rig);
    let mut peer = Peer::hello(&served.socket);

    served.postings.needs_wake();
    peer.ask(r#"{"action":"status"}"#).unwrap();
    assert!(
        served.woken.try_recv().is_err(),
        "a status asks nothing of the loop"
    );

    let added = peer.ask(&frame("add", Some("7"))).unwrap();
    assert_eq!(added["work_item"]["issue"], "7", "{added}");
    assert_eq!(served.woken.recv_timeout(PATIENCE), Ok(()));
}

#[test]
fn an_oversized_line_and_a_silent_connection_are_closed() {
    let rig = Rig::new("koji");
    let served = served_with(rig.open().unwrap(), QUICK);

    // A line past the bound closes the connection
    let mut peer = Peer::hello(&served.socket);
    let too_long = "x".repeat(LONGEST_FRAME + 10);
    peer.send(&too_long);
    assert_eq!(peer.line(), None);

    // A connection that never says hello is closed
    let mut silent = Peer::connect(&served.socket);
    assert_eq!(silent.line(), None);

    // So is one that says hello and then nothing
    let mut idle = Peer::hello(&served.socket);
    assert_eq!(idle.line(), None);

    // A connection that does ask is not closed by those
    let mut busy = Peer::hello(&served.socket);
    assert_eq!(
        busy.ask(r#"{"action":"status"}"#).unwrap()["project"],
        "koji"
    );
}

#[test]
fn a_watcher_gets_the_current_status_first() {
    let rig = Rig::new("koji");
    let served = served(&rig);
    served.postings.needs_wake();
    let mut watcher = Peer::watching(&served.socket);

    let status = watcher.status();
    assert_eq!(status["project"], "koji");
    assert_eq!(status["work_items"], json!([]));
    assert!(
        served.woken.try_recv().is_err(),
        "a watch asks nothing of the loop"
    );
    assert!(
        request_of("watch", None).is_err(),
        "`shep trigger` cannot send it"
    );
}

#[test]
fn a_watcher_sees_a_change_without_polling() {
    let rig = Rig::new("koji");
    let served = served(&rig);
    let mut watcher = Peer::watching(&served.socket);
    assert_eq!(watcher.status()["work_items"], json!([]));

    rig.ask(&served.runner, "add", Some("7"));

    let status = watcher.status();
    assert_eq!(status["work_items"][0]["issue"], "7", "{status}");
}

#[test]
fn two_posts_with_nothing_changed_give_one_event() {
    let rig = Rig::new("koji");
    let served = served(&rig);
    // The answer that `served` asked posted before its lock hold was timed, so post again
    served.runner.lock().unwrap().post();
    let mut watcher = Peer::watching(&served.socket);
    let first = watcher.status();

    // The rig's clock stands still, so a post with no change builds the same status
    {
        let runner = served.runner.lock().unwrap();
        runner.post();
        runner.post();
    }

    // The next event is the change that follows, not a repeat
    served.postings.set_draining(true);
    let next = watcher.status();
    assert_eq!(first["draining"], Value::Null);
    assert!(next["draining"].is_object(), "{next}");
}

#[test]
fn drain_and_undrain_reach_a_watcher_while_a_pass_holds_the_lock() {
    let rig = Rig::new("koji");
    let served = served(&rig);
    let mut watcher = Peer::watching(&served.socket);
    assert_eq!(watcher.status()["draining"], Value::Null);

    let _pass = served.runner.lock().unwrap();
    let mut asker = Peer::hello(&served.socket);
    asker.ask(&frame("drain", None)).expect("a drain answers");
    assert!(watcher.status()["draining"].is_object());

    asker
        .ask(&frame("undrain", None))
        .expect("an undrain answers");
    assert_eq!(watcher.status()["draining"], Value::Null);
}

#[test]
fn a_slow_watcher_skips_snapshots_and_does_not_hold_up_a_post() {
    let rig = Rig::new("koji");
    // A title far past what a socket buffers, so a write to a client that
    // reads nothing blocks and the snapshots after it have to be skipped
    rig.forge
        .open_issue(7, &"x".repeat(4 * 1024 * 1024), "Body.\n");
    let served = served_with(rig.open().unwrap(), Limits::default());
    rig.ask(&served.runner, "add", Some("7"));
    let mut slow = Peer::watching(&served.socket);
    until("the watcher registered", || served.postings.watched());

    const TOGGLES: usize = 6;
    for i in 0..TOGGLES {
        let action = if i % 2 == 0 { "drain" } else { "undrain" };
        rig.ask(&served.runner, action, None);
    }
    rig.ask(&served.runner, "drain", None);

    // It reads now: whatever it was written first, and then where things ended
    let mut last = slow.status();
    let mut events = 1;
    slow.read
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    while let Some(event) = slow.line() {
        last = event["status"].clone();
        events += 1;
    }
    assert!(last["draining"].is_object(), "it ended up draining");
    assert!(
        events < TOGGLES + 2,
        "{events} events for {} changes",
        TOGGLES + 1
    );
}

#[test]
fn a_watcher_that_goes_away_frees_its_thread() {
    let rig = Rig::new("koji");
    let limits = Limits {
        most_connections: 1,
        ..Limits::default()
    };
    let served = served_with(rig.open().unwrap(), limits);
    rig.ask(&served.runner, "status", None);
    let mut watcher = Peer::watching(&served.socket);
    watcher.status();
    assert!(served.postings.watched());

    drop(watcher);
    until("the watcher ended", || !served.postings.watched());
    until("its connection was freed", || {
        Peer::connect(&served.socket)
            .ask(r#"{"protocol":1}"#)
            .is_some()
    });
}

#[test]
fn shutdown_ends_every_watcher() {
    let rig = Rig::new("koji");
    let mut served = served(&rig);
    let mut watchers = [
        Peer::watching(&served.socket),
        Peer::watching(&served.socket),
    ];
    for watcher in &mut watchers {
        watcher.status();
    }

    served.serving.take().expect("serving").end();

    for watcher in &mut watchers {
        assert!(watcher.closed(), "the runner left a watcher open");
    }
    until("the watchers ended", || !served.postings.watched());
}
