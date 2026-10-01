use std::time::Duration;

use super::*;
use crate::settings::EndpointUrl;
use crate::test::{Answer, StandInEndpoint, unreachable_url};

const CHAT: &str = "POST http://model.kelpie.test/v1/chat/completions HTTP/1.1\r\n";

fn upstream(url: &str) -> Upstream {
    Upstream::new(&EndpointUrl::try_from(url.to_owned()).unwrap()).unwrap()
}

// In `/tmp`, as macOS's own temporary folder is too long for a socket.
fn forwarder(url: &str) -> (tempfile::TempDir, Forwarder) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let folder = dir.path().canonicalize().unwrap();
    let forwarder = Forwarder::open(&folder, upstream(url)).unwrap();
    (dir, forwarder)
}

// Sends `bytes` the way the sandbox's proxy does, and reads the reply to its end.
fn send(forwarder: &Forwarder, bytes: &[u8]) -> String {
    let mut stream = UnixStream::connect(&forwarder.socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(bytes).unwrap();
    let _ = stream.shutdown(Shutdown::Write);
    let mut reply = Vec::new();
    // A refusal may close before the body is read, which resets the read.
    let _ = stream.read_to_end(&mut reply);
    String::from_utf8_lossy(&reply).into_owned()
}

fn chat(body: &str) -> Vec<u8> {
    format!(
        "{CHAT}host: model.kelpie.test\r\ncontent-type: application/json\r\n\
         content-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn status(reply: &str) -> &str {
    reply.lines().next().unwrap_or("")
}

#[test]
fn a_chat_call_reaches_the_server_and_its_answer_comes_back() {
    let server = StandInEndpoint::start([Answer::Says("hello from the model")]);
    let (_dir, forwarder) = forwarder(server.url());
    let reply = send(&forwarder, &chat(r#"{"model":"qwen","stream":false}"#));
    assert!(status(&reply).contains("200"), "{reply}");
    assert!(reply.contains("hello from the model"), "{reply}");
    assert_eq!(server.seen(), ["POST /v1/chat/completions HTTP/1.1"]);
    assert_eq!(server.requests()[0]["model"], "qwen");
}

const TUNNEL: &str = "CONNECT model.kelpie.test:80 HTTP/1.1\r\nhost: model.kelpie.test:80\r\n\r\n";

#[test]
fn a_chat_call_through_a_tunnel_to_the_workers_host_is_served_and_any_other_is_refused() {
    let server = StandInEndpoint::start([Answer::Says("hello through the tunnel")]);
    let (_dir, forwarder) = forwarder(server.url());
    let body = chat("{}");
    let mut bytes = TUNNEL.as_bytes().to_vec();
    bytes.extend(&body);
    let reply = send(&forwarder, &bytes);
    assert!(
        status(&reply).contains("200 Connection Established"),
        "{reply}"
    );
    assert!(reply.contains("hello through the tunnel"), "{reply}");

    let admin = format!("{TUNNEL}DELETE /api/delete HTTP/1.1\r\n\r\n");
    let reply = send(&forwarder, admin.as_bytes());
    assert!(reply.contains("DELETE /api/delete was refused"), "{reply}");
    let nested = format!("{TUNNEL}CONNECT model.kelpie.test:80 HTTP/1.1\r\n\r\n");
    let reply = send(&forwarder, nested.as_bytes());
    assert!(
        reply.contains("CONNECT model.kelpie.test:80 was refused"),
        "{reply}"
    );
    for authority in ["192.0.2.9:22", "model.kelpie.test:443", "model.kelpie.test"] {
        let reply = send(
            &forwarder,
            format!("CONNECT {authority} HTTP/1.1\r\n\r\n").as_bytes(),
        );
        assert!(status(&reply).contains("403"), "{reply}");
        assert!(reply.contains(&format!("CONNECT {authority}")), "{reply}");
    }
    assert_eq!(server.seen(), ["POST /v1/chat/completions HTTP/1.1"]);
}

#[test]
fn a_chunked_chat_call_arrives_whole() {
    let server = StandInEndpoint::start([Answer::Says("ok")]);
    let (_dir, forwarder) = forwarder(server.url());
    let request = format!(
        "{CHAT}transfer-encoding: chunked\r\n\r\n8\r\n{{\"model\"\r\n8;ext=1\r\n:\"qwen\"}}\r\n0\r\nx-trailer: 1\r\n\r\n"
    );
    let reply = send(&forwarder, request.as_bytes());
    assert!(status(&reply).contains("200"), "{reply}");
    assert_eq!(server.requests()[0]["model"], "qwen");
}

#[test]
fn an_admin_call_is_refused_by_name_and_never_reaches_the_server() {
    let server = StandInEndpoint::start([]);
    let (_dir, forwarder) = forwarder(server.url());
    for (method, path) in [
        ("DELETE", "/api/delete"),
        ("POST", "/api/pull"),
        ("POST", "/api/create"),
        ("GET", "/api/tags"),
        ("GET", "/v1/models"),
    ] {
        let request = format!(
            "{method} http://model.kelpie.test{path} HTTP/1.1\r\ncontent-length: 2\r\n\r\n{{}}"
        );
        let reply = send(&forwarder, request.as_bytes());
        assert!(status(&reply).contains("403"), "{reply}");
        assert!(reply.contains(&format!("{method} {path}")), "{reply}");
        assert!(reply.contains("only POST /v1/chat/completions"), "{reply}");
    }
    let tunnel = send(&forwarder, b"CONNECT 192.0.2.9:22 HTTP/1.1\r\n\r\n");
    assert!(status(&tunnel).contains("403"), "{tunnel}");
    assert!(tunnel.contains("CONNECT 192.0.2.9:22"), "{tunnel}");
    assert_eq!(server.seen(), Vec::<String>::new());
}

#[test]
fn a_request_behind_a_chat_call_is_not_forwarded() {
    let server = StandInEndpoint::start([Answer::Says("ok")]);
    let (_dir, forwarder) = forwarder(server.url());
    let mut bytes = chat("{}");
    bytes.extend(b"DELETE /api/delete HTTP/1.1\r\ncontent-length: 0\r\n\r\n");
    let reply = send(&forwarder, &bytes);
    assert!(status(&reply).contains("200"), "{reply}");
    assert_eq!(server.seen(), ["POST /v1/chat/completions HTTP/1.1"]);
}

#[test]
fn a_chat_call_with_two_ways_to_say_its_length_is_refused() {
    let server = StandInEndpoint::start([]);
    let (_dir, forwarder) = forwarder(server.url());
    let both = format!("{CHAT}content-length: 2\r\ntransfer-encoding: chunked\r\n\r\n0\r\n\r\n");
    let two = format!("{CHAT}content-length: 2\r\ncontent-length: 3\r\n\r\n{{}}");
    let gzip = format!("{CHAT}transfer-encoding: gzip\r\n\r\n");
    for (request, code) in [(both, "400"), (two, "400"), (gzip, "501")] {
        let reply = send(&forwarder, request.as_bytes());
        assert!(status(&reply).contains(code), "{reply}");
    }
    let short = send(
        &forwarder,
        format!("{CHAT}content-length: 50\r\n\r\n{{}}").as_bytes(),
    );
    assert!(status(&short).contains("400"), "{short}");
    assert_eq!(server.seen(), Vec::<String>::new());
}

#[test]
fn a_model_server_that_cannot_be_reached_is_a_bad_gateway_that_does_not_name_it() {
    let url = unreachable_url();
    let (_dir, forwarder) = forwarder(&url);
    let reply = send(&forwarder, &chat("{}"));
    assert!(status(&reply).contains("502"), "{reply}");
    assert!(!reply.contains("127.0.0.1"), "{reply}");
}

#[test]
fn dropping_the_forwarder_ends_a_call_in_flight_and_removes_the_socket() {
    let server = StandInEndpoint::start([]);
    let (_dir, forwarder) = forwarder(server.url());
    let socket = forwarder.socket.clone();
    let mut idle = UnixStream::connect(&socket).unwrap();
    idle.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    // A head that never ends leaves the connection waiting on the client.
    idle.write_all(b"POST /v1/chat/completions HTTP/1.1\r\n")
        .unwrap();
    drop(forwarder);
    assert!(!socket.exists());
    assert!(UnixStream::connect(&socket).is_err());
    let mut rest = Vec::new();
    let ended = idle.read_to_end(&mut rest);
    assert!(ended.is_ok(), "{ended:?}");
}

#[test]
fn the_socket_has_a_name_no_one_can_guess() {
    let server = StandInEndpoint::start([]);
    let (_dir, forwarder) = forwarder(server.url());
    let name = forwarder.socket.file_name().unwrap().to_string_lossy();
    assert_eq!(name.len(), 32 + ".sock".len(), "{name}");
}
