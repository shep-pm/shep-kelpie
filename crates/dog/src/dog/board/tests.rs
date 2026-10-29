use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::dog::availability::{Availability, Readers, Sources, read};
use crate::ports::{Meter, Timestamp};
use crate::settings::{Account, Gateway, GatewayName, Gateways};
use crate::test::{EPOCH, FakeClock, FakeMeter, LeaseReads, utilization, write_in};

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

// Root reads a folder whatever its mode, so only another user sees the failure.
#[cfg(unix)]
#[test]
fn a_ledger_that_cannot_be_looked_at_keeps_its_last_lines() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("kelpie");
    finished(&home, "koji", ("sonnet-high", "fix"), 2, (600, 0.5));
    let board = Mutex::new(Board::new(&Agents::embedded()));
    refresh(&board, &home);
    let mode = |path: &Path, mode| fs::set_permissions(path, fs::Permissions::from_mode(mode));

    for shut in [home.clone(), home.join("koji")] {
        mode(&shut, 0o000).unwrap();
        let read = refresh(&board, &home);
        mode(&shut, 0o755).unwrap();
        assert_eq!((read, items(&board)), (0, 2), "{}", shut.display());
    }

    fs::remove_file(home.join("koji").join(usage::FILE)).unwrap();
    refresh(&board, &home);
    assert_eq!(items(&board), 0, "a ledger that is gone");
}

// What the dog has read once of `agents`' models and plans, with Claude's
// plan at `session` percent of its 5-hour window.
async fn seen(agents: &Agents, gateways: &Gateways, session: u32) -> Availability {
    let sources = Sources::of(agents, gateways);
    let shown = Mutex::new(Availability::new(&sources));
    let claude = FakeMeter::idle();
    claude.set(utilization(10, session));
    let readers = Readers {
        leases: Arc::new(LeaseReads::default()),
        meters: BTreeMap::from([(Account::Claude, Arc::new(claude) as Arc<dyn Meter>)]),
        clock: Arc::new(FakeClock::at(EPOCH)),
    };
    read(&shown, &sources, &readers).await;
    shown.into_inner().unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_plan_past_half_its_window_waits_until_the_window_resets() {
    let agents = Agents::embedded();
    let view = Board::new(&agents).view();
    let now = Timestamp(EPOCH);

    let past = view.status(&seen(&agents, &Gateways::default(), 60).await, now);
    let under = view.status(&seen(&agents, &Gateways::default(), 40).await, now);

    assert_eq!(
        past["sonnet-high"]["wait"],
        5 * 3600,
        "its window resets in 5 hours"
    );
    assert_eq!(under["sonnet-high"]["wait"], 0);
}

#[tokio::test(start_paused = true)]
async fn a_model_behind_a_gateway_with_no_load_to_read_waits_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let on = |gateway: &str| {
        format!(
            "---\nrole: implementer\nharness: pi\nmodel: coder\neffort: low\n\
             gateway: {gateway}\ncontext: 65536\n---\n"
        )
    };
    write_in(dir.path(), "lite.md", &on("lite"));
    write_in(dir.path(), "local.md", &on("paddock"));
    let agents = Agents::load(dir.path()).unwrap();
    let table: BTreeMap<GatewayName, Gateway> = toml::from_str(
        "[paddock]\nurl = \"http://gpu:8700\"\nkey_env = \"PADDOCK_KEY\"\n\
         [lite]\nkind = \"openai\"\nurl = \"http://lite:4000\"\nkey_env = \"LITE_KEY\"\n",
    )
    .unwrap();
    let gateways = Gateways::reading(table, |_| Some("key".to_owned()));

    let status = Board::new(&agents)
        .view()
        .status(&seen(&agents, &gateways, 0).await, Timestamp(EPOCH));

    assert_eq!(status["lite"]["wait"], 0);
    assert_eq!(
        status["local"]["wait"],
        Value::Null,
        "paddock's load is unread"
    );
}

#[test]
fn an_agent_file_named_with_an_underscore_is_no_implementer_the_board_ranks() {
    let dir = tempfile::tempdir().unwrap();
    let lite = "---\nrole: implementer\nharness: stand-in\nmodel: lite\neffort: low\n\
                usage: none\n---\n";
    write_in(dir.path(), "_lite.md", lite);
    let loaded = crate::dog::agents_in(dir.path());

    let ranked = |agents: &Agents| {
        Board::new(agents)
            .view()
            .ranked
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };

    assert_eq!(ranked(&loaded), ranked(&Agents::embedded()));
}

// An implementer and a reviewer of the same tier, each on a model the dog
// reads no load for.
fn build_and_read(dir: &Path) -> Agents {
    let at = |role: &str| {
        format!(
            "---\nrole: {role}\nharness: stand-in\nmodel: lite\neffort: low\n\
             usage: none\ntier: standard\n---\nRead the diff for defects.\n"
        )
    };
    let folder = dir.join("agents");
    write_in(&folder, "build.md", &at("implementer"));
    write_in(&folder, "read.md", &at("reviewer"));
    Agents::load(&folder).unwrap()
}

#[test]
fn the_board_status_lists_implementers_alone() {
    let dir = tempfile::tempdir().unwrap();
    let agents = build_and_read(dir.path());
    let view = Board::new(&agents).view();

    let status = view.status(&Availability::default(), Timestamp(EPOCH));
    let shown: Vec<_> = status
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    let builders: Vec<_> = (agents.iter())
        .filter(|(_, agent)| agent.role == Role::Implementer)
        .map(|(name, _)| name.as_str())
        .collect();
    let ranked: Vec<_> = view.ranked.keys().map(|k| k.as_str()).collect();

    assert_eq!(shown, builders, "every implementer and no reviewer");
    assert!(ranked.contains(&"read"), "the board ranks a reviewer");
}

#[test]
fn a_reviewers_finished_line_counts_toward_no_tier_median() {
    let dir = tempfile::tempdir().unwrap();
    let agents = build_and_read(dir.path());
    let home = dir.path().join("kelpie");
    let board = Mutex::new(Board::new(&agents));
    finished(&home, "koji", ("build", "fix"), 4, (600, 0.5));
    finished(&home, "koji", ("read", "fix"), 5, (600, 0.5));
    refresh(&board, &home);

    let status = lock(&board)
        .view()
        .status(&Availability::default(), Timestamp(EPOCH));

    assert_eq!(
        status["build"]["secs"], 3600,
        "four items of its own and no tier median of five"
    );
    assert_eq!(status["build"]["cost_usd"], Value::Null);
}

#[test]
fn status_shows_each_agents_records_by_kind() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    finished(home, "koji", ("sonnet-high", "fix"), 2, (7200, 0.5));
    finished(home, "golbat", ("sonnet-high", "fix"), 1, (3600, 1.5));
    finished(home, "golbat", ("sonnet-high", "docs"), 1, (900, 1.0));
    finished(home, "golbat", ("opus-high", "fix"), 1, (600, 2.0));
    let board = Mutex::new(Board::new(&Agents::embedded()));
    refresh(&board, home);

    let records = lock(&board).view().records();

    assert_eq!(
        records["sonnet-high"]["fix"],
        json!({
            "items": 3, "merged": 3, "closed": 0, "merge_rate": 1.0,
            "median_hours": 2.0, "review_rounds": 0, "fix_turns": 0,
            "heavy_fix_items": 0, "rulings": 0, "median_cost_usd": 0.5,
        }),
        "every project's items of the agent and kind, in one record"
    );
    assert_eq!(records["sonnet-high"]["docs"]["items"], 1);
    assert_eq!(records["opus-high"]["fix"]["items"], 1);
}
