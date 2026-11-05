use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{Availability, LOAD_TICK, Ports, Readers, Sources, begin, read};
use crate::agents::Agents;
use crate::api_refusal::{CREDIT_RECHECK_SECS, Refusal};
use crate::pacer::RECHECK_SECS;
use crate::ports::{Meter, MeterError, ModelLoad, Timestamp};
use crate::settings::{Account, Gateway, GatewayName, Gateways};
use crate::test::{CALL_PATIENCE, EPOCH, FakeClock, FakeMeter, LeaseReads, utilization, write_in};
use crate::webhook::KelpieSettings;

// One paddock, one OpenAI-compatible gateway, and a key for each.
fn gateways() -> Gateways {
    let table: BTreeMap<GatewayName, Gateway> = toml::from_str(
        "[paddock]\nurl = \"http://gpu:8700\"\nkey_env = \"PADDOCK_KEY\"\n\
         [lite]\nkind = \"openai\"\nurl = \"http://lite:4000\"\nkey_env = \"LITE_KEY\"\n",
    )
    .unwrap();
    Gateways::reading(table, |_| Some("key".to_owned()))
}

fn on_gateway(gateway: &str, model: &str) -> String {
    format!(
        "---\nrole: implementer\nharness: pi\nmodel: {model}\neffort: low\n\
         gateway: {gateway}\ncontext: 65536\n---\n"
    )
}

// Kelpie's own agents, a Claude Code implementer (kelpie ships none, and the Claude plan is
// listed only where an agent runs on it) and `files`, which add to them.
fn agents(files: &[(&str, String)]) -> (tempfile::TempDir, Agents) {
    let dir = tempfile::tempdir().unwrap();
    write_in(
        dir.path(),
        "sonnet-high.md",
        "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-5-5\neffort: high\n---\n",
    );
    for (name, text) in files {
        write_in(dir.path(), name, text);
    }
    let agents = Agents::load(dir.path()).unwrap();
    (dir, agents)
}

fn model(model: &str, shown: Value) -> Value {
    let mut line = json!({ "gateway": "paddock", "model": model });
    line.as_object_mut()
        .unwrap()
        .extend(shown.as_object().unwrap().clone());
    line
}

fn unknown() -> Value {
    json!({ "unknown": true, "read_at": null })
}

fn idle_claude(read_at: u64) -> Value {
    json!({
        "account": "claude",
        "session": { "used_pct": 0, "resets_at": EPOCH + 5 * 3600 },
        "week": { "used_pct": 0, "resets_at": EPOCH + 7 * 86_400 },
        "read_at": read_at,
    })
}

#[test]
fn each_paddock_model_and_each_metered_account_is_listed_once_and_unknown_until_read() {
    let (_dir, agents) = agents(&[
        ("coder.md", on_gateway("paddock", "coder")),
        ("coder-again.md", on_gateway("paddock", "coder")),
        ("lite.md", on_gateway("lite", "lite-coder")),
        ("nowhere.md", on_gateway("nowhere", "lost")),
        (
            "gpt.md",
            "---\nrole: implementer\nharness: codex\nmodel: gpt-6-sol\neffort: medium\n---\n"
                .to_owned(),
        ),
    ]);
    let shown = Availability::new(&Sources::of(&agents, &gateways()));
    let account = |name: &str| json!({ "account": name, "unknown": true, "read_at": null });
    assert_eq!(
        shown.status(),
        json!({
            "models": [model("coder", unknown())],
            "accounts": [account("claude"), account("codex")],
        })
    );
}

