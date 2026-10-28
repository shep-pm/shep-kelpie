//! The verbs that reach a runner on its own socket, against a real runner
//! served in a folder short enough to bind, and a shepherd that records what
//! it is asked

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use shep_client::shep_core::protocol::Request;
use std::collections::VecDeque;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::*;
use crate::home::socket_of;
use crate::ipc::Listener;
use crate::runner::{Handles, Limits, Posted, Runner, Serving, answer, serve};
use crate::test::{Rig, short_tempdir};

// A shepherd answer no verb on the socket may take
const THE_SHEPHERD: &str = r#"{"error":"the shepherd was asked"}"#;

// A real runner for project `koji` serving its socket under a short kelpie home
struct Served {
    // Kept so the home outlives the socket
    _home: TempDir,
    home: PathBuf,
    _rig: Rig,
    runner: Arc<Mutex<Runner>>,
    postings: Arc<Posted>,
    serving: Option<Serving>,
    _woken: Receiver<()>,
}

impl Drop for Served {
    fn drop(&mut self) {
        if let Some(serving) = self.serving.take() {
            serving.end();
        }
    }
}

// A runner with a ruling to answer and a project manager to tell
fn served() -> Served {
    let (rig, runner, _) = Rig::parked_set("koji", |rig| {
        rig.edit_settings(|s| {
            let listed = "\nimplementers = [\"sonnet-high\"]\n";
            assert!(s.contains(listed), "the example's implementers moved");
            s.replace(listed, "\nimplementers = [\"sonnet-high\"]\npm = \"pm\"\n")
        });
    });
    let runner = Arc::new(runner);
    let postings = runner.lock().unwrap().postings();
    let home = short_tempdir();
    let (wake, woken) = mpsc::channel();
    let handles = Handles {
        runner: Arc::clone(&runner),
        postings: Arc::clone(&postings),
        wake,
    };
    let socket = socket_of(home.path(), "koji");
    let serving = serve(&socket, handles, Limits::default()).unwrap();
    Served {
        home: home.path().to_owned(),
        _home: home,
        _rig: rig,
        runner,
        postings,
        serving: Some(serving),
        _woken: woken,
    }
}

// A reach that keeps what it says
fn reaching(home: &Path) -> (Reach, Arc<Mutex<Vec<String>>>) {
    let said = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&said);
    let reach = Reach::saying(home, move |line| kept.lock().unwrap().push(line.to_owned()));
    (reach, said)
}

fn triggers(shepherd: &mut FakeShepherd) -> usize {
    (shepherd.writes().iter())
        .filter(|w| matches!(w, Request::Trigger { .. }))
        .count()
}

// A shepherd with `koji` up that answers every trigger with `THE_SHEPHERD` first
fn shepherd_for_koji() -> FakeShepherd {
    let shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    shepherd.says("koji", &[THE_SHEPHERD, THE_SHEPHERD]);
    shepherd
}

#[tokio::test]
async fn status_rule_board_and_tell_work_with_the_shepherds_trigger_path_unused() {
    let mut shepherd = shepherd_for_koji();
    let served = served();
    let (reach, said) = reaching(&served.home);
    let client = client(&shepherd).await;
    let koji = project("koji");
    let before = shepherd.answers_left("koji");

    let status = in_time(status(&reach, &client, Format::Plain))
        .await
        .unwrap();
    assert!(status[0].starts_with("koji"), "{status:?}");
    assert!(!status.join("\n").contains("not running"), "{status:?}");

    let answers = shepherd.scratch("answers");
    let ruled = in_time(rule(&reach, &client, &koji, "71 yes", &answers)).await;
    assert_eq!(ruled, Ok(vec!["ruling 71 on koji: yes".to_owned()]));

    let board = in_time(send(&reach, &client, &koji, "board", None))
        .await
        .unwrap();
    let [board] = board.as_slice() else {
        panic!("{board:?}")
    };
    let board: Value = serde_json::from_str(board).unwrap();
    assert!(board.get("error").is_none(), "{board}");

    let told = in_time(tell(&reach, &client, &koji, "how is #7?", Format::Plain)).await;
    assert_eq!(
        told,
        Ok(vec![
            "told koji's project manager, which reads it on its next wake".to_owned()
        ])
    );

    assert_eq!(
        triggers(&mut shepherd),
        0,
        "no trigger went through the shepherd"
    );
    assert_eq!(shepherd.answers_left("koji"), before);
    assert_eq!(said.lock().unwrap().as_slice(), [] as [String; 0]);
    drop(served);
}

// A runner's socket that says hello and then answers each request with the next
// of `script`, where `None` is a request it takes and never answers
fn scripted(home: &Path, script: Vec<Option<String>>) -> TempSocket {
    let socket = socket_of(home, "koji");
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let mut listener = Listener::bind(&socket).unwrap();
    let script = Arc::new(Mutex::new(VecDeque::from(script)));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (seen, told) = (Arc::clone(&script), Arc::clone(&asked));
    tokio::spawn(async move {
        while let Ok(stream) = listener.accept().await {
            let (script, asked) = (Arc::clone(&seen), Arc::clone(&told));
            tokio::spawn(async move {
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                stream
                    .get_mut()
                    .write_all(b"{\"protocol\":1}\n")
                    .await
                    .unwrap();
                loop {
                    line.clear();
                    if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    asked.lock().unwrap().push(line.trim_end().to_owned());
                    let next = script.lock().unwrap().pop_front();
                    if let Some(Some(body)) = next {
                        stream
                            .get_mut()
                            .write_all(format!("{body}\n").as_bytes())
                            .await
                            .unwrap();
                    }
                }
            });
        }
    });
    TempSocket { asked }
}

