use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::agents::Agents;
use crate::dog::availability::{self, Availability, LOAD_TICK, Ports};
use crate::dog::board::Board;
use crate::dog::{lock_desk, open};
use crate::lease::counted::{Taker, Ticket};
use crate::lease::saved::BookFile;
use crate::review_bot::Reviewers;
use crate::shepherd::Answer;
use crate::test::{
    CALL_PATIENCE, EPOCH, FakeAlerts, FakeClock, FakeMeter, FakeShepherd, LeaseReads, RecordedSink,
    write_in,
};
use shep_client::RequestError;
use shep_client::shep_core::protocol::{HelloAck, Request, RpcError, RpcErrorCode};

const BASE: &str = "[leases]\ncargo-test = 3\n[agent_board]\nprefer = [\"opus\"]\n";

// A dog that has started on `section`, with its reread, and the stand-ins it reads and posts through.
struct Dog {
    shepherd: FakeShepherd,
    connection: Box<dyn Connection>,
    desk: Arc<Mutex<Kept>>,
    board: Arc<Mutex<Board>>,
    waiters: Waiters,
    alerts: FakeAlerts,
    standing: Arc<Mutex<SettingsState>>,
    shown: Arc<Mutex<Availability>>,
    clock: FakeClock,
    reread: Reread,
    dir: tempfile::TempDir,
}

async fn started_on(section: &str) -> Dog {
    let dir = tempfile::tempdir().unwrap();
    let shepherd = FakeShepherd::new();
    shepherd.holds_section(section);
    let connection = shepherd.connect().await;
    let started = section::settings(&*connection).await.unwrap();
    let clock = FakeClock::at(EPOCH);
    let mut kept = open(
        BookFile::new(dir.path().join("book.json")),
        Box::new(clock.clone()),
        Reviewers::default(),
        RecordedSink::default().recorder(),
    );
    kept.desk
        .set_test_capacity(started.settings.leases.cargo_test_capacity());
    let desk = Arc::new(Mutex::new(kept));
    let (waiters, alerts) = (Waiters::default(), FakeAlerts::on(clock.clone()));
    let standing = Arc::default();
    // One agent on the paddock gateway `paddock`, which the section names or not.
    write_in(
        dir.path(),
        "coder.md",
        "---\nrole: implementer\nharness: pi\nmodel: coder\neffort: low\n\
         gateway: paddock\ncontext: 65536\n---\n",
    );
    let ports = Ports {
        local_leases: Arc::new(LeaseReads::default()),
        meter: Arc::new(FakeMeter::idle()),
        codex_meter: |_| Arc::new(FakeMeter::idle()),
    };
    let agents = Agents::load(dir.path()).unwrap();
    let board = Board::new(&agents).preferring(started.settings.agent_board.prefer.clone());
    let board = Arc::new(Mutex::new(board));
    let (shown, live) = availability::begin(&agents, &started.settings, ports);
    let reread = Reread::new(
        &started,
        Handles {
            desk: Arc::clone(&desk),
            board: Arc::clone(&board),
            waiters: Arc::clone(&waiters),
            alerts: Arc::new(alerts.clone()),
            standing: Arc::clone(&standing),
            live,
            agents: Some(dir.path().to_owned()),
            loaded: agents,
        },
    );
    Dog {
        shepherd,
        connection,
        desk,
        board,
        waiters,
        alerts,
        standing,
        shown,
        clock,
        reread,
        dir,
    }
}

const NTFY: &str = "[webhook]\nkind = \"ntfy\"\nurl = \"http://ntfy.tail1234.ts.net/dog\"\n";

fn capacity_of(desk: &Mutex<Kept>) -> u32 {
    lock_desk(desk).desk.tests.status().capacity.get()
}

// A shepherd that answers every ask with the errors it was given, the last one repeating.
#[derive(Debug)]
struct Silent {
    daemon: HelloAck,
    errors: Mutex<VecDeque<fn() -> RequestError>>,
}

impl crate::shepherd::Connection for Silent {
    fn send(&self, _: Request, _: Option<Duration>) -> Answer<'_> {
        let mut errors = self.errors.lock().unwrap();
        let error = if errors.len() > 1 {
            errors.pop_front()
        } else {
            errors.front().copied()
        };
        Box::pin(async move { Err(error.unwrap()()) })
    }

    fn daemon(&self) -> HelloAck {
        self.daemon.clone()
    }
}