#[tokio::test(start_paused = true)]
async fn each_pass_reads_every_model_and_a_meter_at_most_every_recheck() {
    let (_dir, agents) = agents(&[("coder.md", on_gateway("paddock", "coder"))]);
    let sources = Sources::of(&agents, &gateways());
    let shown = Mutex::new(Availability::new(&sources));
    let leases = Arc::new(LeaseReads::default());
    let load = ModelLoad {
        slots: 2,
        held: 1,
        waiting: 3,
    };
    leases.loaded(Some(load));
    let (claude, clock) = (FakeMeter::idle(), FakeClock::at(EPOCH));
    let readers = Readers {
        leases: leases.clone(),
        meters: BTreeMap::from([(Account::Claude, Arc::new(claude.clone()) as Arc<dyn Meter>)]),
        clock: Arc::new(clock.clone()),
    };

    read(&shown, &sources, &readers).await;
    let loaded = |at: u64| json!({ "slots": 2, "held": 1, "waiting": 3, "read_at": at });
    assert_eq!(
        shown.lock().unwrap().status(),
        json!({
            "models": [model("coder", loaded(EPOCH))],
            "accounts": [idle_claude(EPOCH)],
        })
    );
    assert_eq!((leases.count(), claude.reads()), (1, 1));

    clock.advance(RECHECK_SECS - 1);
    read(&shown, &sources, &readers).await;
    assert_eq!(
        (leases.count(), claude.reads()),
        (2, 1),
        "the meter is fresh"
    );
    let status = shown.lock().unwrap().status();
    assert_eq!(status["models"][0]["read_at"], EPOCH + RECHECK_SECS - 1);

    clock.advance(1);
    claude.set(utilization(40, 12));
    read(&shown, &sources, &readers).await;
    assert_eq!((leases.count(), claude.reads()), (3, 2));
    let status = shown.lock().unwrap().status();
    assert_eq!(status["accounts"][0]["week"]["used_pct"], 40);
    assert_eq!(status["accounts"][0]["read_at"], EPOCH + RECHECK_SECS);
}

#[tokio::test(start_paused = true)]
async fn a_failed_load_is_unknown_and_a_failed_meter_keeps_its_last_reading() {
    let (_dir, agents) = agents(&[
        ("coder.md", on_gateway("paddock", "coder")),
        (
            "gpt.md",
            "---\nrole: implementer\nharness: codex\nmodel: gpt-6-sol\neffort: medium\n---\n"
                .to_owned(),
        ),
    ]);
    let sources = Sources::of(&agents, &gateways());
    let shown = Mutex::new(Availability::new(&sources));
    let leases = Arc::new(LeaseReads::default());
    leases.loaded(Some(ModelLoad {
        slots: 1,
        held: 0,
        waiting: 0,
    }));
    let (claude, clock) = (FakeMeter::idle(), FakeClock::at(EPOCH));
    // No Codex meter, as when kelpie's Codex home cannot be worked out.
    let readers = Readers {
        leases: leases.clone(),
        meters: BTreeMap::from([(Account::Claude, Arc::new(claude.clone()) as Arc<dyn Meter>)]),
        clock: Arc::new(clock.clone()),
    };
    read(&shown, &sources, &readers).await;
    let read_at = |status: &Value| status["models"][0]["read_at"].clone();
    assert_eq!(read_at(&shown.lock().unwrap().status()), EPOCH);

    leases.loaded(None);
    claude.fail(MeterError::TimedOut);
    clock.advance(RECHECK_SECS);
    read(&shown, &sources, &readers).await;
    assert_eq!(claude.reads(), 2);
    let codex = json!({ "account": "codex", "unknown": true, "read_at": null });
    let status = shown.lock().unwrap().status();
    assert_eq!(status["models"][0], model("coder", unknown()));
    assert_eq!(status["accounts"], json!([idle_claude(EPOCH), codex]));
}

// Cargo sets it for every test binary it runs, and nothing the dog starts reads it.
const SET: &str = "CARGO_PKG_DESCRIPTION";

#[tokio::test(start_paused = true)]
async fn a_meter_read_never_sees_a_gateway_key() {
    assert!(std::env::var_os(SET).is_some(), "{SET} is set under cargo");
    let section = format!("[gateways.paddock]\nurl = \"http://gpu:8700\"\nkey_env = \"{SET}\"\n");
    let settings = crate::webhook::KelpieSettings::from_section(&section).unwrap();
    let ports = super::Ports {
        local_leases: Arc::new(LeaseReads::default()),
        meter: Arc::new(FakeMeter::idle()),
        codex_meter: |_| Arc::new(FakeMeter::idle()),
    };
    super::begin(&Agents::embedded(), &settings, ports);
    let shown = crate::spawn::command(crate::spawn::env()).output().unwrap();
    let shown = String::from_utf8_lossy(&shown.stdout).into_owned();
    assert!(shown.contains("PATH="), "{shown}");
    assert!(!shown.contains(SET), "a meter's command saw it");
}

