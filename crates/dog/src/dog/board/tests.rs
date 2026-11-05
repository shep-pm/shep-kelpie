use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::api_refusal::{CREDIT_RECHECK_SECS, Refusal};
use crate::dog::availability::{Availability, Readers, Sources, read};
use crate::lease::door::PickAsk;
use crate::ports::{Meter, Timestamp, Utilization};
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
    seen_plans(
        agents,
        gateways,
        utilization(10, session),
        utilization(0, 0),
    )
    .await
}

// What the dog has read once of `agents`' models and the Claude and Codex plans.
async fn seen_plans(
    agents: &Agents,
    gateways: &Gateways,
    claude: Utilization,
    codex: Utilization,
) -> Availability {
    let sources = Sources::of(agents, gateways);
    let shown = Mutex::new(Availability::new(&sources));
    let meter = |usage| {
        let meter = FakeMeter::idle();
        meter.set(usage);
        Arc::new(meter) as Arc<dyn Meter>
    };
    let readers = Readers {
        leases: Arc::new(LeaseReads::default()),
        meters: BTreeMap::from([
            (Account::Claude, meter(claude)),
            (Account::Codex, meter(codex)),
        ]),
        clock: Arc::new(FakeClock::at(EPOCH)),
    };
    read(&shown, &sources, &readers).await;
    shown.into_inner().unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_plan_past_half_its_window_waits_until_the_window_resets() {
    let (_dir, agents) = crate::test::claude_implementers();
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
async fn a_plan_whose_week_is_spent_waits_until_the_week_resets() {
    let dir = tempfile::tempdir().unwrap();
    let on = |usage: &str| {
        format!(
            "---\nrole: implementer\nharness: stand-in\nmodel: m\neffort: low\n\
             usage: {usage}\n---\n"
        )
    };
    write_in(dir.path(), "on-claude.md", &on("claude"));
    write_in(dir.path(), "on-codex.md", &on("codex"));
    let agents = Agents::load(dir.path()).unwrap();
    let now = Timestamp(EPOCH);
    let week_resets = 7 * 86_400;
    // The week is spent on Claude's plan, with the 5-hour window nearly empty
    let shown = seen_plans(
        &agents,
        &Gateways::default(),
        utilization(100, 19),
        utilization(40, 19),
    )
    .await;
    let view = Board::new(&agents).view();

    let status = view.status(&shown, now);
    let ask = PickAsk {
        kind: None,
        tier: None,
        pin: None,
        slack: None,
        agents: ["on-claude", "on-codex"].map(AgentName::kelpies).to_vec(),
        leave_out: vec![],
    };
    let picks = pick::answer(&view, &shown, &ask, now);

    assert_eq!(status["on-claude"]["wait"], week_resets);
    assert_eq!(status["on-codex"]["wait"], 0);
    let order: Vec<_> = picks.iter().map(|p| p.agent.as_str()).collect();
    assert_eq!(
        order,
        ["on-codex", "on-claude"],
        "the spent account goes last"
    );
    assert_eq!(picks[1].wait, week_resets);
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

const ON_CLAUDE: &str = "---\nrole: implementer\nharness: claude-code\n\
                         model: claude-sonnet-5-5\neffort: high\n";

// A heavy plan agent, a light API agent on the same model and a light local one. Only the
// plan agent has a history to price.
fn plan_api_and_local(dir: &Path) -> Agents {
    write_in(dir, "plan.md", &format!("{ON_CLAUDE}tier: heavy\n---\n"));
    write_in(
        dir,
        "billed.md",
        &format!("{ON_CLAUDE}login: api\ntier: light\n---\n"),
    );
    write_in(
        dir,
        "local.md",
        "---\nrole: implementer\nharness: stand-in\nmodel: lite\neffort: low\nusage: none\ntier: light\n---\n",
    );
    Agents::load(dir).unwrap()
}

// Every candidate is within the slack, so cost alone orders them.
fn ask_all() -> PickAsk {
    PickAsk {
        kind: None,
        tier: None,
        pin: None,
        slack: Some(100_000),
        agents: ["plan", "billed", "local"].map(AgentName::kelpies).to_vec(),
        leave_out: vec![],
    }
}

#[tokio::test(start_paused = true)]
async fn an_api_agent_is_not_ranked_as_a_local_model() {
    let dir = tempfile::tempdir().unwrap();
    let agents = plan_api_and_local(&dir.path().join("agents"));
    let home = dir.path().join("kelpie");
    let board = Mutex::new(Board::new(&agents));
    finished(&home, "koji", ("plan", "fix"), 5, (600, 3.0));
    refresh(&board, &home);
    let shown = seen(&agents, &Gateways::default(), 10).await;

    let picks = pick::answer(&lock(&board).view(), &shown, &ask_all(), Timestamp(EPOCH));

    let order: Vec<_> = picks.iter().map(|p| p.agent.as_str()).collect();
    assert_eq!(
        order,
        ["local", "plan", "billed"],
        "with no history the API agent is not free, as the local model is"
    );
}

#[tokio::test(start_paused = true)]
async fn an_api_agent_waits_nothing_while_the_plan_is_spent() {
    let dir = tempfile::tempdir().unwrap();
    let agents = plan_api_and_local(dir.path());
    let shown = seen_plans(
        &agents,
        &Gateways::default(),
        utilization(100, 100),
        utilization(0, 0),
    )
    .await;
    let now = Timestamp(EPOCH);

    let status = Board::new(&agents).view().status(&shown, now);
    let picks = pick::answer(&Board::new(&agents).view(), &shown, &ask_all(), now);

    assert_eq!(status["plan"]["wait"], 7 * 86_400, "the plan's week resets");
    assert_eq!(status["billed"]["wait"], 0);
    let billed = picks.iter().find(|p| p.agent.as_str() == "billed").unwrap();
    assert_eq!(billed.wait, 0);
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

// Two plan agents and an API agent, all standard-tier implementers on one model.
fn two_plans_and_a_billed(dir: &Path) -> Agents {
    write_in(dir, "plan-a.md", &format!("{ON_CLAUDE}---\n"));
    write_in(dir, "plan-b.md", &format!("{ON_CLAUDE}---\n"));
    write_in(dir, "billed.md", &format!("{ON_CLAUDE}login: api\n---\n"));
    Agents::load(dir).unwrap()
}

fn ask_of(agents: &[&'static str]) -> PickAsk {
    PickAsk {
        kind: Some("fix".into()),
        tier: None,
        pin: None,
        slack: Some(100_000),
        agents: agents.iter().map(|a| AgentName::kelpies(a)).collect(),
        leave_out: vec![],
    }
}

fn order(picks: &[crate::lease::door::Pick]) -> Vec<&str> {
    picks.iter().map(|p| p.agent.as_str()).collect()
}

#[tokio::test(start_paused = true)]
async fn an_api_agent_ranks_as_any_other() {
    let dir = tempfile::tempdir().unwrap();
    let agents = two_plans_and_a_billed(&dir.path().join("agents"));
    let home = dir.path().join("kelpie");
    finished(&home, "koji", ("plan-a", "fix"), 5, (600, 1.0));
    finished(&home, "koji", ("plan-b", "fix"), 5, (600, 2.0));
    let shown = seen(&agents, &Gateways::default(), 10).await;
    let board = Mutex::new(Board::new(&agents));
    refresh(&board, &home);
    let view = lock(&board).view();
    let ask = ask_of(&["plan-a", "plan-b", "billed"]);
    let pinned = PickAsk {
        pin: Some(AgentName::kelpies("billed")),
        ..ask.clone()
    };

    let picks = pick::answer(&view, &shown, &ask, Timestamp(EPOCH));
    let one = pick::answer(&view, &shown, &pinned, Timestamp(EPOCH));

    assert_eq!(
        order(&picks),
        ["plan-a", "billed", "plan-b"],
        "the API agent is ranked on its tier's median cost, as a plan agent is"
    );
    assert_eq!(order(&one), ["billed"], "a pinned API agent is picked");
}

fn names(list: &[&'static str]) -> Vec<AgentName> {
    list.iter().map(|a| AgentName::kelpies(a)).collect()
}

// Two plan agents of known cost and an API agent, with a ledger that prices only the plan agents.
async fn priced_plans(dir: &Path, prefer: &[&'static str]) -> (View, Availability) {
    let agents = two_plans_and_a_billed(&dir.join("agents"));
    let home = dir.join("kelpie");
    finished(&home, "koji", ("plan-a", "fix"), 5, (600, 1.0));
    finished(&home, "koji", ("plan-b", "fix"), 5, (600, 2.0));
    let shown = seen(&agents, &Gateways::default(), 10).await;
    let board = Mutex::new(Board::new(&agents).preferring(names(prefer)));
    refresh(&board, &home);
    let view = lock(&board).view();
    (view, shown)
}

#[tokio::test(start_paused = true)]
async fn a_listed_api_agent_ranks_first_and_an_unlisted_one_ranks_as_any_other() {
    let dir = tempfile::tempdir().unwrap();
    let (listed, shown) = priced_plans(dir.path(), &["billed"]).await;
    let (unlisted, _) = priced_plans(dir.path(), &["plan-b"]).await;
    let ask = ask_of(&["plan-a", "plan-b", "billed"]);

    let first = pick::answer(&listed, &shown, &ask, Timestamp(EPOCH));
    let other = pick::answer(&unlisted, &shown, &ask, Timestamp(EPOCH));

    assert_eq!(order(&first), ["billed", "plan-a", "plan-b"]);
    assert_eq!(
        order(&other),
        ["plan-b", "plan-a", "billed"],
        "the list names the plan agent, so the API agent follows on its cost as any other"
    );
}

#[test]
fn the_list_orders_reviewers_as_it_orders_implementers() {
    let dir = tempfile::tempdir().unwrap();
    let reader = "---\nrole: reviewer\nharness: stand-in\nmodel: lite\neffort: low\n\
                  usage: none\ntier: standard\n---\nRead the diff for defects.\n";
    let folder = dir.path().join("agents");
    write_in(&folder, "read-a.md", reader);
    write_in(&folder, "read-b.md", reader);
    let agents = Agents::load(&folder).unwrap();
    let ask = ask_of(&["read-a", "read-b"]);
    let ranked = |prefer: &[&'static str]| {
        let view = Board::new(&agents).preferring(names(prefer)).view();
        let picks = pick::answer(&view, &Availability::default(), &ask, Timestamp(EPOCH));
        order(&picks)
            .iter()
            .map(|a| (*a).to_owned())
            .collect::<Vec<_>>()
    };

    assert_eq!(ranked(&[]), ["read-a", "read-b"]);
    assert_eq!(ranked(&["read-b"]), ["read-b", "read-a"]);
}

#[tokio::test(start_paused = true)]
async fn a_new_prefer_list_orders_the_next_pick_and_not_one_already_read() {
    let dir = tempfile::tempdir().unwrap();
    let agents = two_plans_and_a_billed(&dir.path().join("agents"));
    let home = dir.path().join("kelpie");
    finished(&home, "koji", ("plan-a", "fix"), 5, (600, 1.0));
    finished(&home, "koji", ("plan-b", "fix"), 5, (600, 2.0));
    let shown = seen(&agents, &Gateways::default(), 10).await;
    let board = Mutex::new(Board::new(&agents).preferring(names(&["billed"])));
    refresh(&board, &home);
    let ask = ask_of(&["plan-a", "plan-b", "billed"]);
    let before = lock(&board).view();

    lock(&board).set_prefer(names(&["plan-b"]));
    let after = lock(&board).view();

    let old = pick::answer(&before, &shown, &ask, Timestamp(EPOCH));
    let new = pick::answer(&after, &shown, &ask, Timestamp(EPOCH));
    assert_eq!(order(&old), ["billed", "plan-a", "plan-b"]);
    assert_eq!(order(&new), ["plan-b", "plan-a", "billed"]);
}

#[tokio::test(start_paused = true)]
async fn new_agents_order_the_next_pick_and_not_one_already_read() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("agents");
    let agents = two_plans_and_a_billed(&folder);
    let shown = seen(&agents, &Gateways::default(), 10).await;
    let board = Mutex::new(Board::new(&agents));
    let ask = ask_of(&["plan-a", "plan-b", "billed"]);
    let before = lock(&board).view();

    // plan-a now takes only docs, and plan-b is light, so it ranks before billed.
    write_in(
        &folder,
        "plan-a.md",
        &format!("{ON_CLAUDE}kinds: [docs]\n---\n"),
    );
    write_in(
        &folder,
        "plan-b.md",
        &format!("{ON_CLAUDE}tier: light\n---\n"),
    );
    lock(&board).set_agents(&Agents::load(&folder).unwrap());
    let after = lock(&board).view();

    let old = pick::answer(&before, &shown, &ask, Timestamp(EPOCH));
    let new = pick::answer(&after, &shown, &ask, Timestamp(EPOCH));
    assert_eq!(order(&old), ["billed", "plan-a", "plan-b"]);
    assert_eq!(
        order(&new),
        ["plan-b", "billed"],
        "plan-a no longer takes `fix`, and plan-b's lighter tier ranks it first"
    );
}

#[tokio::test(start_paused = true)]
async fn a_handoff_ask_leaves_out_the_agent_left_and_takes_the_next_listed() {
    let dir = tempfile::tempdir().unwrap();
    let (view, shown) = priced_plans(dir.path(), &["plan-b", "plan-a", "billed"]).await;
    let ask = PickAsk {
        leave_out: names(&["plan-b"]),
        ..ask_of(&["plan-a", "plan-b", "billed"])
    };

    let picks = pick::answer(&view, &shown, &ask, Timestamp(EPOCH));

    assert_eq!(order(&picks), ["plan-a", "billed"]);
}

#[tokio::test(start_paused = true)]
async fn a_listed_name_with_no_agent_file_is_ignored_and_the_rest_rank() {
    let dir = tempfile::tempdir().unwrap();
    let (view, shown) = priced_plans(dir.path(), &["ghost", "billed", "gone"]).await;
    let ask = ask_of(&["plan-a", "plan-b", "billed"]);

    let picks = pick::answer(&view, &shown, &ask, Timestamp(EPOCH));

    assert_eq!(order(&picks), ["billed", "plan-a", "plan-b"]);
}

// Held for the whole of a day from `EPOCH`, as a spend limit that resumes then.
const HELD_FOR: u64 = 86_400;

fn limit_hit() -> Refusal {
    Refusal::SpendLimit {
        resumes: Some(Timestamp(EPOCH + HELD_FOR)),
    }
}

// Only an agent finishing within ten minutes of the soonest ranks by cost.
fn ask_soon() -> PickAsk {
    PickAsk {
        slack: Some(10),
        ..ask_all()
    }
}

#[tokio::test(start_paused = true)]
async fn a_held_api_agent_waits_until_the_hold_ends_and_ranks_behind_agents_that_can_start() {
    let dir = tempfile::tempdir().unwrap();
    let agents = plan_api_and_local(dir.path());
    let mut shown = seen(&agents, &Gateways::default(), 10).await;
    let now = Timestamp(EPOCH);
    let view = Board::new(&agents).view();
    let before = pick::answer(&view, &shown, &ask_soon(), now);
    let billed = |picks: &[crate::lease::door::Pick]| {
        picks.iter().find(|p| p.agent.as_str() == "billed").cloned()
    };
    assert_eq!(billed(&before).unwrap().wait, 0);

    shown.refused(limit_hit(), now);
    let picks = pick::answer(&view, &shown, &ask_soon(), now);
    let status = view.status(&shown, now);

    assert_eq!(billed(&picks).unwrap().wait, HELD_FOR);
    assert_eq!(status["billed"]["wait"], HELD_FOR);
    assert_eq!(order(&picks).last(), Some(&"billed"), "{:?}", order(&picks));
    let pinned = PickAsk {
        pin: Some(AgentName::kelpies("billed")),
        ..ask_soon()
    };
    let one = pick::answer(&view, &shown, &pinned, now);
    assert_eq!(
        order(&one),
        ["billed"],
        "a pinned item is not stopped by the hold"
    );

    let ended = Timestamp(EPOCH + HELD_FOR);
    let after = pick::answer(&view, &shown, &ask_soon(), ended);
    assert_eq!(billed(&after).unwrap().wait, 0, "the hold has ended");
}

#[tokio::test(start_paused = true)]
async fn a_short_hold_does_not_lift_an_api_agent_over_agents_that_can_start() {
    let dir = tempfile::tempdir().unwrap();
    let agents = plan_api_and_local(dir.path());
    let mut shown = seen(&agents, &Gateways::default(), 10).await;
    let now = Timestamp(EPOCH);
    let view = Board::new(&agents).view();
    let free = pick::answer(&view, &shown, &ask_all(), now);
    assert_eq!(order(&free), ["local", "billed", "plan"], "ranked on tier");

    shown.refused(
        Refusal::SpendLimit {
            resumes: Some(Timestamp(EPOCH + 60)),
        },
        now,
    );
    let held = pick::answer(&view, &shown, &ask_all(), now);
    assert_eq!(
        order(&held),
        ["local", "plan", "billed"],
        "inside the slack it still goes after both that can start"
    );

    let after = pick::answer(&view, &shown, &ask_all(), Timestamp(EPOCH + 60));
    assert_eq!(
        order(&after),
        ["local", "billed", "plan"],
        "once the hold ends"
    );
}

#[tokio::test(start_paused = true)]
async fn a_hold_reaches_an_api_reviewer_as_it_does_an_implementer() {
    let dir = tempfile::tempdir().unwrap();
    let read = |login: &str| {
        format!(
            "---\nrole: reviewer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
             effort: high\n{login}tier: standard\n---\nRead the diff for defects.\n"
        )
    };
    write_in(dir.path(), "read-api.md", &read("login: api\n"));
    write_in(dir.path(), "read-plan.md", &read(""));
    let agents = Agents::load(dir.path()).unwrap();
    let mut shown = seen(&agents, &Gateways::default(), 10).await;
    shown.refused(limit_hit(), Timestamp(EPOCH));
    let ask = ask_of(&["read-api", "read-plan"]);

    let picks = pick::answer(&Board::new(&agents).view(), &shown, &ask, Timestamp(EPOCH));

    let waits: Vec<_> = picks.iter().map(|p| (p.agent.as_str(), p.wait)).collect();
    assert_eq!(waits, [("read-plan", 0), ("read-api", HELD_FOR)]);
}

#[tokio::test(start_paused = true)]
async fn a_plan_agent_is_not_held_by_an_api_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let agents = plan_api_and_local(dir.path());
    let mut shown = seen(&agents, &Gateways::default(), 10).await;
    shown.refused(Refusal::NoCredit, Timestamp(EPOCH));

    let status = Board::new(&agents).view().status(&shown, Timestamp(EPOCH));

    assert_eq!(status["plan"]["wait"], 0);
    assert_eq!(status["local"]["wait"], 0);
    assert_eq!(status["billed"]["wait"], CREDIT_RECHECK_SECS);
}
