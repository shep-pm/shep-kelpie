// An e2e ask through a door whose ssh and rsync are the stand-ins.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::dog::e2e::CEILING;
use crate::dog::e2e::stand_in::{StandIn, ended};
use crate::lease::door::TIMED_OUT;

const TABLE: &str = r#"{"host":"e2e-box"}"#;

// A dog's door in its own folder, running e2e asks through `stand`.
fn e2e_door(stand: &StandIn) -> (tempfile::TempDir, PathBuf, Arc<Mutex<Kept>>) {
    let dir = tempfile::tempdir().unwrap();
    let desk = kept(dir.path(), 1);
    let socket = dir.path().join("lease.sock");
    tokio::spawn(serve(
        open(&socket).unwrap(),
        Arc::clone(&desk),
        Duration::ZERO,
        stand.kelpie_home(),
        stand.remote.clone(),
        Arc::default(),
        no_board(),
    ));
    (dir, socket, desk)
}

fn e2e_ask(worktree: &Path) -> Ask {
    Ask {
        take: E2E.into(),
        worktree: Some(worktree.to_owned()),
        forge: None,
        ..ask("shep-kelpie lease e2e")
    }
}

// The answers up to the run's end, which the dog follows by closing.
async fn until_done(visit: &mut Visit) -> Vec<Answer> {
    let mut answers = Vec::new();
    while let Some(next) = answer(visit).await {
        answers.push(next);
    }
    answers
}

// Waits, in real time and within the bound, until `done` holds. A test with
// the clock paused cannot use `until`: its sleeps would run out the bound
// before a process the dog started had done anything.
async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !done() {
        assert!(start.elapsed() < BOUND, "{what}");
        let rest = std::time::Duration::from_millis(10);
        tokio::task::spawn_blocking(move || std::thread::sleep(rest))
            .await
            .unwrap();
    }
}

fn e2e_holders(desk: &Mutex<Kept>, host: &str) -> usize {
    let kept = lock_desk(desk);
    kept.desk
        .e2e
        .get(host)
        .map_or(0, |l| l.status().holders.len())
}

#[tokio::test]
async fn a_second_e2e_ask_on_the_same_host_waits_for_the_first() {
    let stand = StandIn::new();
    let script = format!("mkdir -p test-results; {}", stand.wait_at_gate());
    let first_tree = stand.project("koji", TABLE, 7, &script);
    let second_tree = stand.project("koji", TABLE, 8, &script);
    let (_dir, socket, _desk) = e2e_door(&stand);

    let mut first = Visit::knock(&socket, &e2e_ask(&first_tree)).await.unwrap();
    assert_eq!(answer(&mut first).await, Some(Answer::Granted));
    let mut second = Visit::knock(&socket, &e2e_ask(&second_tree)).await.unwrap();
    assert_eq!(answer(&mut second).await, Some(Answer::Queued(0)));

    stand.open_gate();
    assert_eq!(until_done(&mut first).await, [Answer::Done(0)]);
    assert_eq!(
        until_done(&mut second).await,
        [Answer::Granted, Answer::Done(0)]
    );
}