#[tokio::test(start_paused = true)]
async fn the_dog_reads_no_account_for_an_api_agent_and_hides_the_api_key_from_its_processes() {
    let dir = tempfile::tempdir().unwrap();
    write_in(
        dir.path(),
        "billed.md",
        "---\nrole: implementer\nharness: claude-code\nmodel: claude-sonnet-5-5\n\
         effort: high\nlogin: api\n---\n",
    );
    let agents = Agents::load(dir.path()).unwrap();
    let sources = Sources::of(&agents, &gateways());
    let status = Availability::new(&sources).status();
    assert_eq!(
        status["accounts"],
        json!([]),
        "an API agent names no account"
    );

    let settings = crate::webhook::KelpieSettings::from_section("").unwrap();
    let ports = super::Ports {
        local_leases: Arc::new(LeaseReads::default()),
        meter: Arc::new(FakeMeter::idle()),
        codex_meter: |_| Arc::new(FakeMeter::idle()),
    };
    super::begin(&agents, &settings, ports);
    // A removed variable shows in the built command as set to nothing.
    let command = crate::spawn::command("env");
    let removed = |var: &str| command.get_envs().any(|(k, v)| k == var && v.is_none());
    assert!(
        removed("ANTHROPIC_API_KEY"),
        "the API key reaches a process"
    );
    assert!(
        removed("CLAUDE_CODE_OAUTH_TOKEN"),
        "the login reaches a process"
    );
}

// A lease reader whose load read panics, as a bug in a parse would.
struct Panics;

impl crate::ports::LocalLeases for Panics {
    fn paddock_holder(
        &self,
        _: &crate::forwarder::Upstream,
        _: &str,
    ) -> Option<crate::ports::PaddockHolder> {
        None
    }

    fn paddock_load(&self, _: &crate::forwarder::Upstream, _: &str) -> Option<ModelLoad> {
        panic!("a load read that panics")
    }
}

#[tokio::test(start_paused = true)]
async fn a_load_read_that_panics_leaves_each_model_unknown() {
    let (_dir, agents) = agents(&[("coder.md", on_gateway("paddock", "coder"))]);
    let sources = Sources::of(&agents, &gateways());
    let shown = Mutex::new(Availability::new(&sources));
    let leases = Arc::new(LeaseReads::default());
    leases.loaded(Some(ModelLoad {
        slots: 1,
        held: 1,
        waiting: 0,
    }));
    let clock = Arc::new(FakeClock::at(EPOCH));
    let readers = |leases| Readers {
        leases,
        meters: BTreeMap::new(),
        clock: clock.clone(),
    };
    read(&shown, &sources, &readers(leases)).await;
    assert_eq!(shown.lock().unwrap().status()["models"][0]["held"], 1);

    read(&shown, &sources, &readers(Arc::new(Panics))).await;
    let status = shown.lock().unwrap().status();
    assert!(
        status["models"]
            .as_array()
            .unwrap()
            .contains(&model("coder", unknown())),
        "{status}"
    );
}

// The paddock's key unset in the dog's environment, as when shep gives the dog none.
fn keyless() -> Gateways {
    let table: BTreeMap<GatewayName, Gateway> =
        toml::from_str("[paddock]\nurl = \"http://gpu:8700\"\nkey_env = \"PADDOCK_KEY\"\n")
            .unwrap();
    Gateways::reading(table, |_| None)
}

fn naming<'a>(lines: &'a [String], model: &str) -> Vec<&'a String> {
    let model = format!("`{model}`");
    lines.iter().filter(|l| l.contains(&model)).collect()
}

