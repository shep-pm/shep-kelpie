use std::num::NonZeroU32;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::*;
use crate::dog::board::Board;
use crate::dog::open as open_book;
use crate::lease::cli::counted_run;
use crate::lease::door::{NO_E2E_HOST, Visit};
use crate::lease::saved::BookFile;
use crate::review_bot::Reviewers;
use crate::test::{EPOCH, FakeClock, RecordedSink};

// A `Visit` made here can be inherited by a child another test forks at
// that moment, which delays the dog hearing it close until that child
// ends. Children here live seconds, well inside this bound.
const BOUND: Duration = Duration::from_secs(10);

// A dog's desk in `dir`, with `capacity` places in cargo-test.
fn kept(dir: &Path, capacity: u32) -> Arc<Mutex<Kept>> {
    let file = BookFile::new(dir.join("book.json"));
    let mut kept = open_book(
        file,
        Box::new(FakeClock::at(EPOCH)),
        Reviewers::default(),
        RecordedSink::default().recorder(),
    );
    kept.desk
        .set_test_capacity(NonZeroU32::new(capacity).unwrap());
    Arc::new(Mutex::new(kept))
}

// The board of kelpie's own agents, with no ledger read.
fn no_board() -> Arc<Mutex<Board>> {
    Arc::new(Mutex::new(Board::new(&crate::agents::Agents::embedded())))
}

// A dog's door in its own folder, with `capacity` places.
fn door(capacity: u32) -> (tempfile::TempDir, PathBuf, Arc<Mutex<Kept>>) {
    door_seeing(capacity, Availability::default(), no_board())
}

// A dog's door that answers a pick from `shown` and `board`.
fn door_seeing(
    capacity: u32,
    shown: Availability,
    board: Arc<Mutex<Board>>,
) -> (tempfile::TempDir, PathBuf, Arc<Mutex<Kept>>) {
    let dir = tempfile::tempdir().unwrap();
    let (socket, desk) = door_in(dir.path(), capacity, shown, board);
    (dir, socket, desk)
}

// A dog's door in `dir`, whose kelpie home is `dir/kelpie`, answering a pick
// from `shown` and `board`. Its socket, and the desk holding the leases.
fn door_in(
    dir: &Path,
    capacity: u32,
    shown: Availability,
    board: Arc<Mutex<Board>>,
) -> (PathBuf, Arc<Mutex<Kept>>) {
    let desk = kept(dir, capacity);
    let socket = dir.join("lease.sock");
    let listener = open(&socket).unwrap();
    tokio::spawn(serve(
        listener,
        Arc::clone(&desk),
        Duration::ZERO,
        dir.join("kelpie"),
        Remote::default(),
        Arc::new(Mutex::new(shown)),
        board,
    ));
    (socket, desk)
}

// A client is blocking, so it runs off the runtime the dog's door is served on.
async fn off_the_runtime<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work).await.unwrap()
}

