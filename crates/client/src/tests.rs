use std::path::PathBuf;
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use shep_kelpie_core::ipc::Listener;
use shep_kelpie_test_support::short_tempdir;

use super::*;

const SOON: Duration = Duration::from_millis(150);
const PATIENCE: Duration = Duration::from_secs(10);

// What a fake runner does with each request line after the hello
enum Script {
    // Answers every request with this line
    Answers(String),
    // Reads requests and answers none
    Silent,
    // Answers the first request with these lines and goes on reading
    Streams(Vec<String>),
    // Answers the first request with these lines and closes
    Closes(Vec<String>),
}

// A fake runner at a short socket, answering `hello` first, and the lines it was asked
struct Fake {
    _dir: TempDir,
    socket: PathBuf,
    asked: UnboundedReceiver<String>,
}

fn runner(hello: &'static str, script: Script) -> Fake {
    let dir = short_tempdir();
    let socket = dir.path().join("k.sock");
    let mut listener = Listener::bind(&socket).unwrap();
    let (asked_tx, asked) = unbounded_channel();
    tokio::spawn(async move {
        let mut stream = BufReader::new(listener.accept().await.unwrap());
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        stream
            .get_mut()
            .write_all(format!("{hello}\n").as_bytes())
            .await
            .unwrap();
        loop {
            line.clear();
            if stream.read_line(&mut line).await.unwrap() == 0 {
                return;
            }
            let _ = asked_tx.send(line.trim_end().to_owned());
            match &script {
                Script::Answers(reply) => stream
                    .get_mut()
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap(),
                Script::Streams(lines) | Script::Closes(lines) => {
                    for line in lines {
                        stream
                            .get_mut()
                            .write_all(format!("{line}\n").as_bytes())
                            .await
                            .unwrap();
                    }
                    if matches!(script, Script::Closes(_)) {
                        return;
                    }
                }
                Script::Silent => {}
            }
        }
    });
    Fake {
        _dir: dir,
        socket,
        asked,
    }
}

#[tokio::test]
async fn a_missing_socket_is_unreachable() {
    let dir = short_tempdir();
    let missing = dir.path().join("none.sock");
    let error = connect(&missing, PATIENCE).await.unwrap_err();
    assert!(!error.why.is_empty());
}