#[tokio::test(start_paused = true)]
async fn a_load_unread_for_an_unset_key_is_logged_once_naming_the_key_s_variable() {
    let (_dir, agents) = agents(&[("coder.md", on_gateway("paddock", "coder"))]);
    let sources = Sources::of(&agents, &keyless());
    let shown = Mutex::new(Availability::new(&sources));
    let leases = Arc::new(LeaseReads::default());
    let readers = Readers {
        leases: leases.clone(),
        meters: BTreeMap::new(),
        clock: Arc::new(FakeClock::at(EPOCH)),
    };

    let logged = read(&shown, &sources, &readers).await;
    let coder = naming(&logged, "coder");
    assert_eq!(coder.len(), 1, "{logged:?}");
    assert!(coder[0].contains("gateway `paddock`"), "{}", coder[0]);
    assert!(coder[0].contains("PADDOCK_KEY"), "{}", coder[0]);
    assert_eq!(leases.count(), 0, "a model with no key is never asked");

    let logged = read(&shown, &sources, &readers).await;
    assert_eq!(
        logged,
        Vec::<String>::new(),
        "the same reason is not logged again"
    );
}

#[tokio::test(start_paused = true)]
async fn a_load_the_gateway_does_not_give_is_logged_again_only_after_a_good_read() {
    let (_dir, agents) = agents(&[("coder.md", on_gateway("paddock", "coder"))]);
    let sources = Sources::of(&agents, &gateways());
    let shown = Mutex::new(Availability::new(&sources));
    let leases = Arc::new(LeaseReads::default());
    let readers = Readers {
        leases: leases.clone(),
        meters: BTreeMap::new(),
        clock: Arc::new(FakeClock::at(EPOCH)),
    };
    let free = ModelLoad {
        slots: 1,
        held: 0,
        waiting: 0,
    };

    let logged = read(&shown, &sources, &readers).await;
    let coder = naming(&logged, "coder");
    assert_eq!(coder.len(), 1, "{logged:?}");
    assert!(coder[0].contains("gateway `paddock`"), "{}", coder[0]);
    assert!(!coder[0].contains("PADDOCK_KEY"), "{}", coder[0]);
    assert_eq!(read(&shown, &sources, &readers).await, Vec::<String>::new());

    leases.loaded(Some(free));
    assert_eq!(read(&shown, &sources, &readers).await, Vec::<String>::new());
    leases.loaded(None);
    let logged = read(&shown, &sources, &readers).await;
    assert_eq!(naming(&logged, "coder").len(), 1, "{logged:?}");
}

// 2026-09-01 22:00 UTC less a day: six hours on is 04:00 UTC.
const EVENING: Timestamp = Timestamp(1_788_213_600);
const CREDIT_BACK: u64 = EVENING.0 + CREDIT_RECHECK_SECS;
// 2026-12-01 00:00 UTC
const DECEMBER: Timestamp = Timestamp(1_796_083_200);

fn spend_limit(resumes: Option<Timestamp>) -> Refusal {
    Refusal::SpendLimit { resumes }
}

#[test]
fn a_refusal_holds_the_api_until_the_later_of_two_times() {
    let mut shown = Availability::default();
    assert_eq!(shown.api_held(EVENING), None);

    let first = shown.refused(spend_limit(Some(DECEMBER)), EVENING);
    assert_eq!(first, DECEMBER);
    assert_eq!(shown.api_held(EVENING), Some(DECEMBER));

    let sooner = shown.refused(Refusal::NoCredit, EVENING);
    assert_eq!(
        sooner, DECEMBER,
        "a refusal that ends sooner keeps the later time"
    );
    assert_eq!(shown.api_held(EVENING), Some(DECEMBER));

    let later = Timestamp(DECEMBER.0 + 3600);
    assert_eq!(shown.refused(spend_limit(Some(later)), EVENING), later);
    assert_eq!(shown.api_held(EVENING), Some(later));

    assert_eq!(shown.api_held(Timestamp(later.0 - 1)), Some(later));
    assert_eq!(shown.api_held(later), None, "the hold ends at its time");
    let again = shown.refused(Refusal::NoCredit, later);
    assert_eq!(
        again,
        Timestamp(later.0 + CREDIT_RECHECK_SECS),
        "a lapsed hold does not outlast a new one"
    );
}

