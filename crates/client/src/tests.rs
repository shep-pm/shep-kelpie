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
    Answers(&'static str),
    // Reads requests and answers none
    Silent,
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
            if let Script::Answers(reply) = &script {
                stream
                    .get_mut()
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
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
        Script::Answers(r#"{"error":"no work item for #7 is open"}"#),
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
        Script::Answers(r#"{"request":"status","version":"1.2.3"}"#),
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
