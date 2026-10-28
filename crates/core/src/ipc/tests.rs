use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use super::*;

fn socket_in(dir: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
    dir.path().join(name)
}

#[cfg(unix)]
#[test]
fn a_socket_is_its_own_address_on_unix() {
    let socket = Path::new("/k/dog/lease.sock");
    assert_eq!(address(socket), socket);
}

#[test]
fn a_pipe_is_named_for_its_socket_path_and_no_other() {
    let door = pipe_name(Path::new(r"C:\shep\kelpie\dog\lease.sock"));
    assert!(door.starts_with(r"\\.\pipe\kelpie-lease-"), "{door}");
    assert_eq!(door, pipe_name(Path::new(r"C:\shep\kelpie\dog\lease.sock")));
    assert_ne!(
        door,
        pipe_name(Path::new(r"C:\other\kelpie\dog\lease.sock"))
    );
    let long = pipe_name(Path::new(&format!(
        r"C:\{}\{}.sock",
        "d".repeat(300),
        "f".repeat(300)
    )));
    assert!(long.len() < 256, "{long}");
}

#[test]
fn one_windows_path_spelled_another_way_is_the_same_pipe() {
    let door = pipe_name(Path::new(r"C:\Users\me\.kelpie\shep\kelpie\dog\lease.sock"));
    for spelling in [
        r"c:\users\me\.kelpie\shep\kelpie\dog\lease.sock",
        "C:/Users/me/.kelpie/shep/kelpie/dog/lease.sock",
        r"C:\USERS\me/.kelpie\SHEP\kelpie\dog/Lease.sock",
    ] {
        assert_eq!(door, pipe_name(Path::new(spelling)), "{spelling}");
    }
}

#[test]
fn one_windows_path_spelled_with_dots_or_a_verbatim_prefix_is_the_same_pipe() {
    let door = pipe_name(Path::new(r"C:\base\shep\kelpie\dog\lease.sock"));
    for spelling in [
        r"C:\base\x\..\shep\kelpie\dog\lease.sock",
        r"C:\base\.\shep\kelpie\dog\lease.sock",
        r"C:\base\\shep\kelpie\dog\lease.sock",
        r"C:\base\shep\kelpie\dog\..\dog\lease.sock",
        r"\\?\C:\base\shep\kelpie\dog\lease.sock",
        r"\\?\c:/base/shep/kelpie/dog/lease.sock",
    ] {
        assert_eq!(door, pipe_name(Path::new(spelling)), "{spelling}");
    }
    assert_ne!(
        door,
        pipe_name(Path::new(r"C:\base\x\shep\kelpie\dog\lease.sock"))
    );
    assert_eq!(
        pipe_name(Path::new(r"\\host\share\dog\lease.sock")),
        pipe_name(Path::new(r"\\?\UNC\host\share\dog\lease.sock"))
    );
}

#[tokio::test]
async fn a_command_asks_and_the_door_answers_over_the_platform_transport() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir, "door.sock");
    let mut listener = Listener::bind(&socket).unwrap();
    let door = tokio::spawn(async move {
        let stream = listener.accept().await.unwrap();
        let (read, mut write) = split(stream);
        let mut line = String::new();
        tokio::io::BufReader::new(read)
            .read_line(&mut line)
            .await
            .unwrap();
        write.write_all(b"granted\n").await.unwrap();
        line
    });

    let mut stream = tokio::io::BufReader::new(connect(&socket).await.unwrap());
    stream.get_mut().write_all(b"ask\n").await.unwrap();
    let mut answer = String::new();
    stream.read_line(&mut answer).await.unwrap();

    assert_eq!(answer, "granted\n");
    assert_eq!(door.await.unwrap(), "ask\n");
}

#[tokio::test]
async fn nothing_answers_at_a_door_no_one_opened() {
    let dir = tempfile::tempdir().unwrap();
    assert!(connect(&socket_in(&dir, "none.sock")).await.is_err());
}

#[tokio::test]
async fn a_connected_pair_carries_bytes_both_ways() {
    let (server, client) = connected_pair().await.unwrap();
    let (mut server, mut client) = (server, client);
    client.write_all(b"ping\n").await.unwrap();
    let mut line = String::new();
    tokio::io::BufReader::new(&mut server)
        .read_line(&mut line)
        .await
        .unwrap();
    assert_eq!(line, "ping\n");
}

#[test]
fn a_blocking_client_and_server_exchange_lines() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(&dir, "bridge.sock");
    let listener = blocking::Listener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        stream.write_all(line.to_uppercase().as_bytes()).unwrap();
    });

    let mut stream = blocking::Stream::connect(&socket).unwrap();
    stream.write_all(b"hello\n").unwrap();
    let mut reply = String::new();
    BufReader::new(&mut stream).read_line(&mut reply).unwrap();

    assert_eq!(reply, "HELLO\n");
    server.join().unwrap();
}

#[test]
fn a_blocking_read_times_out_and_a_shutdown_ends_it() {
    let (near, far) = blocking::Stream::pair().unwrap();
    near.set_read_timeout(Some(std::time::Duration::from_millis(50)))
        .unwrap();
    let mut byte = [0u8; 1];
    let timed_out = (&near).read(&mut byte).unwrap_err();
    assert!(
        matches!(
            timed_out.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "{timed_out:?}"
    );

    near.set_read_timeout(None).unwrap();
    let waiting = near.try_clone().unwrap();
    let reader = std::thread::spawn(move || (&waiting).read(&mut [0u8; 1]).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(50));
    near.shutdown(Shutdown::Both).unwrap();
    assert_eq!(reader.join().unwrap(), 0);
    drop(far);
}

#[test]
fn closing_the_write_side_leaves_the_replies_still_to_read() {
    let (near, far) = blocking::Stream::pair().unwrap();
    near.shutdown(Shutdown::Write).unwrap();
    let late = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        (&far).write_all(b"late\n").unwrap();
        far
    });

    let mut reply = String::new();
    BufReader::new(&near).read_line(&mut reply).unwrap();

    assert_eq!(reply, "late\n");
    assert!((&near).write(b"x").is_err(), "writes are closed");
    drop(late.join().unwrap());
}

#[test]
fn a_blocking_peer_that_hangs_up_is_read_as_the_end() {
    let (near, far) = blocking::Stream::pair().unwrap();
    drop(far);
    assert_eq!((&near).read(&mut [0u8; 1]).unwrap(), 0);
}

#[test]
fn a_runner_socket_is_pinned_to_one_pipe_name() {
    let socket = crate::home::socket_of(Path::new(r"C:\Users\me\.kelpie\shep\kelpie"), "koji");
    let pipe = pipe_name(&socket);
    assert!(pipe.starts_with(r"\\.\pipe\kelpie-koji-"), "{pipe}");
    assert_eq!(
        pipe,
        pipe_name(Path::new(r"c:/users/me/.kelpie/shep/kelpie/RUN/koji.sock"))
    );
    assert_ne!(
        pipe,
        pipe_name(&crate::home::socket_of(
            Path::new(r"C:\Users\me\.kelpie\shep\kelpie"),
            "other"
        ))
    );
}