#[test]
fn the_status_says_the_hold_in_one_line_and_leaves_it_out_before_one() {
    let mut shown = Availability::default();
    assert_eq!(shown.status().get("api"), None);

    shown.refused(Refusal::NoCredit, EVENING);
    assert_eq!(
        shown.status()["api"],
        json!({
            "kind": "no_credit",
            "until": CREDIT_BACK,
            "line": "API: out of credit, checking again at 04:00 UTC",
        })
    );

    let mut limited = Availability::default();
    limited.refused(spend_limit(Some(DECEMBER)), EVENING);
    assert_eq!(
        limited.status()["api"],
        json!({
            "kind": "spend_limit",
            "until": DECEMBER,
            "line": "API: spend limit reached, back at 2026-12-01 00:00 UTC",
        })
    );

    let mut unreadable = Availability::default();
    unreadable.refused(spend_limit(None), EVENING);
    assert_eq!(
        unreadable.status()["api"]["line"],
        "API: spend limit reached, checking again at 04:00 UTC"
    );
}

#[tokio::test(start_paused = true)]
async fn a_pass_drops_a_hold_once_its_time_has_come() {
    let sources = Sources::of(&agents(&[]).1, &gateways());
    let shown = Mutex::new(Availability::new(&sources));
    let clock = FakeClock::at(EVENING.0);
    let readers = Readers {
        leases: Arc::new(LeaseReads::default()),
        meters: BTreeMap::new(),
        clock: Arc::new(clock.clone()),
    };
    shown.lock().unwrap().refused(Refusal::NoCredit, EVENING);

    clock.advance(CREDIT_RECHECK_SECS - 1);
    read(&shown, &sources, &readers).await;
    assert!(shown.lock().unwrap().status().get("api").is_some());

    clock.advance(1);
    read(&shown, &sources, &readers).await;
    assert_eq!(shown.lock().unwrap().status().get("api"), None);
}

// A gateway key the dog's environment holds under cargo, so a reread's gateway can be asked.
const KEY: &str = "CARGO_PKG_DESCRIPTION";

fn gateway_section(names: &[&str], key_env: &str) -> KelpieSettings {
    let tables = names.iter().map(|name| {
        format!("[gateways.{name}]\nurl = \"http://{name}:8700\"\nkey_env = \"{key_env}\"\n")
    });
    KelpieSettings::from_section(&tables.collect::<String>()).unwrap()
}

fn live_ports(leases: &Arc<LeaseReads>, codex_meter: fn(PathBuf) -> Arc<dyn Meter>) -> Ports {
    Ports {
        local_leases: leases.clone(),
        meter: Arc::new(FakeMeter::idle()),
        codex_meter,
    }
}