#[tokio::test]
async fn a_runner_on_an_older_protocol_is_refused() {
    let fake = runner(r#"{"protocol":0}"#, Script::Silent);
    let error = connect(&fake.socket, PATIENCE).await.unwrap_err();
    assert!(error.why.contains("protocol 0"), "{error}");
}

#[tokio::test]
async fn a_slow_runner_times_out_and_is_not_asked_again() {
    let mut fake = runner(r#"{"protocol":1}"#, Script::Silent);
    let mut connection = connect(&fake.socket, PATIENCE).await.unwrap();

    assert_eq!(
        connection.ask(&Request::Status, SOON).await,
        Err(Failure::TimedOut)
    );
    let second = connection.ask(&Request::Status, PATIENCE).await;
    assert!(matches!(second, Err(Failure::Broken(_))), "{second:?}");

    drop(connection);
    assert_eq!(
        fake.asked.recv().await.as_deref(),
        Some(r#"{"action":"status"}"#)
    );
    assert_eq!(fake.asked.recv().await, None, "the second ask sent nothing");
}

#[tokio::test]
async fn a_refusal_comes_back_as_a_refusal() {
    let fake = runner(
        r#"{"protocol":1}"#,
        Script::Answers(r#"{"error":"no work item for #7 is open"}"#.into()),
    );
    let mut connection = connect(&fake.socket, PATIENCE).await.unwrap();
    assert_eq!(
        connection.ask(&Request::Gate(None), PATIENCE).await,
        Ok(Reply::Refused("no work item for #7 is open".into()))
    );
}

#[tokio::test]
async fn a_request_the_runner_does_not_have_is_named() {
    let fake = runner(
        r#"{"protocol":1}"#,
        Script::Answers(r#"{"request":"status","version":"1.2.3"}"#.into()),
    );
    let mut connection = connect(&fake.socket, PATIENCE).await.unwrap();
    assert_eq!(
        connection.ask(&Request::Status, PATIENCE).await,
        Err(Failure::Unknown(UnknownRequest {
            request: "status".into(),
            version: "1.2.3".into(),
        }))
    );
}

#[tokio::test]
async fn a_reply_past_a_frame_is_read_whole() {
    let long = "x".repeat(LONGEST_FRAME * 2);
    let fake = runner(
        r#"{"protocol":1}"#,
        Script::Answers(format!(r#"{{"error":"{long}"}}"#)),
    );
    let mut connection = connect(&fake.socket, PATIENCE).await.unwrap();
    assert_eq!(
        connection.ask(&Request::Gate(None), PATIENCE).await,
        Ok(Reply::Refused(long))
    );
}

#[tokio::test]
async fn a_request_past_a_frame_is_refused_before_it_is_sent() {
    let mut fake = runner(r#"{"protocol":1}"#, Script::Silent);
    let mut connection = connect(&fake.socket, PATIENCE).await.unwrap();
    let long = Request::Tell("x".repeat(LONGEST_FRAME));
    let asked = connection.ask(&long, PATIENCE).await;
    assert!(matches!(asked, Err(Failure::TooLong { .. })), "{asked:?}");

    drop(connection);
    assert_eq!(fake.asked.recv().await, None, "nothing was sent");
}

// The recorded status of protocol, as the event line of project `project`
fn status_event(project: &str) -> String {
    let mut status: Value =
        serde_json::from_str(include_str!("../../protocol/fixtures/status.json")).unwrap();
    status["project"] = project.into();
    serde_json::json!({"event": "status", "status": status}).to_string()
}

#[tokio::test]
async fn a_runner_that_does_not_know_watch_is_named_as_too_old() {
    let mut fake = runner(
        r#"{"protocol":1}"#,
        Script::Answers(r#"{"request":"watch","version":"1.2.3"}"#.into()),
    );
    let error = watch(&fake.socket, PATIENCE).await.unwrap_err();
    assert_eq!(
        error,
        WatchError::Failed(Failure::Unknown(UnknownRequest {
            request: "watch".into(),
            version: "1.2.3".into(),
        }))
    );
    assert_eq!(
        error.to_string(),
        "this runner (1.2.3) has no `watch` request: upgrade kelpie"
    );
    assert_eq!(
        fake.asked.recv().await.as_deref(),
        Some(r#"{"action":"watch"}"#)
    );
}

#[tokio::test]
async fn watch_yields_each_changed_status_in_order() {
    let fake = runner(
        r#"{"protocol":1}"#,
        Script::Streams(vec![
            status_event("first"),
            status_event("second"),
            status_event("third"),
        ]),
    );
    let mut watching = watch(&fake.socket, PATIENCE).await.unwrap();
    for want in ["first", "second", "third"] {
        let status = watching.next().await.unwrap().expect("a status");
        assert_eq!(status.project, want);
    }
}

#[tokio::test]
async fn a_watch_ends_when_the_runner_closes_it() {
    let fake = runner(
        r#"{"protocol":1}"#,
        Script::Closes(vec![status_event("only")]),
    );
    let mut watching = watch(&fake.socket, PATIENCE).await.unwrap();
    assert!(watching.next().await.unwrap().is_some());
    assert_eq!(watching.next().await, Ok(None));
}

#[tokio::test]
async fn a_line_that_is_no_status_breaks_the_watch() {
    let fake = runner(
        r#"{"protocol":1}"#,
        Script::Streams(vec![status_event("first"), r#"{"event":"gone"}"#.into()]),
    );
    let mut watching = watch(&fake.socket, PATIENCE).await.unwrap();
    assert!(watching.next().await.unwrap().is_some());
    let broken = watching.next().await;
    assert!(matches!(broken, Err(Failure::Broken(_))), "{broken:?}");
}
