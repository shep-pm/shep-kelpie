use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::*;
use crate::dog::open as open_book;
use crate::lease::cli::counted_run;
use crate::lease::door::Visit;
use crate::lease::gpu::GpuLock;
use crate::lease::saved::BookFile;
use crate::review_bot::Reviewers;
use crate::test::FakeClock;

const BOUND: Duration = Duration::from_secs(10);

// A dog's desk in `dir`, with `capacity` places in cargo-test.
fn kept(dir: &Path, capacity: u32) -> Arc<Mutex<Kept>> {
    let file = BookFile::new(dir.join("book.json"));
    let mut kept = open_book(
        file,
        Box::new(FakeClock::at(1_790_000_000)),
        GpuLock::under(dir),
        Reviewers::default(),
    );
    kept.desk
        .set_test_capacity(NonZeroU32::new(capacity).unwrap());
    Arc::new(Mutex::new(kept))
}

// A dog's door in its own folder, with `capacity` places.
fn door(capacity: u32) -> (tempfile::TempDir, PathBuf, Arc<Mutex<Kept>>) {
    let dir = tempfile::tempdir().unwrap();
    let desk = kept(dir.path(), capacity);
    let socket = dir.path().join("lease.sock");
    let listener = open(&socket).unwrap();
    tokio::spawn(serve(listener, Arc::clone(&desk)));
    (dir, socket, desk)
}

fn ask(what: &str) -> Ask {
    Ask {
        take: CARGO_TEST.into(),
        pid: std::process::id(),
        what: what.into(),
        running: false,
    }
}

async fn answer(visit: &mut Visit) -> Option<Answer> {
    tokio::time::timeout(BOUND, visit.answer())
        .await
        .expect("the dog never answered")
        .unwrap()
}

fn holders(desk: &Mutex<Kept>) -> Vec<String> {
    let status = lock_desk(desk).desk.tests.status();
    status.holders.into_iter().map(|h| h.taker.what).collect()
}

fn queued(desk: &Mutex<Kept>) -> usize {
    lock_desk(desk).desk.tests.status().queue.len()
}

// Waits, within the bound, until `done` holds.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let waited = async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(BOUND, waited)
        .await
        .unwrap_or_else(|_| panic!("{what}"));
}

// A process of this test binary doing `job` at `socket`, as `door_child` reads them.
// Its own process, so no signal another test sends this one reaches it.
fn child(socket: &Path, job: &str) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "dog::door::tests::door_child", "--ignored"])
        .env("KELPIE_TEST_DOOR", socket)
        .env("KELPIE_TEST_DOOR_JOB", job)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

// The child's exit code, within the bound.
async fn exited(child: &mut Child) -> Option<i32> {
    let mut status = None;
    until("the child never exited", || {
        status = status.or_else(|| child.try_wait().unwrap());
        status.is_some()
    })
    .await;
    status.unwrap().code()
}

// The other half of `child`: holds the lease, runs a dog, binds and leaves
// a socket, or runs a command under the lease and exits with what the run did.
#[tokio::test]
#[ignore = "run by the tests that start a child"]
async fn door_child() {
    let (Some(socket), Some(job)) = (
        std::env::var_os("KELPIE_TEST_DOOR"),
        std::env::var("KELPIE_TEST_DOOR_JOB").ok(),
    ) else {
        return;
    };
    let socket = PathBuf::from(socket);
    match job.split_once(':') {
        Some(("run", script)) => {
            let code = counted_run::run(&socket, &["sh", "-c", script]).await;
            std::process::exit(code.into());
        }
        Some(("exec", program)) => {
            let code = counted_run::run(&socket, &[program]).await;
            std::process::exit(code.into());
        }
        _ if job == "dog" => {
            let dir = tempfile::tempdir().unwrap();
            serve(open(&socket).unwrap(), kept(dir.path(), 1)).await;
        }
        _ if job == "bind" => {
            let _left = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            std::process::exit(0);
        }
        _ => {
            let mut visit = Visit::knock(&socket, &ask("holder")).await.unwrap();
            assert_eq!(answer(&mut visit).await, Some(Answer::Granted));
            std::future::pending::<()>().await;
        }
    }
}