// Yields until `seen` holds, with the clock paused so only the dog's own tasks move.
async fn until(what: &str, seen: impl Fn() -> bool) {
    let waited = async {
        while !seen() {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(CALL_PATIENCE, waited)
        .await
        .unwrap_or_else(|_| panic!("{what}"));
}

fn account(shown: &Mutex<Availability>, name: &str) -> Value {
    let status = shown.lock().unwrap().status();
    let accounts = status["accounts"].as_array().unwrap().clone();
    accounts
        .into_iter()
        .find(|a| a["account"] == name)
        .unwrap_or(Value::Null)
}

// The first pass has read every account, so the next one is the next tick.
async fn first_pass_done(shown: &Mutex<Availability>) {
    until("the first pass never ended", || {
        account(shown, "claude")["read_at"] != Value::Null
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_gateway_added_by_a_reread_is_read_from_the_next_pass() {
    let (_dir, agents) = agents(&[("coder.md", on_gateway("paddock", "coder"))]);
    let leases = Arc::new(LeaseReads::default());
    leases.loaded(Some(ModelLoad {
        slots: 2,
        held: 1,
        waiting: 0,
    }));
    let started = KelpieSettings::from_section("").unwrap();
    let (shown, live) = begin(&agents, &started, live_ports(&leases, |_| idle_meter()));
    first_pass_done(&shown).await;
    assert_eq!(shown.lock().unwrap().status()["models"], json!([]));

    live.apply(&gateway_section(&["paddock"], KEY));
    assert_eq!(
        shown.lock().unwrap().status()["models"],
        json!([model("coder", unknown())]),
        "unknown until read"
    );
    assert_eq!(leases.count(), 0);

    tokio::time::advance(LOAD_TICK).await;
    until("the next pass never read it", || leases.count() == 1).await;
    until("the reading never showed", || {
        shown.lock().unwrap().status()["models"][0]["slots"] == 2
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_gateway_taken_out_leaves_status_and_keeps_the_readings_of_the_rest() {
    let (_dir, agents) = agents(&[
        ("coder.md", on_gateway("paddock", "coder")),
        ("spare-coder.md", on_gateway("spare", "spare-coder")),
    ]);
    let leases = Arc::new(LeaseReads::default());
    leases.loaded(Some(ModelLoad {
        slots: 2,
        held: 1,
        waiting: 0,
    }));
    let both = gateway_section(&["paddock", "spare"], KEY);
    let (shown, live) = begin(&agents, &both, live_ports(&leases, |_| idle_meter()));
    first_pass_done(&shown).await;
    let before = shown.lock().unwrap().status();
    assert_eq!(before["models"].as_array().unwrap().len(), 2, "{before}");
    assert_eq!(before["models"][0]["slots"], 2, "{before}");

    let (old_sources, old_readers) = live.now();
    live.apply(&gateway_section(&["paddock"], KEY));

    let after = shown.lock().unwrap().status();
    assert_eq!(after["models"], json!([before["models"][0].clone()]));
    assert_eq!(after["accounts"], before["accounts"]);

    // A pass that began on the old sources cannot put the model back.
    read(&shown, &old_sources, &old_readers).await;
    let status = shown.lock().unwrap().status();
    assert_eq!(status["models"].as_array().unwrap().len(), 1, "{status}");
}

#[tokio::test(start_paused = true)]
async fn a_new_gateway_key_variable_is_hidden_from_processes_from_then_on() {
    const NEW: &str = "CARGO_PKG_NAME";
    assert!(std::env::var_os(NEW).is_some(), "{NEW} is set under cargo");
    let (_dir, agents) = agents(&[]);
    let leases = Arc::new(LeaseReads::default());
    let started = KelpieSettings::from_section("").unwrap();
    let (_shown, live) = begin(&agents, &started, live_ports(&leases, |_| idle_meter()));
    let hidden = |var: &str| {
        let command = crate::spawn::command("env");
        command.get_envs().any(|(k, v)| k == var && v.is_none())
    };
    assert!(!hidden(NEW), "hidden before a gateway names it");

    live.apply(&gateway_section(&["paddock"], NEW));
    assert!(hidden(NEW));

    live.apply(&KelpieSettings::from_section("").unwrap());
    assert!(hidden(NEW), "a gateway taken out keeps its variable hidden");
}

fn idle_meter() -> Arc<dyn Meter> {
    Arc::new(FakeMeter::idle())
}

// The Codex meter of the login at `/srv/codex-live` reads 40% of the week, any other 0%.
fn codex_meter_by_login(home: PathBuf) -> Arc<dyn Meter> {
    let meter = FakeMeter::idle();
    if home == std::path::Path::new("/srv/codex-live") {
        meter.set(utilization(40, 12));
    }
    Arc::new(meter)
}

#[tokio::test(start_paused = true)]
async fn a_changed_codex_home_builds_the_meter_on_the_new_login() {
    let (_dir, agents) = agents(&[(
        "gpt.md",
        "---\nrole: implementer\nharness: codex\nmodel: gpt-6-sol\neffort: medium\n---\n"
            .to_owned(),
    )]);
    let leases = Arc::new(LeaseReads::default());
    let started = KelpieSettings::from_section("").unwrap();
    let ports = live_ports(&leases, codex_meter_by_login);
    let (shown, live) = begin(&agents, &started, ports);
    until("the codex account never read", || {
        account(&shown, "codex")["read_at"] != Value::Null
    })
    .await;
    assert_eq!(account(&shown, "codex")["week"]["used_pct"], 0);

    live.apply(&KelpieSettings::from_section("codex_home = \"/srv/codex-live\"\n").unwrap());
    tokio::time::advance(LOAD_TICK).await;

    until("the new login was never read", || {
        account(&shown, "codex")["week"]["used_pct"] == 40
    })
    .await;
}
