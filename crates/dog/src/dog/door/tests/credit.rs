// Real time, as the other door tests: a paused clock would run the door's
// ask timeout out while the blocking client connects.

use super::*;
use crate::agents::Agents;
use crate::dog::board::refresh;
use crate::dog::board::tests::call;
use crate::lease::door::{CREDIT, Credit};
use crate::lease::door_pick::credit;
use crate::test::{EPOCH, write_in};

// EPOCH is 2026-09-21T14:13:20Z: the first second of its UTC month, and of the month after it
const MONTH: u64 = 1_788_220_800;
const NEXT_MONTH: u64 = 1_790_812_800;

// An implementer billed to the API key, and one on the Claude plan
fn billed_and_plan(dir: &Path) -> Agents {
    let claude = "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
                  effort: high\n";
    write_in(dir, "billed.md", &format!("{claude}login: api\n---\n"));
    write_in(dir, "plan.md", &format!("{claude}---\n"));
    Agents::load(dir).unwrap()
}

// A board with `home`'s two ledgers read once, each with a call of the API agent
fn board_of(agents: &Agents, home: &Path) -> Arc<Mutex<Board>> {
    call(home, "koji", Some("billed"), EPOCH, Some(1.5));
    call(home, "golbat", Some("billed"), EPOCH + 60, Some(2.5));
    call(home, "koji", Some("plan"), EPOCH, Some(9.0));
    let board = Arc::new(Mutex::new(Board::new(agents)));
    refresh(&board, home);
    board
}

#[tokio::test]
async fn the_dog_answers_a_credit_ask_with_the_spend_and_the_months_ends() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("kelpie");
    let agents = billed_and_plan(&dir.path().join("agents"));
    let board = board_of(&agents, &home);
    let (socket, _desk) = door_in(dir.path(), 3, Availability::default(), Arc::clone(&board));

    let asked = off_the_runtime(move || credit(&socket)).await;

    assert_eq!(
        asked,
        Some(Credit {
            spent_usd: 4.0,
            since: MONTH,
            resets_at: NEXT_MONTH,
        }),
        "the desk's clock stands at EPOCH, and only the API agent's calls count"
    );
}

#[tokio::test]
async fn a_credit_ask_before_any_ledger_is_read_answers_zero() {
    let (_dir, socket, _desk) = door(3);

    let asked = off_the_runtime(move || credit(&socket)).await;

    assert_eq!(
        asked,
        Some(Credit {
            spent_usd: 0.0,
            since: MONTH,
            resets_at: NEXT_MONTH,
        }),
        "the dog has read no ledger, so it knows of no spend"
    );
}

#[tokio::test]
async fn a_credit_ask_is_answered_one_line_and_the_door_closes() {
    let (_dir, socket, _desk) = door(3);
    let ask = Ask::new(CREDIT, std::process::id(), "credit");
    let mut visit = Visit::knock(&socket, &ask).await.unwrap();

    let first = answer(&mut visit).await;
    let next = answer(&mut visit).await;

    assert!(matches!(first, Some(Answer::Credit(_))), "{first:?}");
    assert_eq!(
        next, None,
        "the dog closes the connection after its one line"
    );
}