#[tokio::test]
async fn a_fourth_command_waits_until_one_of_three_gives_back() {
    let (_dir, socket, desk) = door(3);
    let mut held = Vec::new();
    for n in 1..=3 {
        let mut visit = Visit::knock(&socket, &ask(&format!("suite {n}")))
            .await
            .unwrap();
        assert_eq!(answer(&mut visit).await, Some(Answer::Granted), "suite {n}");
        held.push(visit);
    }
    let mut fourth = Visit::knock(&socket, &ask("suite 4")).await.unwrap();
    assert_eq!(answer(&mut fourth).await, Some(Answer::Queued(0)));
    drop(held.remove(1));
    assert_eq!(answer(&mut fourth).await, Some(Answer::Granted));
    assert_eq!(holders(&desk), ["suite 1", "suite 3", "suite 4"]);
}

// The holder is a real process killed outright, so nothing of it says goodbye.
#[tokio::test]
async fn a_holder_that_dies_gives_its_place_back() {
    let (_dir, socket, desk) = door(1);
    let mut holder = child(&socket, "hold");
    until("the holder never took the lease", || {
        !holders(&desk).is_empty()
    })
    .await;
    let mut waiter = Visit::knock(&socket, &ask("waiter")).await.unwrap();
    assert_eq!(answer(&mut waiter).await, Some(Answer::Queued(0)));
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(answer(&mut waiter).await, Some(Answer::Granted));
    assert_eq!(holders(&desk), ["waiter"]);
}

#[tokio::test]
async fn a_waiter_that_hangs_up_leaves_the_queue() {
    let (_dir, socket, desk) = door(1);
    let mut first = Visit::knock(&socket, &ask("first")).await.unwrap();
    answer(&mut first).await;
    let mut gone = Visit::knock(&socket, &ask("gone")).await.unwrap();
    assert_eq!(answer(&mut gone).await, Some(Answer::Queued(0)));
    drop(gone);
    until("the hung-up waiter kept its place", || queued(&desk) == 0).await;
    let mut next = Visit::knock(&socket, &ask("next")).await.unwrap();
    assert_eq!(answer(&mut next).await, Some(Answer::Queued(0)));
}

#[tokio::test]
async fn a_run_waits_for_room_and_keeps_its_commands_exit_code() {
    let (dir, socket, desk) = door(1);
    let mut first = Visit::knock(&socket, &ask("first")).await.unwrap();
    answer(&mut first).await;
    let marker = dir.path().join("ran");
    let mut run = child(&socket, &format!("run:touch {}; exit 7", marker.display()));
    until("the run never asked", || queued(&desk) == 1).await;
    assert!(!marker.exists(), "the command ran before it was granted");
    drop(first);
    assert_eq!(exited(&mut run).await, Some(7));
    assert!(marker.exists());
    until("the run kept the lease", || holders(&desk).is_empty()).await;
}

#[tokio::test]
async fn a_command_that_cannot_start_exits_as_a_shell_would() {
    let (_dir, socket, desk) = door(1);
    let mut run = child(&socket, "exec:/nonexistent/kelpie-test-command");
    let not_run = i32::from(counted_run::NOT_RUN);
    assert_eq!(exited(&mut run).await, Some(not_run));
    until("the run kept the lease", || holders(&desk).is_empty()).await;
}

#[tokio::test]
async fn a_run_with_no_dog_says_so_with_its_own_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = child(&dir.path().join("lease.sock"), "run:true");
    let no_lease = i32::from(counted_run::NO_LEASE);
    assert_eq!(exited(&mut run).await, Some(no_lease));
}