fn ask(what: &str) -> Ask {
    Ask::new(CARGO_TEST, std::process::id(), what)
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
            let code =
                counted_run::run(&socket, &["sh", "-c", script], counted_run::RUN_LIMIT).await;
            std::process::exit(code.into());
        }
        Some(("hurry", script)) => {
            let hurried = counted_run::Patience {
                tries: 1,
                ..counted_run::PATIENCE
            };
            let code = counted_run::run_with_patience(
                &socket,
                &["sh", "-c", script],
                counted_run::RUN_LIMIT,
                hurried,
            )
            .await;
            std::process::exit(code.into());
        }
        // "<milliseconds>:<script>", with the bound the job gives
        Some(("bounded", rest)) => {
            let (millis, script) = rest.split_once(':').unwrap();
            let limit = Duration::from_millis(millis.parse().unwrap());
            let code = counted_run::run(&socket, &["sh", "-c", script], limit).await;
            std::process::exit(code.into());
        }
        Some(("exec", program)) => {
            let code = counted_run::run(&socket, &[program], counted_run::RUN_LIMIT).await;
            std::process::exit(code.into());
        }
        _ if job == "dog" => {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("kelpie");
            let desk = kept(dir.path(), 1);
            serve(
                open(&socket).unwrap(),
                desk,
                Duration::ZERO,
                home,
                Remote::default(),
                Arc::default(),
                no_board(),
            )
            .await;
        }
        _ if job == "bind" => {
            let _left = crate::ipc::blocking::Listener::bind(&socket).unwrap();
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

// A descendant of the command inherits the connection and may write to it;
// only the connection's end, with the last holder of it gone, gives it back.
#[tokio::test]
async fn a_stray_line_from_a_descendant_does_not_give_the_lease_back() {
    let (_dir, socket, desk) = door(1);
    let mut stream = crate::ipc::connect(&socket).await.unwrap();
    let mut line = serde_json::to_vec(&ask("holder")).unwrap();
    line.push(b'\n');
    stream.write_all(&line).await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    while !reply.contains("granted") {
        reply.clear();
        let read = reader.read_line(&mut reply);
        assert_ne!(tokio::time::timeout(BOUND, read).await.unwrap().unwrap(), 0);
    }
    reader.get_mut().write_all(b"stray\n").await.unwrap();
    let mut waiter = Visit::knock(&socket, &ask("waiter")).await.unwrap();
    assert_eq!(answer(&mut waiter).await, Some(Answer::Queued(0)));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(holders(&desk), ["holder"]);
    drop(reader);
    assert_eq!(answer(&mut waiter).await, Some(Answer::Granted));
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

// The bound counts from the grant: a run that queued longer than its bound
// still gets all of it.
#[tokio::test]
async fn a_run_gets_its_whole_bound_after_waiting_in_the_queue() {
    let (_dir, socket, desk) = door(1);
    let mut first = Visit::knock(&socket, &ask("first")).await.unwrap();
    answer(&mut first).await;
    let mut run = child(&socket, "bounded:2000:sleep 1; exit 7");
    until("the run never asked", || queued(&desk) == 1).await;
    tokio::time::sleep(Duration::from_millis(3000)).await;
    drop(first);
    assert_eq!(exited(&mut run).await, Some(7));
}

#[tokio::test]
async fn a_run_ended_at_its_bound_gives_the_lease_back() {
    let (_dir, socket, desk) = door(1);
    let mut run = child(&socket, "bounded:500:sleep 300");
    until("the run never took the lease", || {
        !holders(&desk).is_empty()
    })
    .await;
    let mut next = Visit::knock(&socket, &ask("next")).await.unwrap();
    assert_eq!(answer(&mut next).await, Some(Answer::Queued(0)));
    assert_eq!(answer(&mut next).await, Some(Answer::Granted));
    assert_eq!(
        exited(&mut run).await,
        Some(i32::from(counted_run::TIMED_OUT))
    );
    assert_eq!(holders(&desk), ["next"]);
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
    let mut run = child(&dir.path().join("lease.sock"), "hurry:true");
    let no_lease = i32::from(counted_run::NO_LEASE);
    assert_eq!(exited(&mut run).await, Some(no_lease));
}

// The wrapper dies outright; its command still runs, so it still holds the lease.
// A Windows pipe is not handed to the command, so there the lease goes with the wrapper.
#[cfg(unix)]
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

// A dog in a child process, killing which is its death. No fork of this
// process can have inherited its door. It has opened once something connects:
// no file stands for a door on Windows.
async fn first_dog(socket: &Path) -> Child {
    let dog = child(socket, "dog");
    until("the first dog never opened", || {
        crate::ipc::blocking::Stream::connect(socket).is_ok()
    })
    .await;
    dog
}

// Knocks until the door answers `Queued`, so whatever holds it took it first.
async fn queued_behind(socket: &Path, what: &str) -> Visit {
    loop {
        let mut probe = Visit::knock(socket, &ask(what)).await.unwrap();
        if answer(&mut probe).await != Some(Answer::Granted) {
            return probe;
        }
        drop(probe);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// The next dog starts full, so only `running` gets the run counted.
#[tokio::test]
async fn a_run_whose_dog_restarts_is_seated_by_the_next_even_when_full() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lease.sock");
    let mut dog = first_dog(&socket).await;
    let mut run = child(&socket, "run:sleep 4");
    drop(queued_behind(&socket, "probe").await);
    dog.kill().unwrap();
    dog.wait().unwrap();

    let next_desk = kept(dir.path(), 1);
    tokio::spawn(serve(
        open(&socket).unwrap(),
        Arc::clone(&next_desk),
        Duration::ZERO,
        dir.path().join("kelpie"),
        Remote::default(),
        Arc::default(),
        no_board(),
    ));
    let mut holder = Visit::knock(&socket, &ask("holder")).await.unwrap();
    assert_eq!(answer(&mut holder).await, Some(Answer::Granted));
    until("the next dog never counted the run", || {
        holders(&next_desk) == ["holder", "sh -c sleep 4"]
    })
    .await;
    let _ = run.kill();
    let _ = run.wait();
}

// The dog goes away while the run waits; the run asks the next one afresh.
#[tokio::test]
async fn a_queued_run_whose_dog_restarts_waits_for_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lease.sock");
    let mut dog = first_dog(&socket).await;
    let mut holder = Visit::knock(&socket, &ask("holder")).await.unwrap();
    assert_eq!(answer(&mut holder).await, Some(Answer::Granted));
    let marker = dir.path().join("ran");
    let mut run = child(&socket, &format!("run:touch {}", marker.display()));
    drop(queued_behind(&socket, "probe").await);
    dog.kill().unwrap();
    dog.wait().unwrap();
    drop(holder);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!marker.exists(), "the run went ahead without a dog");

    let next_desk = kept(dir.path(), 1);
    tokio::spawn(serve(
        open(&socket).unwrap(),
        Arc::clone(&next_desk),
        Duration::ZERO,
        dir.path().join("kelpie"),
        Remote::default(),
        Arc::default(),
        no_board(),
    ));
    assert_eq!(exited(&mut run).await, Some(0));
    assert!(marker.exists());
}

// A dog that answers `queued` and goes away `restarts` times, then grants and
// holds the lease until the run ends. Each life takes the one ask it hears.
async fn fake_dog(socket: PathBuf, restarts: u32) {
    for life in 0..=restarts {
        let mut listener = open(&socket).unwrap();
        let stream = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        stream.read_line(&mut String::new()).await.unwrap();
        let mut said = vec![Answer::Queued(0)];
        if life == restarts {
            said.push(Answer::Granted);
        }
        for answer in said {
            let mut line = serde_json::to_vec(&answer).unwrap();
            line.push(b'\n');
            stream.get_mut().write_all(&line).await.unwrap();
        }
        if life == restarts {
            stream.read_line(&mut String::new()).await.unwrap();
        }
        drop(listener);
        // No file stands for the door on Windows.
        let _ = std::fs::remove_file(&socket);
    }
}

// Every restart is a dog that answered, so a run that has waited minutes in
// the queue is not out of patience for the next. The run knocks every few
// milliseconds here, not every second.
#[tokio::test]
async fn a_queued_run_keeps_asking_after_each_dog_restart() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lease.sock");
    let restarts = counted_run::REJOIN_TRIES + 5;
    let dog = tokio::spawn(fake_dog(socket.clone(), restarts));
    let patience = counted_run::Patience {
        every: Duration::from_millis(5),
        ..counted_run::PATIENCE
    };
    let run = counted_run::run_with_patience(&socket, &["true"], counted_run::RUN_LIMIT, patience);
    let code = tokio::time::timeout(BOUND, run)
        .await
        .expect("the run never ended");
    assert_eq!(code, 0);
    tokio::time::timeout(BOUND, dog).await.unwrap().unwrap();
}

