// Real time, as the other door tests: a paused clock would run the door's
// ask timeout out while the blocking client connects.

use std::collections::BTreeMap;

use super::*;
use crate::agents::{Agents, Tier};
use crate::dog::availability::{Readers, Sources, read};
use crate::dog::board::refresh;
use crate::dog::board::tests::finished;
use crate::forwarder::Upstream;
use crate::lease::door::{Pick, PickAsk};
use crate::lease::door_pick::pick;
use crate::ports::{LocalLeases, Meter, ModelLoad, PaddockHolder, Utilization};
use crate::settings::{Account, AgentName, Gateway, GatewayName, Gateways};
use crate::test::{FakeMeter, utilization, write_in};

const FREE: ModelLoad = ModelLoad {
    slots: 1,
    held: 0,
    waiting: 0,
};

// One turn running and two waiting on the model's one slot.
const BUSY: ModelLoad = ModelLoad {
    slots: 1,
    held: 1,
    waiting: 2,
};

// A paddock whose models each have their own load, and any other unreadable.
struct Paddock(BTreeMap<&'static str, ModelLoad>);

impl LocalLeases for Paddock {
    fn paddock_holder(&self, _: &Upstream, _: &str) -> Option<PaddockHolder> {
        None
    }

    fn paddock_load(&self, _: &Upstream, model: &str) -> Option<ModelLoad> {
        self.0.get(model).copied()
    }
}

// An implementer on paddock's `model`, with the lines `fit` adds to its file.
fn on_paddock(model: &str, fit: &str) -> String {
    format!(
        "---\nrole: implementer\nharness: pi\nmodel: {model}\neffort: low\n\
         gateway: paddock\ncontext: 65536\n{fit}---\n"
    )
}

// A reviewer on paddock's `model`, with the lines `fit` adds to its file.
fn review_on_paddock(model: &str, fit: &str) -> String {
    format!(
        "---\nrole: reviewer\nharness: pi\nmodel: {model}\neffort: low\n\
         gateway: paddock\ncontext: 65536\n{fit}---\nRead the diff for defects.\n"
    )
}

// A reviewer on a Claude Code session, which spends the Claude plan.
fn review_on_plan(fit: &str) -> String {
    format!(
        "---\nrole: reviewer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
         effort: high\n{fit}---\nRead the diff for defects.\n"
    )
}

struct Dog {
    // Each folder lives as long as the test.
    _folders: [tempfile::TempDir; 2],
    home: PathBuf,
    board: Arc<Mutex<Board>>,
    socket: PathBuf,
}

// A dog that knows kelpie's own agents and `files`, has read each paddock
// model at its `loads`, and reads ledgers under its `home`.
async fn dog(files: &[(&str, String)], loads: &[(&'static str, ModelLoad)]) -> Dog {
    dog_at(files, loads, &[]).await
}

// `dog`, with each account named in `plans` read once at that utilization
// before its first ask.
async fn dog_at(
    files: &[(&str, String)],
    loads: &[(&'static str, ModelLoad)],
    plans: &[(Account, Utilization)],
) -> Dog {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("agents");
    for (name, text) in files {
        write_in(&folder, name, text);
    }
    let agents = Agents::load(&folder).unwrap();
    let table: BTreeMap<GatewayName, Gateway> =
        toml::from_str("[paddock]\nurl = \"http://gpu:8700\"\nkey_env = \"PADDOCK_KEY\"\n")
            .unwrap();
    let sources = Sources::of(&agents, &Gateways::reading(table, |_| Some("key".into())));
    let shown = Mutex::new(Availability::new(&sources));
    let mut meters: BTreeMap<Account, Arc<dyn Meter>> = BTreeMap::new();
    for (account, usage) in plans {
        let meter = FakeMeter::idle();
        meter.set(*usage);
        meters.insert(*account, Arc::new(meter));
    }
    let readers = Readers {
        leases: Arc::new(Paddock(loads.iter().copied().collect())),
        meters,
        clock: Arc::new(FakeClock::at(EPOCH)),
    };
    read(&shown, &sources, &readers).await;
    let board = Arc::new(Mutex::new(Board::new(&agents)));
    let shown = shown.into_inner().unwrap();
    let (door_dir, socket, _desk) = door_seeing(3, shown, Arc::clone(&board));
    Dog {
        home: dir.path().join("kelpie"),
        _folders: [dir, door_dir],
        board,
        socket,
    }
}

fn name(agent: &str) -> AgentName {
    AgentName::try_from(agent.to_owned()).unwrap()
}

fn item(kind: Option<&str>, agents: &[&str]) -> PickAsk {
    PickAsk {
        kind: kind.map(str::to_owned),
        tier: None,
        pin: None,
        slack: None,
        agents: agents.iter().map(|a| name(a)).collect(),
        leave_out: Vec::new(),
    }
}

async fn picked(socket: &Path, item: PickAsk) -> Vec<Pick> {
    let socket = socket.to_owned();
    off_the_runtime(move || pick(&socket, &item))
        .await
        .expect("the dog answered")
}

fn agents_of(picks: &[Pick]) -> Vec<&str> {
    picks.iter().map(|p| p.agent.as_str()).collect()
}

#[tokio::test]
async fn a_free_agent_with_history_is_ranked_above_a_busy_one() {
    let files = [
        ("fast.md", on_paddock("fast-coder", "")),
        ("busy.md", on_paddock("busy-coder", "tier: heavy\n")),
    ];
    let loads = [("fast-coder", FREE), ("busy-coder", BUSY)];
    let dog = dog(&files, &loads).await;
    finished(&dog.home, "koji", ("fast", "fix"), 5, (600, 0.25));
    refresh(&dog.board, &dog.home);

    let picks = picked(&dog.socket, item(Some("fix"), &["busy", "fast"])).await;

    let fast = Pick {
        agent: name("fast"),
        finish: 600,
        wait: 0,
        cost_usd: Some(0.25),
    };
    // Three turns ahead on one slot, each the hour a turn takes with no history.
    let busy = Pick {
        agent: name("busy"),
        finish: 4 * 3600,
        wait: 3 * 3600,
        cost_usd: None,
    };
    assert_eq!(picks, [fast, busy]);
}

#[tokio::test]
async fn a_local_agent_with_no_history_is_ranked_as_free_within_the_slack() {
    let files = [
        ("fast.md", on_paddock("fast-coder", "")),
        ("fresh.md", on_paddock("fresh-coder", "tier: light\n")),
    ];
    let loads = [("fast-coder", FREE), ("fresh-coder", FREE)];
    let dog = dog(&files, &loads).await;
    finished(&dog.home, "koji", ("fast", "fix"), 5, (600, 0.25));
    refresh(&dog.board, &dog.home);

    let picks = picked(&dog.socket, item(Some("fix"), &["fast", "fresh"])).await;

    assert_eq!(agents_of(&picks), ["fresh", "fast"]);
}

#[tokio::test]
async fn an_agent_whose_load_is_unknown_is_not_picked() {
    let files = [
        ("fast.md", on_paddock("fast-coder", "")),
        ("lost.md", on_paddock("lost-coder", "")),
    ];
    let dog = dog(&files, &[("fast-coder", FREE)]).await;

    let picks = picked(&dog.socket, item(None, &["fast", "lost"])).await;

    assert_eq!(agents_of(&picks), ["fast"]);
}

#[tokio::test]
async fn a_pin_keeps_only_the_pinned_agent() {
    let files = [
        ("fast.md", on_paddock("fast-coder", "")),
        ("busy.md", on_paddock("busy-coder", "")),
    ];
    let loads = [("fast-coder", FREE), ("busy-coder", BUSY)];
    let dog = dog(&files, &loads).await;
    let pinned = PickAsk {
        pin: Some(name("busy")),
        ..item(None, &["fast", "busy"])
    };

    let picks = picked(&dog.socket, pinned).await;

    assert_eq!(agents_of(&picks), ["busy"]);
}

#[tokio::test]
async fn an_agent_the_ask_does_not_name_is_never_ranked() {
    let files = [
        ("fast.md", on_paddock("fast-coder", "")),
        ("busy.md", on_paddock("busy-coder", "")),
    ];
    let loads = [("fast-coder", FREE), ("busy-coder", BUSY)];
    let dog = dog(&files, &loads).await;

    let picks = picked(&dog.socket, item(None, &["busy", "unknown-agent"])).await;

    assert_eq!(agents_of(&picks), ["busy"]);
    let none = picked(&dog.socket, item(None, &[])).await;
    assert_eq!(none, [], "no agent allowed is an answer");
}

#[tokio::test]
async fn a_new_finished_line_changes_the_next_pick_after_a_refresh() {
    let files = [
        ("able.md", on_paddock("able-coder", "tier: light\n")),
        ("baker.md", on_paddock("baker-coder", "tier: heavy\n")),
    ];
    let loads = [("able-coder", FREE), ("baker-coder", FREE)];
    let dog = dog(&files, &loads).await;
    finished(&dog.home, "koji", ("baker", "fix"), 4, (600, 0.5));
    refresh(&dog.board, &dog.home);
    // With no slack, the cost of a local agent with no history cannot reorder them.
    let ask = || PickAsk {
        slack: Some(0),
        ..item(Some("fix"), &["able", "baker"])
    };
    let before = picked(&dog.socket, ask()).await;
    assert_eq!(
        agents_of(&before),
        ["able", "baker"],
        "four items say nothing"
    );

    finished(&dog.home, "koji", ("baker", "fix"), 1, (600, 0.5));
    let unread = picked(&dog.socket, ask()).await;
    refresh(&dog.board, &dog.home);
    let after = picked(&dog.socket, ask()).await;

    assert_eq!(unread, before, "the ledger is read on the tick");
    assert_eq!(agents_of(&after), ["baker", "able"]);
    assert_eq!(after[0].cost_usd, Some(0.5));
}

#[tokio::test]
async fn an_ask_naming_reviewers_ranks_a_local_one_before_a_hosted_one() {
    let files = [
        ("hosted.md", review_on_plan("")),
        ("local.md", review_on_paddock("rev-coder", "")),
    ];
    let dog = dog_at(
        &files,
        &[("rev-coder", FREE)],
        &[(Account::Claude, utilization(10, 40))],
    )
    .await;

    let picks = picked(&dog.socket, item(None, &["hosted", "local"])).await;

    assert_eq!(agents_of(&picks), ["local", "hosted"]);
    assert_eq!(
        picks
            .iter()
            .map(|p| (p.finish, p.cost_usd))
            .collect::<Vec<_>>(),
        [(3600, None), (3600, None)],
        "each takes the hour an item with no history takes, and the tier ranks without reporting a cost"
    );
}

#[tokio::test]
async fn a_reviewer_on_a_held_plan_finishes_after_its_hold() {
    let files = [("held.md", review_on_plan(""))];
    let dog = dog_at(&files, &[], &[(Account::Claude, utilization(10, 60))]).await;

    let picks = picked(&dog.socket, item(None, &["held"])).await;

    assert_eq!(
        picks,
        [Pick {
            agent: name("held"),
            finish: 6 * 3600,
            wait: 5 * 3600,
            cost_usd: None,
        }],
        "the hold ends when the plan's window resets"
    );
}

#[tokio::test]
async fn a_reviewer_below_the_asks_tier_is_not_ranked() {
    let files = [
        (
            "light.md",
            review_on_paddock("light-coder", "tier: light\n"),
        ),
        (
            "heavy.md",
            review_on_paddock("heavy-coder", "tier: heavy\n"),
        ),
    ];
    let loads = [("light-coder", FREE), ("heavy-coder", FREE)];
    let dog = dog(&files, &loads).await;
    let ask = PickAsk {
        tier: Some(Tier::Standard),
        ..item(None, &["light", "heavy"])
    };

    let picks = picked(&dog.socket, ask).await;

    assert_eq!(agents_of(&picks), ["heavy"]);
}