#[tokio::test]
async fn two_hosts_do_not_wait_on_each_other() {
    let stand = StandIn::new();
    let script = format!("mkdir -p test-results; {}", stand.wait_at_gate());
    let koji = stand.project("koji", TABLE, 7, &script);
    let golbat = stand.project("golbat", r#"{"host":"other-box"}"#, 3, &script);
    let (_dir, socket, desk) = e2e_door(&stand);

    let mut first = Visit::knock(&socket, &e2e_ask(&koji)).await.unwrap();
    assert_eq!(answer(&mut first).await, Some(Answer::Granted));
    let mut second = Visit::knock(&socket, &e2e_ask(&golbat)).await.unwrap();
    assert_eq!(answer(&mut second).await, Some(Answer::Granted));
    assert_eq!(e2e_holders(&desk, "e2e-box"), 1);
    assert_eq!(e2e_holders(&desk, "other-box"), 1);

    stand.open_gate();
    assert_eq!(until_done(&mut first).await, [Answer::Done(0)]);
    assert_eq!(until_done(&mut second).await, [Answer::Done(0)]);
}

#[tokio::test]
async fn an_ask_cannot_name_a_host_or_a_command() {
    let stand = StandIn::new();
    let worktree = stand.project("koji", TABLE, 7, "mkdir -p test-results");
    let (_dir, socket, _desk) = e2e_door(&stand);

    // An ask with a host or a command in it is not one the door reads.
    let named = serde_json::json!({
        "take": "e2e",
        "pid": 1,
        "what": "x",
        "worktree": worktree,
        "host": "evil-box",
        "command": "scripts/evil.sh",
        "results": "/",
    });
    let mut stream = crate::ipc::connect(&socket).await.unwrap();
    stream
        .write_all(format!("{named}\n").as_bytes())
        .await
        .unwrap();
    let mut reply = String::new();
    tokio::time::timeout(BOUND, stream.read_to_string(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert!(reply.contains("not an ask"), "{reply}");
    assert!(stand.started().is_empty());

    // The ask it does read runs where the project's own table says.
    let mut visit = Visit::knock(&socket, &e2e_ask(&worktree)).await.unwrap();
    assert_eq!(
        until_done(&mut visit).await,
        [Answer::Granted, Answer::Done(0)]
    );
    let started = stand.started().join("\n");
    assert!(started.contains(" e2e-box "), "{started}");
    assert!(!started.contains("evil"), "{started}");
}

#[tokio::test(start_paused = true)]
async fn a_worker_that_hangs_up_ends_the_run_and_gives_the_host_back() {
    let stand = StandIn::new();
    let script = format!("mkdir -p test-results; {}", stand.wait_at_gate());
    let first_tree = stand.project("koji", TABLE, 7, &script);
    let second_tree = stand.project("koji", TABLE, 8, &script);
    let (_dir, socket, desk) = e2e_door(&stand);

    let mut first = Visit::knock(&socket, &e2e_ask(&first_tree)).await.unwrap();
    assert_eq!(answer(&mut first).await, Some(Answer::Granted));
    let mut second = Visit::knock(&socket, &e2e_ask(&second_tree)).await.unwrap();
    assert_eq!(answer(&mut second).await, Some(Answer::Queued(0)));
    eventually("ssh never started", || stand.ssh_pid().is_some()).await;
    let ssh = stand.ssh_pid().unwrap();

    first.give_back().await;

    // The next in the queue is granted, and the first run's ssh is gone.
    assert_eq!(answer(&mut second).await, Some(Answer::Granted));
    eventually("ssh was left running", || ended(ssh)).await;
    assert_eq!(e2e_holders(&desk, "e2e-box"), 1);
}

#[tokio::test(start_paused = true)]
async fn a_run_past_its_ceiling_is_ended() {
    let stand = StandIn::new();
    let script = format!("mkdir -p test-results; {}", stand.wait_at_gate());
    let worktree = stand.project("koji", TABLE, 7, &script);
    let (_dir, socket, desk) = e2e_door(&stand);

    let mut visit = Visit::knock(&socket, &e2e_ask(&worktree)).await.unwrap();
    assert_eq!(answer(&mut visit).await, Some(Answer::Granted));
    eventually("ssh never started", || stand.ssh_pid().is_some()).await;
    let ssh = stand.ssh_pid().unwrap();

    tokio::time::advance(CEILING + Duration::from_secs(1)).await;

    assert_eq!(
        until_done(&mut visit).await,
        [
            Answer::Output("e2e: ended after 30 minutes".into()),
            Answer::Done(TIMED_OUT)
        ]
    );
    eventually("ssh was left running", || ended(ssh)).await;
    eventually("the host was not given back", || {
        e2e_holders(&desk, "e2e-box") == 0
    })
    .await;
}