impl Dog {
    fn edit(&self, section: &str) {
        self.shepherd.holds_section(section);
    }

    // One pass of the reread, and the lines it logged.
    async fn pass(&mut self) -> Vec<String> {
        tokio::time::timeout(CALL_PATIENCE, self.reread.pass(&*self.connection))
            .await
            .expect("the pass never finished")
    }

    fn capacity(&self) -> u32 {
        capacity_of(&self.desk)
    }

    fn prefer(&self) -> Vec<String> {
        let prefer = crate::dog::board::lock(&self.board).prefer();
        prefer.iter().map(|a| a.as_str().to_owned()).collect()
    }

    // The models the dog reads a load for, as `gateway/model`.
    fn models(&self) -> Vec<String> {
        let status = self.shown.lock().unwrap().status();
        let models = status["models"].as_array().unwrap().clone();
        let name = |m: &serde_json::Value| format!("{}/{}", m["gateway"], m["model"]);
        models.iter().map(name).collect()
    }

    // The implementers the board ranks, by name.
    fn ranked(&self) -> Vec<String> {
        let view = crate::dog::board::lock(&self.board).view();
        let status = view.status(&Availability::default(), Timestamp(EPOCH));
        status.as_object().unwrap().keys().cloned().collect()
    }

    fn state(&self) -> SettingsState {
        self.standing.lock().unwrap().clone()
    }
}