// The wrapper dies outright; its command still runs, so it still holds the lease.
#[tokio::test]
async fn a_killed_run_keeps_the_lease_until_its_command_ends() {
    let (dir, socket, _desk) = door(1);
    let started = dir.path().join("started");
    let mut run = child(
        &socket,
        &format!("run:touch {}; sleep 2", started.display()),
    );
    until("the command never started", || started.exists()).await;
    run.kill().unwrap();
    run.wait().unwrap();
    let mut waiter = Visit::knock(&socket, &ask("waiter")).await.unwrap();
    assert_eq!(answer(&mut waiter).await, Some(Answer::Queued(0)));
    assert_eq!(answer(&mut waiter).await, Some(Answer::Granted));
}

// A process the command leaves behind keeps the connection open, not the lease.
#[tokio::test]
async fn a_command_that_ends_gives_the_lease_back_whatever_it_left_running() {
    let (_dir, socket, desk) = door(1);
    let mut run = child(&socket, "run:sleep 5 & exit 0");
    assert_eq!(exited(&mut run).await, Some(0));
    until("the leftover process kept the lease", || {
        holders(&desk).is_empty()
    })
    .await;
}

// The first dog is a child process, so killing it is its death, and no
// fork of this process can have inherited its door.
#[tokio::test]
async fn a_run_whose_dog_restarts_is_counted_again_by_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lease.sock");
    let mut first_dog = child(&socket, "dog");
    until("the first dog never opened", || socket.exists()).await;
    let mut run = child(&socket, "run:sleep 4");
    let mut probe = Visit::knock(&socket, &ask("probe")).await.unwrap();
    while answer(&mut probe).await == Some(Answer::Granted) {
        drop(probe);
        tokio::time::sleep(Duration::from_millis(20)).await;
        probe = Visit::knock(&socket, &ask("probe")).await.unwrap();
    }
    drop(probe);
    first_dog.kill().unwrap();
    first_dog.wait().unwrap();

    let next_desk = kept(dir.path(), 1);
    tokio::spawn(serve(open(&socket).unwrap(), Arc::clone(&next_desk)));
    until("the next dog never counted the run", || {
        holders(&next_desk) == ["sh -c sleep 4"]
    })
    .await;
    let mut waiter = Visit::knock(&socket, &ask("waiter")).await.unwrap();
    assert_eq!(answer(&mut waiter).await, Some(Answer::Queued(0)));
    let _ = run.kill();
    let _ = run.wait();
}

#[tokio::test]
async fn a_line_that_is_not_an_ask_is_refused() {
    let (_dir, socket, desk) = door(1);
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    stream.write_all(b"take cargo-test\n").await.unwrap();
    let mut reply = String::new();
    let mut reader = BufReader::new(stream);
    let read = reader.read_line(&mut reply);
    tokio::time::timeout(BOUND, read).await.unwrap().unwrap();
    let answer: Answer = serde_json::from_str(&reply).unwrap();
    assert!(
        matches!(&answer, Answer::Error(why) if why.starts_with("not an ask")),
        "{answer:?}"
    );
    assert!(holders(&desk).is_empty());
}

#[tokio::test]
async fn any_other_lease_is_refused_at_the_door() {
    let (_dir, socket, _desk) = door(3);
    let other = Ask {
        take: "coderabbit".into(),
        ..ask("x")
    };
    let mut visit = Visit::knock(&socket, &other).await.unwrap();
    let Some(Answer::Error(why)) = answer(&mut visit).await else {
        panic!("the door let coderabbit in");
    };
    assert!(why.contains("only cargo-test"), "{why}");
    assert_eq!(answer(&mut visit).await, None);
}

// The dead socket is bound in a child that has exited, so no process
// this one forks can have inherited it.
#[tokio::test]
async fn a_live_door_is_never_taken_over_and_a_dead_one_is() {
    let (dir, socket, _desk) = door(3);
    let err = open(&socket).unwrap_err();
    assert!(err.contains("another dog answers"), "{err}");

    let stale = dir.path().join("stale.sock");
    let mut binder = child(&stale, "bind");
    assert_eq!(exited(&mut binder).await, Some(0));
    assert!(open(&stale).is_ok());
    let mode = std::fs::metadata(&stale).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}