// A restarted dog grants nothing new until the runs it lost have rejoined.
#[tokio::test]
async fn a_new_door_seats_running_commands_before_any_new_ask() {
    let dir = tempfile::tempdir().unwrap();
    let desk = kept(dir.path(), 1);
    let socket = dir.path().join("lease.sock");
    let grace = Duration::from_secs(2);
    let home = dir.path().join("kelpie");
    tokio::spawn(serve(
        open(&socket).unwrap(),
        Arc::clone(&desk),
        grace,
        home,
        Remote::default(),
        Arc::default(),
        no_board(),
    ));
    let mut fresh = Visit::knock(&socket, &ask("fresh")).await.unwrap();
    let early = tokio::time::timeout(Duration::from_millis(500), fresh.answer()).await;
    assert!(
        early.is_err(),
        "a new ask was answered in the grace: {early:?}"
    );
    let rejoining = Ask {
        running: true,
        ..ask("rejoining")
    };
    let mut rejoin = Visit::knock(&socket, &rejoining).await.unwrap();
    assert_eq!(answer(&mut rejoin).await, Some(Answer::Granted));
    assert_eq!(answer(&mut fresh).await, Some(Answer::Queued(0)));
    assert_eq!(holders(&desk), ["rejoining"]);
}

// The tests above pass no grace, so this is what keeps the real one from
// opening before a run whose dog went away has knocked twice.
#[test]
fn the_production_grace_outlasts_a_runs_knock() {
    assert!(REJOIN_GRACE > 2 * counted_run::REJOIN_EVERY);
}

