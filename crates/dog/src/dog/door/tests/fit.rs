// Real time, as the other door tests: a paused clock would run the door's
// ask timeout out while the blocking client connects.

use super::*;
use crate::agent_board::{Finding, Measure};
use crate::agents::Agents;
use crate::dog::board::tests::finished;
use crate::dog::board::{Board, load_marks, refresh};
use crate::lease::door::{FitAsk, Fits};
use crate::lease::door_pick::fits;
use crate::settings::AgentName;
use crate::test::write_in;

// An implementer on the stand-in harness whose file names `fit`'s lines.
fn implementer(fit: &str) -> String {
    format!(
        "---\nrole: implementer\nharness: stand-in\nmodel: lite\neffort: low\n\
         usage: none\n{fit}---\n"
    )
}

// `slow`, which takes `fix` and `docs` and takes twice as long at it, and
// `quick`, which takes `fix` alone.
fn pair(dir: &Path) -> Agents {
    write_in(dir, "slow.md", &implementer("kinds: [fix, docs]\n"));
    write_in(dir, "quick.md", &implementer("kinds: [fix]\n"));
    Agents::load(dir).unwrap()
}

// `home`, the kelpie home of a test's dog, with the `dog` folder the dog's own
// start makes for its book and its marks.
fn home_in(dir: &Path) -> PathBuf {
    let home = dir.join("kelpie");
    std::fs::create_dir_all(home.join("dog")).unwrap();
    home
}

// The board of `agents`, with ten items of `fix` each for the pair, `slow`'s
// taking twelve times as long as `quick`'s.
fn board_of(agents: &Agents, home: &Path) -> Arc<Mutex<Board>> {
    let board = Arc::new(Mutex::new(Board::new(agents)));
    finished(home, "koji", ("slow", "fix"), 10, (7200, 0.5));
    finished(home, "koji", ("quick", "fix"), 10, (600, 0.5));
    refresh(&board, home);
    board
}

// What the dog at `socket` says about `agent` and `kind`. The client blocks,
// so it runs off the runtime the door is served on.
async fn asked(socket: &Path, agent: &str, kind: &str) -> Fits {
    let socket = socket.to_owned();
    let item = FitAsk {
        agent: AgentName::try_from(agent.to_owned()).unwrap(),
        kind: kind.to_owned(),
    };
    off_the_runtime(move || fits(&socket, &item))
        .await
        .expect("the dog answered")
}

// The finding the pair gives: `slow` takes twelve times as long at `fix`.
fn finding() -> Finding {
    Finding {
        kind: "fix".into(),
        worse: "slow".into(),
        better: "quick".into(),
        measure: Measure::Hours {
            worse: 2.0,
            better: 1.0 / 6.0,
        },
        suggested_kinds: vec!["docs".into()],
    }
}

// A dog's door at `socket`, with its kelpie home at `home`, answering a fit
// ask from `board`.
fn door_at(socket: &Path, home: &Path, board: Arc<Mutex<Board>>) {
    let dir = socket.parent().unwrap();
    std::fs::create_dir_all(dir).unwrap();
    let desk = kept(dir, 3);
    let listener = open(socket).unwrap();
    tokio::spawn(serve(
        listener,
        desk,
        Duration::ZERO,
        home.to_owned(),
        Remote::default(),
        Arc::new(Mutex::new(Availability::default())),
        board,
    ));
}

#[tokio::test]
async fn a_pair_is_answered_once_and_again_only_after_ten_more_items() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_in(dir.path());
    let agents = pair(dir.path());
    let board = board_of(&agents, &home);
    let (socket, _desk) = door_in(dir.path(), 3, Availability::default(), Arc::clone(&board));

    let first = asked(&socket, "slow", "fix").await;
    let again = asked(&socket, "slow", "fix").await;

    assert_eq!(
        first,
        Fits::No(finding()),
        "ten items of the kind is the reading"
    );
    assert_eq!(again, Fits::Yes, "the dog answered this pair just now");
    finished(&home, "koji", ("slow", "fix"), 9, (7200, 0.5));
    refresh(&board, &home);
    let nine_more = asked(&socket, "slow", "fix").await;
    finished(&home, "koji", ("slow", "fix"), 1, (7200, 0.5));
    refresh(&board, &home);
    let ten_more = asked(&socket, "slow", "fix").await;
    assert_eq!(nine_more, Fits::Yes, "nineteen items is not ten more");
    assert_eq!(ten_more, Fits::No(finding()), "twenty is");
}

#[tokio::test]
async fn the_marks_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_in(dir.path());
    let agents = pair(dir.path());
    let board = board_of(&agents, &home);
    let first = dir.path().join("first/lease.sock");
    door_at(&first, &home, Arc::clone(&board));
    assert_eq!(asked(&first, "slow", "fix").await, Fits::No(finding()));

    // The next dog: the same ledgers, and the marks file the first one wrote.
    let mut next = Board::new(&agents);
    load_marks(&mut next, &home);
    let second = dir.path().join("second/lease.sock");
    door_at(&second, &home, Arc::new(Mutex::new(next)));

    let raised = asked(&second, "slow", "fix").await;
    assert_eq!(
        (raised, std::fs::read_dir(home.join("dog")).unwrap().count()),
        (Fits::Yes, 1),
        "the next dog read the mark the first one left"
    );
}

#[tokio::test]
async fn an_agent_with_no_record_of_the_kind_still_fits_it() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_in(dir.path());
    let agents = pair(dir.path());
    let board = board_of(&agents, &home);
    let (socket, _desk) = door_in(dir.path(), 3, Availability::default(), Arc::clone(&board));

    let docs = asked(&socket, "slow", "docs").await;
    let other = asked(&socket, "other", "fix").await;

    assert_eq!((docs, other), (Fits::Yes, Fits::Yes));
}

#[tokio::test]
async fn an_ask_with_no_agent_and_kind_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, _desk) = door_in(dir.path(), 3, Availability::default(), no_board());
    let ask = Ask::new(FIT, std::process::id(), "fit");
    let mut visit = Visit::knock(&socket, &ask).await.unwrap();

    let Some(Answer::Error(why)) = answer(&mut visit).await else {
        panic!("the door let a fit ask through")
    };
    assert!(why.contains("names the agent and the kind"), "{why}");
}
