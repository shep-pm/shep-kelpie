use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

use serde_json::json;

use super::*;

// Adds `n` merged items of `agent` and `kind` to `project`'s ledger under
// `home`, each taking `wall` seconds over four worker turns and costing `cost_usd`.
pub(in crate::dog) fn finished(
    home: &Path,
    project: &str,
    (agent, kind): (&str, &str),
    n: usize,
    (wall, cost_usd): (u64, f64),
) {
    let role = |calls: u32| {
        let tokens = json!({ "input": 0, "cache_write": 0, "cache_read": 0, "output": 0 });
        json!({ "calls": calls, "tokens": tokens, "units": 0, "cost_usd": 0.0 })
    };
    let line = json!({
        "line": "finished", "at": 1, "issue": 7, "title": "t", "pull_request": 7,
        "merged": true, "wall": wall, "units": 0, "cost_usd": cost_usd,
        "worker": role(4), "reviewer": role(1), "qwen": { "rounds": 0, "seconds": 0 },
        "review_rounds": 0, "fix_turns": 0, "rulings": 0, "agent": agent, "kind": kind,
    });
    let folder = home.join(project);
    fs::create_dir_all(&folder).unwrap();
    let mut ledger = (OpenOptions::new().create(true).append(true))
        .open(folder.join(usage::FILE))
        .unwrap();
    for _ in 0..n {
        writeln!(ledger, "{line}").unwrap();
    }
}

fn items(board: &Mutex<Board>) -> usize {
    lock(board).view().ledgers.iter().map(|l| l.len()).sum()
}

#[test]
fn a_ledger_is_read_again_only_once_it_changes() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    fs::create_dir_all(home.join("agents")).unwrap();
    finished(home, "koji", ("sonnet-high", "fix"), 2, (600, 0.5));
    let board = Mutex::new(Board::new(&Agents::embedded()));

    assert_eq!(
        refresh(&board, home),
        1,
        "koji's ledger, and no other folder"
    );
    assert_eq!(refresh(&board, home), 0, "nothing changed");
    assert_eq!(items(&board), 2);

    finished(home, "koji", ("sonnet-high", "fix"), 1, (600, 0.5));
    finished(home, "golbat", ("opus-high", "docs"), 1, (900, 1.0));
    assert_eq!(refresh(&board, home), 2);
    assert_eq!(items(&board), 4);

    fs::remove_file(home.join("golbat").join(usage::FILE)).unwrap();
    assert_eq!(refresh(&board, home), 0);
    assert_eq!(items(&board), 3, "a ledger that is gone leaves the board");
}