#[tokio::test(start_paused = true)]
async fn a_changed_section_applies_the_capacity_and_the_prefer_list_at_the_next_pass() {
    let mut dog = started_on(BASE).await;
    assert_eq!((dog.capacity(), dog.prefer()), (3, vec!["opus".to_owned()]));

    dog.edit("[leases]\ncargo-test = 5\n[agent_board]\nprefer = [\"sonnet\", \"opus\"]\n");
    let lines = dog.pass().await;

    assert_eq!(dog.capacity(), 5);
    assert_eq!(dog.prefer(), ["sonnet", "opus"]);
    assert_eq!(
        lines,
        [
            "applied leases.cargo-test: 5 at a time",
            "applied agent_board.prefer: sonnet, opus"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_section_read_again_unchanged_applies_nothing() {
    let mut dog = started_on(BASE).await;

    assert_eq!(dog.pass().await, Vec::<String>::new());
    assert_eq!(dog.pass().await, Vec::<String>::new());

    assert_eq!((dog.capacity(), dog.prefer()), (3, vec!["opus".to_owned()]));
    assert_eq!(dog.state(), SettingsState::default());
}

#[tokio::test(start_paused = true)]
async fn a_raised_capacity_wakes_the_waiting_command_and_a_lowered_one_takes_no_lease_back() {
    let mut dog = started_on("[leases]\ncargo-test = 1\n").await;
    let taker = |ticket, pid| Taker {
        ticket: Ticket(ticket),
        pid,
        what: format!("suite {ticket}"),
    };
    let (wake, mut woken) = tokio::sync::oneshot::channel();
    {
        let mut kept = lock_desk(&dog.desk);
        kept.desk.take_test(taker(1, 4242));
        kept.desk.take_test(taker(2, 4243));
    }
    dog.waiters.lock().unwrap().insert(Ticket(2), wake);

    dog.edit("[leases]\ncargo-test = 2\n");
    dog.pass().await;

    tokio::time::timeout(CALL_PATIENCE, &mut woken)
        .await
        .expect("the waiting command was never woken")
        .unwrap();
    let held = |dog: &Dog| lock_desk(&dog.desk).desk.tests.status().holders.len();
    assert_eq!((dog.capacity(), held(&dog)), (2, 2));

    dog.edit("[leases]\ncargo-test = 1\n");
    dog.pass().await;
    assert_eq!(
        (dog.capacity(), held(&dog)),
        (1, 2),
        "both holders keep their lease"
    );
    lock_desk(&dog.desk).desk.take_test(taker(3, 4244));
    assert_eq!(
        lock_desk(&dog.desk).desk.tests.status().queue.len(),
        1,
        "no new grant"
    );
}

#[tokio::test(start_paused = true)]
async fn a_section_that_does_not_parse_keeps_the_old_settings_and_alerts_once_per_reason() {
    let mut dog = started_on(&format!("{NTFY}{BASE}")).await;

    dog.edit("[leases]\ncargo-test = 0\n");
    let first = dog.pass().await;
    dog.pass().await;
    dog.edit("[leases]\ncargo-test = 7\nnonsense = 1\n");
    dog.pass().await;
    dog.pass().await;

    assert_eq!((dog.capacity(), dog.prefer()), (3, vec!["opus".to_owned()]));
    assert!(first[0].contains("old settings stand"), "{first:?}");
    let posts = dog.alerts.posts();
    assert_eq!(posts.len(), 2, "one alert per reason");
    for (_, alert) in &posts {
        assert_eq!(alert.title, "kelpie: dog settings change refused");
        assert!(
            alert
                .text
                .ends_with("The dog keeps its old settings until its [kelpie] section is fixed."),
            "{}",
            alert.text
        );
    }
    assert_ne!(posts[0].1.text, posts[1].1.text);
}

#[tokio::test(start_paused = true)]
async fn a_refusal_shows_in_status_with_its_first_time_until_a_good_read_clears_it() {
    let mut dog = started_on(BASE).await;

    dog.edit("not a section");
    dog.pass().await;
    let first = dog.state().refused.unwrap();
    dog.clock.advance(90);
    dog.pass().await;
    assert_eq!(
        dog.state().refused.unwrap(),
        first,
        "the same reason keeps its first time"
    );
    assert_eq!(first.at, Timestamp(EPOCH));

    dog.edit("[leases]\ncargo-test = 4\n");
    dog.pass().await;
    assert_eq!(dog.state().refused, None);
    assert_eq!(dog.capacity(), 4);
}

#[tokio::test(start_paused = true)]
async fn an_alert_that_failed_is_posted_again_at_the_next_pass() {
    let mut dog = started_on(&format!("{NTFY}{BASE}")).await;
    dog.alerts.set_down(true);
    dog.edit("not a section");

    let down = dog.pass().await;
    assert!(down.iter().any(|l| l.contains("cannot post")), "{down:?}");
    dog.alerts.set_down(false);
    dog.pass().await;
    dog.pass().await;

    assert_eq!(dog.alerts.posts().len(), 2, "tried again once, then told");
}

#[tokio::test(start_paused = true)]
async fn a_dog_with_no_webhook_posts_nothing_for_a_refusal() {
    let mut dog = started_on(BASE).await;
    dog.edit("not a section");

    dog.pass().await;

    assert_eq!(dog.alerts.posts().len(), 0);
    assert!(dog.state().refused.is_some());
}

#[tokio::test(start_paused = true)]
async fn a_webhook_that_would_stop_the_dog_is_refused_before_a_restart() {
    let mut dog = started_on(&format!("{NTFY}{BASE}")).await;

    dog.edit(&format!("{NTFY}trusted = true\n[leases]\ncargo-test = 9\n"));
    dog.pass().await;

    assert_eq!(dog.capacity(), 3, "nothing of the text applies");
    let reason = dog.state().refused.unwrap().reason;
    assert!(reason.contains("`webhook.trusted`"), "{reason}");
    assert_eq!(dog.alerts.posts().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_refused_section_applies_once_its_ca_cert_file_appears() {
    let mut dog = started_on(&format!("{NTFY}{BASE}")).await;
    let ca = dog.dir.path().join("ca.pem");
    let section = format!(
        "{NTFY}ca_cert = {:?}\n[leases]\ncargo-test = 6\n",
        ca.display().to_string()
    );
    dog.edit(&section);

    dog.pass().await;
    assert_eq!(dog.capacity(), 3);
    assert!(
        dog.state()
            .refused
            .unwrap()
            .reason
            .contains("`webhook.ca_cert`")
    );

    std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\n").unwrap();
    let lines = dog.pass().await;

    assert_eq!(dog.capacity(), 6);
    assert_eq!(dog.state().refused, None);
    assert!(
        lines.contains(&"waiting for restart: webhook.ca_cert".to_owned()),
        "{lines:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_changed_webhook_waits_for_a_restart_naming_each_key_and_no_value() {
    let mut dog = started_on(&format!("{NTFY}{BASE}")).await;

    dog.edit(&format!(
        "[webhook]\nkind = \"ntfy\"\nurl = \"https://other-box.example/secret-topic\"\n\
         token = \"tk_s3cr3t\"\n{BASE}",
    ));
    let lines = dog.pass().await;
    let again = dog.pass().await;

    assert_eq!(
        lines,
        [
            "waiting for restart: webhook.url",
            "waiting for restart: webhook.token"
        ]
    );
    assert_eq!(again, Vec::<String>::new(), "said once");
    assert_eq!(dog.state().waiting, ["webhook.url", "webhook.token"]);
    assert!(
        lines
            .iter()
            .all(|l| !l.contains("secret") && !l.contains("s3cr3t"))
    );

    dog.edit(BASE);
    let gone = dog.pass().await;
    assert_eq!(gone, ["waiting for restart: webhook"]);
    assert_eq!(dog.state().waiting, ["webhook"]);
}

#[tokio::test(start_paused = true)]
async fn a_webhook_put_back_stops_waiting() {
    let mut dog = started_on(&format!("{NTFY}{BASE}")).await;
    let other = NTFY.replace("/dog", "/other");

    dog.edit(&format!("{other}{BASE}"));
    dog.pass().await;
    assert_eq!(dog.state().waiting, ["webhook.url"]);

    dog.edit(&format!("{NTFY}{BASE}"));
    dog.pass().await;
    assert!(dog.state().waiting.is_empty());
}

// A key variable the dog's environment holds under cargo.
const PADDOCK: &str =
    "[gateways.paddock]\nurl = \"http://gpu:8700\"\nkey_env = \"CARGO_PKG_DESCRIPTION\"\n";

#[tokio::test(start_paused = true)]
async fn a_changed_gateway_or_codex_home_applies_and_waits_for_nothing() {
    let mut dog = started_on(BASE).await;
    assert_eq!(dog.models(), Vec::<String>::new());

    dog.edit(&format!("codex_home = \"/srv/codex\"\n{PADDOCK}{BASE}"));
    let lines = dog.pass().await;

    assert_eq!(
        lines,
        [
            "applied gateways: paddock",
            "applied codex_home: /srv/codex"
        ]
    );
    assert_eq!(dog.state(), SettingsState::default(), "nothing waits");
    assert_eq!(dog.models(), [r#""paddock"/"coder""#]);
    assert_eq!(dog.pass().await, Vec::<String>::new(), "applied once");

    dog.edit(BASE);
    let lines = dog.pass().await;
    assert_eq!(
        lines,
        ["applied gateways: none", "applied codex_home: kelpie's own"]
    );
    assert_eq!(dog.models(), Vec::<String>::new());
    assert_eq!(dog.state(), SettingsState::default());
}

#[tokio::test(start_paused = true)]
async fn a_codex_home_the_dog_refuses_refuses_the_whole_section() {
    let mut dog = started_on(&format!("{NTFY}{BASE}")).await;

    dog.edit(&format!(
        "codex_home = \"logins/codex\"\n{PADDOCK}{NTFY}[leases]\ncargo-test = 9\n"
    ));
    let first = dog.pass().await;
    dog.pass().await;

    assert_eq!(dog.capacity(), 3, "nothing of the text applies");
    assert_eq!(dog.models(), Vec::<String>::new());
    assert!(first[0].contains("old settings stand"), "{first:?}");
    let reason = dog.state().refused.unwrap().reason;
    assert!(reason.contains("`codex_home`"), "{reason}");
    assert_eq!(dog.alerts.posts().len(), 1, "alerted once");

    dog.edit(&format!(
        "codex_home = \"/srv/codex\"\n{NTFY}[leases]\ncargo-test = 9\n"
    ));
    dog.pass().await;
    assert_eq!(dog.capacity(), 9);
    assert_eq!(dog.state().refused, None);
}

#[tokio::test(start_paused = true)]
async fn an_edit_elsewhere_applies_while_the_dog_runs_on_a_codex_home_it_started_with() {
    let mut dog = started_on(&format!("codex_home = \"logins/codex\"\n{NTFY}{BASE}")).await;

    dog.edit(&format!(
        "codex_home = \"logins/codex\"\n{NTFY}[leases]\ncargo-test = 9\n"
    ));
    let lines = dog.pass().await;

    assert_eq!(dog.capacity(), 9);
    assert_eq!(dog.state().refused, None);
    assert_eq!(dog.alerts.posts().len(), 0);
    assert!(lines.iter().all(|l| !l.contains("refused")), "{lines:?}");
}

#[tokio::test(start_paused = true)]
async fn a_section_that_cannot_be_asked_for_keeps_the_settings_and_logs_once() {
    let mut dog = started_on(BASE).await;
    dog.edit("[leases]\ncargo-test = 5\n");
    let refuse_not_found = || {
        RequestError::Rpc(RpcError {
            code: RpcErrorCode::NotFound,
            message: "no kelpie entry".into(),
            daemon_version: None,
        })
    };
    let silent = Silent {
        daemon: dog.connection.daemon(),
        errors: Mutex::new(VecDeque::from([
            (|| RequestError::Closed) as fn() -> RequestError,
            || RequestError::Closed,
            refuse_not_found,
        ])),
    };

    let first = dog.reread.pass(&silent).await;
    let second = dog.reread.pass(&silent).await;
    let third = dog.reread.pass(&silent).await;

    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(second, Vec::<String>::new(), "the same reason is said once");
    assert_eq!(third.len(), 1, "a new reason is said");
    assert_eq!(dog.capacity(), 3);
    assert_eq!(dog.state(), SettingsState::default(), "no status entry");
    assert_eq!(dog.alerts.posts().len(), 0);

    dog.edit("");
    dog.pass().await;
    assert_eq!(dog.capacity(), 3, "an empty text is the default capacity");
    assert!(dog.prefer().is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_pass_runs_every_load_tick() {
    let Dog {
        shepherd,
        connection,
        desk,
        reread,
        ..
    } = started_on(BASE).await;
    begin(reread, Arc::from(connection));
    let settle = || async {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    };
    let until = |what: &'static str, want: u32| {
        let desk = Arc::clone(&desk);
        async move {
            let seen = async {
                while capacity_of(&desk) != want {
                    tokio::task::yield_now().await;
                }
            };
            tokio::time::timeout(CALL_PATIENCE, seen).await.expect(what);
        }
    };

    settle().await;
    shepherd.holds_section("[leases]\ncargo-test = 4\n");
    tokio::time::advance(LOAD_TICK - Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(capacity_of(&desk), 3, "not before the tick");
    tokio::time::advance(Duration::from_secs(1)).await;
    until("the first tick never applied the change", 4).await;

    shepherd.holds_section("[leases]\ncargo-test = 2\n");
    tokio::time::advance(LOAD_TICK).await;
    until("the second tick never applied the change", 2).await;
}

const CODER: &str = "---\nrole: implementer\nharness: pi\nmodel: coder\neffort: low\n\
                     gateway: paddock\ncontext: 65536\n---\n";

#[tokio::test(start_paused = true)]
async fn a_changed_agent_file_applies_at_the_next_pass() {
    let mut dog = started_on(&format!("{PADDOCK}{BASE}")).await;
    assert_eq!(dog.models(), [r#""paddock"/"coder""#]);
    assert!(dog.ranked().contains(&"coder".to_owned()));
    assert_eq!(dog.pass().await, Vec::<String>::new(), "no edit, no line");

    write_in(
        dog.dir.path(),
        "coder.md",
        &CODER.replace("model: coder", "model: coder-2"),
    );
    write_in(dog.dir.path(), "extra.md", &CODER.replace("coder", "extra"));
    let lines = dog.pass().await;

    assert_eq!(lines, ["applied agent files: coder, extra"]);
    assert_eq!(
        dog.models(),
        [r#""paddock"/"coder-2""#, r#""paddock"/"extra""#]
    );
    assert!(dog.ranked().contains(&"extra".to_owned()));
    assert_eq!(dog.pass().await, Vec::<String>::new(), "applied once");
    assert_eq!(dog.state(), SettingsState::default(), "nothing waits");
}

#[tokio::test(start_paused = true)]
async fn an_agent_folder_that_fails_to_load_keeps_the_old_agents() {
    let mut dog = started_on(&format!("{PADDOCK}{BASE}")).await;
    let ranked = dog.ranked();

    // Not UTF-8, so the file cannot be read.
    std::fs::write(dog.dir.path().join("broken.md"), [0xff, 0xfe]).unwrap();
    write_in(dog.dir.path(), "extra.md", &CODER.replace("coder", "extra"));
    let lines = dog.pass().await;

    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("the agent files are not read and the old ones stand:"),
        "{lines:?}"
    );
    assert!(lines[0].contains("broken.md"), "{lines:?}");
    assert_eq!(dog.ranked(), ranked);
    assert_eq!(dog.models(), [r#""paddock"/"coder""#]);
    assert_eq!(dog.pass().await, Vec::<String>::new(), "said once");

    std::fs::remove_file(dog.dir.path().join("broken.md")).unwrap();
    assert_eq!(dog.pass().await, ["applied agent files: extra"]);
    assert!(dog.ranked().contains(&"extra".to_owned()));
}