// What a scripted socket was asked
struct TempSocket {
    asked: Arc<Mutex<Vec<String>>>,
}

// What the verbs print, on whichever path `reach` takes
async fn printed(shepherd: &FakeShepherd, reach: &Reach, answers: &Path) -> Vec<Vec<String>> {
    let client = client(shepherd).await;
    let koji = project("koji");
    let mut out = Vec::new();
    for format in [Format::Plain, Format::Json] {
        out.push(in_time(status(reach, &client, format)).await.unwrap());
        out.push(
            in_time(tell(reach, &client, &koji, "how is #7?", format))
                .await
                .unwrap(),
        );
    }
    out.push(
        in_time(send(reach, &client, &koji, "board", None))
            .await
            .unwrap(),
    );
    out.push(
        in_time(send(reach, &client, &koji, "timings", Some("5")))
            .await
            .unwrap(),
    );
    let refused = in_time(change(
        reach,
        &client,
        &koji,
        "drop",
        Some("9"),
        Format::Plain,
    ))
    .await;
    out.push(vec![refused.unwrap_err()]);
    out.push(
        in_time(rule(reach, &client, &koji, "71 no not yet", answers))
            .await
            .unwrap(),
    );
    out
}

#[tokio::test]
async fn without_a_socket_the_verbs_fall_back_to_shep_trigger_and_say_so_once() {
    // The bodies a real runner gives, in the order `printed` asks for them
    let served = served();
    let asks = [
        ("status", None),
        ("tell", Some("how is #7?")),
        ("status", None),
        ("tell", Some("how is #7?")),
        ("board", None),
        ("timings", Some("5")),
        ("drop", Some("9")),
        ("rule", Some("71 no not yet")),
    ];
    let bodies: Vec<String> = (asks.iter())
        .map(|(action, params)| answer(&served.runner, &served.postings, action, *params))
        .collect();
    drop(served);

    // A socket that gives those bodies
    let home = short_tempdir();
    let socket = scripted(home.path(), bodies.iter().cloned().map(Some).collect());
    let shepherd = shepherd_for_koji();
    let answers = shepherd.scratch("answers");
    let (reach, said) = reaching(home.path());
    let over_socket = printed(&shepherd, &reach, &answers).await;
    assert_eq!(socket.asked.lock().unwrap().len(), asks.len());
    assert!(said.lock().unwrap().is_empty());

    // No socket: the shepherd gives the same bodies
    let mut shepherd = FakeShepherd::new();
    runner(&shepherd, "koji", Path::new("/src/koji"), true);
    let bodies: Vec<&str> = bodies.iter().map(String::as_str).collect();
    shepherd.says("koji", &bodies);
    let nothing = short_tempdir();
    let (reach, said) = reaching(nothing.path());
    let answers = shepherd.scratch("answers");
    let over_trigger = printed(&shepherd, &reach, &answers).await;

    assert_eq!(over_trigger, over_socket);
    assert_eq!(triggers(&mut shepherd), asks.len());
    let said = said.lock().unwrap();
    let [line] = said.as_slice() else {
        panic!("one line a command: {said:?}")
    };
    let path = socket_of(nothing.path(), "koji");
    assert!(line.contains(&path.display().to_string()), "{line}");
}

#[tokio::test]
async fn a_timeout_on_the_socket_does_not_send_the_trigger_again() {
    let home = short_tempdir();
    let socket = scripted(home.path(), vec![None]);
    let mut shepherd = shepherd_for_koji();
    let (reach, said) = reaching(home.path());
    let reach = reach.waiting(Duration::from_millis(200));
    let client = client(&shepherd).await;

    let ran = in_time(change(
        &reach,
        &client,
        &project("koji"),
        "drop",
        Some("713"),
        Format::Plain,
    ))
    .await;
    let lines = ran.expect("a late answer is no error to retry");
    assert!(lines[0].contains("most likely been taken"), "{lines:?}");
    assert_eq!(socket.asked.lock().unwrap().len(), 1);
    assert_eq!(
        triggers(&mut shepherd),
        0,
        "the shepherd was not asked as well"
    );
    assert!(said.lock().unwrap().is_empty(), "a timeout is no fallback");
}

#[tokio::test]
async fn a_note_longer_than_a_frame_is_named_so_and_nothing_is_sent() {
    let home = short_tempdir();
    let socket = scripted(home.path(), vec![]);
    let mut shepherd = shepherd_for_koji();
    let (reach, said) = reaching(home.path());
    let client = client(&shepherd).await;
    let note = "x".repeat(70 * 1024);

    let ran = in_time(tell(
        &reach,
        &client,
        &project("koji"),
        &note,
        Format::Plain,
    ))
    .await;
    let why = ran.expect_err("a frame has a bound");
    assert!(why.contains("a frame is at most"), "{why}");
    assert!(!why.contains("socket broke"), "{why}");
    assert!(socket.asked.lock().unwrap().is_empty());
    assert_eq!(triggers(&mut shepherd), 0);
    assert!(said.lock().unwrap().is_empty(), "no fallback");
}
