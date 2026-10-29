use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{Availability, Readers, Sources, read};
use crate::agents::Agents;
use crate::pacer::RECHECK_SECS;
use crate::ports::{Meter, MeterError, ModelLoad};
use crate::settings::{Account, Gateway, GatewayName, Gateways};
use crate::test::{EPOCH, FakeClock, FakeMeter, LeaseReads, utilization, write_in};

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

// Kelpie's own agents and `files`, which add to them.
fn agents(files: &[(&str, String)]) -> (tempfile::TempDir, Agents) {
    let dir = tempfile::tempdir().unwrap();
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
            "models": [model("coder", unknown()), model("iq3_xxs", unknown())],
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
            "models": [model("coder", loaded(EPOCH)), model("iq3_xxs", loaded(EPOCH))],
            "accounts": [idle_claude(EPOCH)],
        })
    );
    assert_eq!((leases.count(), claude.reads()), (2, 1));

    clock.advance(RECHECK_SECS - 1);
    read(&shown, &sources, &readers).await;
    assert_eq!(
        (leases.count(), claude.reads()),
        (4, 1),
        "the meter is fresh"
    );
    let status = shown.lock().unwrap().status();
    assert_eq!(status["models"][0]["read_at"], EPOCH + RECHECK_SECS - 1);

    clock.advance(1);
    claude.set(utilization(40, 12));
    read(&shown, &sources, &readers).await;
    assert_eq!((leases.count(), claude.reads()), (6, 2));
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