#[tokio::test]
async fn a_line_that_is_not_an_ask_is_refused() {
    let (_dir, socket, desk) = door(1);
    let mut stream = crate::ipc::connect(&socket).await.unwrap();
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
async fn an_ask_for_any_other_lease_is_still_refused() {
    let (_dir, socket, _desk) = door(3);
    let other = Ask {
        take: "coderabbit".into(),
        ..ask("x")
    };
    let mut visit = Visit::knock(&socket, &other).await.unwrap();
    let Some(Answer::Error(why)) = answer(&mut visit).await else {
        panic!("the door let coderabbit in");
    };
    assert!(why.contains("only cargo-test and e2e"), "{why}");
    assert_eq!(answer(&mut visit).await, None);
}

// An e2e ask from a worktree of a project under the door's kelpie home
// that has no `e2e.json`.
async fn e2e_answer() -> (Answer, Vec<Answer>) {
    let (dir, socket, _desk) = door(1);
    let worktree = dir.path().join("kelpie/koji/worktrees/7");
    std::fs::create_dir_all(&worktree).unwrap();
    let ask = Ask {
        take: E2E.into(),
        worktree: Some(worktree),
        forge: None,
        ..ask("e2e")
    };
    let mut visit = Visit::knock(&socket, &ask).await.unwrap();
    let first = answer(&mut visit).await.expect("the dog answered");
    let mut rest = Vec::new();
    while let Some(more) = answer(&mut visit).await {
        rest.push(more);
    }
    (first, rest)
}

#[tokio::test]
async fn an_e2e_ask_from_a_project_without_the_table_is_told_it_is_not_configured() {
    let (first, rest) = e2e_answer().await;
    let Answer::Error(why) = first else {
        panic!("the door did not refuse: {first:?}");
    };
    assert!(why.starts_with(NO_E2E_HOST), "{why}");
    assert!(why.contains("[app.dogs.kelpie.e2e]"), "{why}");
    assert_eq!(rest, [], "the dog closes after refusing");
}

#[tokio::test]
async fn an_e2e_ask_from_a_path_that_names_no_project_is_told_it_is_not_configured() {
    let (dir, socket, _desk) = door(1);
    let ask = |worktree: Option<PathBuf>| Ask {
        take: E2E.into(),
        worktree,
        ..ask("e2e")
    };
    for worktree in [None, Some(dir.path().to_owned()), Some("/nowhere".into())] {
        let mut visit = Visit::knock(&socket, &ask(worktree.clone())).await.unwrap();
        let Some(Answer::Error(why)) = answer(&mut visit).await else {
            panic!("the door did not refuse {worktree:?}");
        };
        assert!(why.starts_with(NO_E2E_HOST), "{why}");
    }
}

#[tokio::test]
async fn a_live_door_is_never_taken_over() {
    let (_dir, socket, _desk) = door(3);
    let err = open(&socket).unwrap_err();
    assert!(err.contains("another dog answers"), "{err}");
}

// The dead socket is bound in a child that has exited, so no process
// this one forks can have inherited it. A pipe leaves nothing behind to take over.
#[cfg(unix)]
#[tokio::test]
async fn a_dead_door_is_taken_over_and_closed_to_others() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let stale = dir.path().join("stale.sock");
    let mut binder = child(&stale, "bind");
    assert_eq!(exited(&mut binder).await, Some(0));
    assert!(open(&stale).is_ok());
    let mode = std::fs::metadata(&stale).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

mod e2e;
mod fit;
mod forge;
mod pick;

#[test]
fn a_lease_is_logged_as_granted_only_when_the_ask_had_to_wait() {
    let line = || "cargo-test: granted to pid 9 running cargo test".to_owned();
    assert_eq!(granted_after_wait(Taken::Granted, line()), None);
    assert_eq!(
        granted_after_wait(Taken::Queued { ahead: 2 }, line()).as_deref(),
        Some("cargo-test: granted to pid 9 running cargo test, after 2 ahead of it")
    );
}
