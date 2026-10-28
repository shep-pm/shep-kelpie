//! The kelpie dog: the lease book, run by the adopted `kelpie`
//!
//! Shep starts the adopted kelpie with no arguments, and that start is the
//! dog. It asks for the shepherd channel in its `--version` answer, so
//! `shep trigger kelpie <status|take|return>` reaches it. It listens on the
//! shepherd's bus for runners' lease notes and their process events,
//! keeps the book on its [`desk`], and grants with a `grant` trigger on the
//! runner. The book is saved to `<kelpie home>/dog/book.json` after every
//! change and loaded on start, with a review window for each review bot as
//! its agent file in kelpie's home defines it. It keeps each agent's [`availability`].

pub mod availability;
mod bus;
pub mod desk;
mod door;
mod e2e;
mod forge;
pub mod left_over;
mod link;
pub mod triggers;
mod watch;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use shep_client::shep_core::protocol::request::{ActionOutcome, Response};
use shep_client::shep_core::protocol::{DogSectionToml, Request, RpcErrorCode, SelectorSpec};
use shep_client::shep_core::status::ProcStatus;
use shep_client::{EventStream, ReconnectingClient, RequestError};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::agents::{self, Agents};
use crate::home;
use crate::lease::saved::{BookFile, SavedBook};
use crate::lease::wire::{GRANT, grant_params};
use crate::metrics::{Recorder, ShepherdSink};
use crate::ports::{Clock, LocalPace, SystemClock};
use crate::review_bot::{Bot, Reviewers};
use crate::settings::AgentName;
use crate::shep_home;
use crate::skills;
use crate::webhook::KelpieSettings;
use bus::{Bus, Sent};
use desk::{Delivery, Desk};
use triggers::ACTIONS;

/// The dog's name, which kelpie is adopted under and `shep kelpie lease` triggers
pub const NAME: &str = crate::shepherd::DOG;

/// How long queued replies get to reach the shepherd before the dog exits
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the dog looks for a review window that has opened. CodeRabbit
/// quotes waits to the minute, so a few seconds late costs nothing.
const WINDOW_TICK: Duration = Duration::from_secs(10);

/// Runs the dog until the shepherd stops it, as the adopted kelpie's start
pub fn run(ports: availability::Ports) -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let served = match runtime {
        Ok(runtime) => {
            let served = runtime.block_on(serve(ports));
            // A plan meter's read can take half a minute, and the dog does not wait for it.
            runtime.shutdown_background();
            served
        }
        Err(e) => Err(format!("cannot start the async runtime: {e}")),
    };
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("kelpie dog: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The dog's book file, under kelpie's home
pub const BOOK: &str = "dog/book.json";

/// The dog's book file: `<kelpie home>/dog/book.json`
///
/// # Errors
///
/// A message when kelpie's home cannot be worked out.
pub fn book_path() -> Result<PathBuf, String> {
    Ok(home::kelpie_home()?.join(BOOK))
}

// The desk and the file it is saved to, written whenever the book changes.
struct Kept {
    desk: Desk,
    file: BookFile,
    last: SavedBook,
    // The login's forge hold and pace, in memory only: a restart starts it empty.
    pace: Arc<LocalPace>,
}

impl Kept {
    // Grants whatever an opened review window now allows, and says how the
    // forge hold stands, which ends with no one to tell the dog.
    fn tick(&mut self) -> Vec<Delivery> {
        let grants = self.change(Desk::tick);
        let now = self.desk.book.now();
        forge::publish(&self.pace, &self.desk.metrics, now);
        self.desk.metrics.refresh(now);
        grants
    }

    // A save that fails leaves the previous book standing, and the next
    // change tries again.
    fn change<T>(&mut self, act: impl FnOnce(&mut Desk) -> T) -> T {
        let out = act(&mut self.desk);
        let saved = self.desk.saved();
        if saved != self.last {
            match self.file.save(&saved) {
                Ok(()) => self.last = saved,
                Err(e) => println!("{e}"),
            }
        }
        out
    }
}

// Another build's file, or none, starts an empty book. One that claims
// this build's format and cannot be read may have held a summon, so its
// empty book starts with every review window closed for its span, as if
// a summon had just been accepted. The first change overwrites the file.
fn open(
    file: BookFile,
    clock: Box<dyn Clock>,
    reviewers: Reviewers,
    metrics: Arc<Recorder>,
) -> Kept {
    let empty = || SavedBook::new(Vec::new(), Vec::new(), Vec::new());
    let (last, unread) = match file.load() {
        Ok(Some(saved)) => (saved, false),
        Ok(None) => (empty(), false),
        Err(e) => {
            println!("{e}: starting with an empty book and every review window closed");
            (empty(), true)
        }
    };
    let now = clock.now();
    let bots: Vec<_> = reviewers.defined().map(|(bot, _)| bot).collect();
    let mut desk = Desk::restore(clock, last.clone(), reviewers, metrics);
    if unread {
        for bot in bots {
            desk.book.summoned(&bot.lease(), now);
        }
    }
    Kept {
        desk,
        file,
        last,
        pace: Arc::new(LocalPace::new()),
    }
}

// The agent files in kelpie's home, or kelpie's own when they cannot be read.
fn agents() -> Agents {
    match home::kelpie_home() {
        Ok(home) => agents_in(&home.join(agents::FOLDER)),
        Err(e) => {
            println!("{e}: booking each review bot's window as kelpie's own file has it");
            Agents::embedded()
        }
    }
}

// Kelpie's own agents, whole and read-only in `agents/.defaults`, to read and
// diff. Nothing loads them, so a failure is said and the dog runs on.
fn refresh_baselines() {
    match home::kelpie_home() {
        Ok(home) => refresh_baselines_in(&home.join(agents::FOLDER)),
        Err(e) => println!("{e}: not writing kelpie's own agents under `.defaults`"),
    }
}

fn refresh_baselines_in(folder: &std::path::Path) {
    if let Err(e) = agents::write_baselines(folder) {
        println!("{e}: kelpie's own agents under `.defaults` are not refreshed");
    }
}

// A stub that inherits its baseline's binding gets it written in, as `shep
// kelpie agents migrate` does. The upgrade that first brings this step is run
// by a build without it, so the dog does it too. A failure is said and the dog
// runs on.
fn migrate_agents() {
    match home::kelpie_home() {
        Ok(home) => {
            for line in migrate_agents_in(&home.join(agents::FOLDER)) {
                println!("{line}");
            }
        }
        Err(e) => println!("{e}: not migrating kelpie's agent files"),
    }
}

fn migrate_agents_in(folder: &std::path::Path) -> Vec<String> {
    match agents::legacy::migrate(folder) {
        Ok(report) => report
            .lines()
            .into_iter()
            .map(|line| format!("agent files: {line}"))
            .collect(),
        Err(e) => vec![format!("{e}: kelpie's agent files are not migrated")],
    }
}

// The issue skill in each installed harness's own skills folder, written again
// as each start does so an upgrade's restart brings the new one.
fn link_issue_skill() {
    let Some(user_home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return println!("HOME is not set: not linking kelpie's issue skill");
    };
    match home::kelpie_home() {
        Ok(home) => {
            for line in skills::issue::user::install(&home, &user_home) {
                println!("{line}");
            }
        }
        Err(e) => println!("{e}: not linking kelpie's issue skill"),
    }
}

fn agents_in(folder: &std::path::Path) -> Agents {
    Agents::load(folder).unwrap_or_else(|e| {
        println!("{e}: booking each review bot's window as kelpie's own file has it");
        Agents::embedded()
    })
}

// Each review bot's window, from its agent file. Files that cannot be read,
// or a bot's that cannot be used, leave kelpie's own windows, and a runner
// that names them refuses them anyway.
fn windows_of(agents: &Agents) -> Reviewers {
    let own = Reviewers::from_agents(&Agents::embedded());
    let broken = |bot: &Bot| agents.broken(&AgentName::kelpies(bot.as_str()));
    (Bot::ALL.iter()).fold(Reviewers::from_agents(agents), |windows, bot| {
        match (broken(bot), own.window(*bot)) {
            (Some(e), Some(window)) => {
                println!("{e}: booking {bot}'s window as kelpie's own file has it");
                windows.with(*bot, window)
            }
            _ => windows,
        }
    })
}

// Kelpie's section, for its leases. One that cannot be read holds them at
// the default capacities. A section the shepherd refuses stops the dog.
// So does a webhook `ca_cert` that cannot be read, and `trusted` with no
// `token`, since replies would then be taken on a topic anyone can post to.
async fn settings(client: &ReconnectingClient) -> Result<KelpieSettings, String> {
    section_settings(client.dog_config(crate::shepherd::DOG).await)
}

fn section_settings(
    answer: Result<DogSectionToml, RequestError>,
) -> Result<KelpieSettings, String> {
    match answer {
        Ok(toml) => read_section(toml.as_str()),
        Err(RequestError::Rpc(refusal))
            if matches!(
                refusal.code,
                RpcErrorCode::InvalidConfig | RpcErrorCode::Unsupported
            ) =>
        {
            Err(format!(
                "the shepherd refused the [kelpie] section: {}",
                refusal.message
            ))
        }
        Err(e) => {
            println!("cannot read the [kelpie] section: {e}");
            Ok(KelpieSettings::default())
        }
    }
}

fn read_section(text: &str) -> Result<KelpieSettings, String> {
    if text.trim().is_empty() {
        return Ok(KelpieSettings::default());
    }
    // The text is checked, not the settings, so a mistake elsewhere in the
    // section that salvage works around cannot drop the certificate.
    KelpieSettings::check_ca_cert(text).map_err(|e| e.to_string())?;
    KelpieSettings::check_trusted(text).map_err(|e| e.to_string())?;
    Ok(KelpieSettings::from_section(text).unwrap_or_else(|e| {
        println!("{e}");
        salvage(text)
    }))
}

// A section that does not parse whole: the `[leases]` table and the rest
// are read apart, so a mistake in one leaves the other standing.
fn salvage(text: &str) -> KelpieSettings {
    let Ok(mut table) = text.parse::<toml::Table>() else {
        println!("cargo-test is held by 3 at a time");
        return KelpieSettings::default();
    };
    let leases = table.remove("leases");
    let rest = toml::to_string(&table).unwrap_or_default();
    let mut settings = KelpieSettings::from_section(&rest).unwrap_or_default();
    match leases.map(toml::Value::try_into).transpose() {
        Ok(leases) => settings.leases = leases.unwrap_or_default(),
        Err(e) => println!("[leases] is not right ({e}): cargo-test is held by 3 at a time"),
    }
    settings
}

async fn serve(ports: availability::Ports) -> Result<(), String> {
    let shepherd = shep_channel::serve();
    if !shepherd.is_active() {
        return Err(crate::shepherd::no_channel());
    }
    let shep_home = shep_home::required(shep_home::DOG_FIX)?;
    let socket = home::shepherd_address(&shep_home);
    let identity = link::identity();
    let listener = link::connect(&socket, &identity, "for events").await?;
    let client = Arc::new(link::connect(&socket, &identity, "for requests").await?);
    let mut events = link::subscribe(&listener, link::BUDGET).await?;

    let file = BookFile::new(book_path()?);
    if let (Some(folder), Some(old)) = (file.path().parent(), home::old_dog_home()) {
        home::dog_may_start(&old, folder)?;
    }
    if let Some(removed) = left_over::remove(&*client, file.path()).await? {
        println!("{removed}");
    }
    if let Some(folder) = file.path().parent() {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
    }
    println!("the book is {}", file.path().display());
    let settings = settings(&client).await?;
    let metrics = Arc::new(Recorder::new(Arc::new(ShepherdSink::new(shepherd.clone()))));
    refresh_baselines();
    migrate_agents();
    link_issue_skill();
    let agents = agents();
    let mut kept = open(file, Box::new(SystemClock), windows_of(&agents), metrics);
    let capacity = settings.leases.cargo_test_capacity();
    kept.desk.set_test_capacity(capacity);
    let desk = Arc::new(Mutex::new(kept));
    let shown = availability::begin(&agents, &settings, ports);
    // The other leases do not need the door, so the dog runs on without it.
    // It is bound before any handler can start a process that inherits it.
    // The runner hands its workers the same door, or the default with this message.
    let (socket, refused) = crate::lease::door::checked_socket(&shep_home);
    if let Some(why) = refused {
        println!("{why}");
    }
    let opened = door::open(&socket).map(|listener| (socket, listener));
    match opened {
        Ok((socket, listener)) => {
            println!("the door is {}, {capacity} at a time", socket.display());
            let grace = door::REJOIN_GRACE;
            // Never `KELPIE_HOME`: shep starts the adopted dog without it.
            let home = home::under(&shep_home);
            let remote = e2e::Remote::default();
            tokio::spawn(door::serve(
                listener,
                Arc::clone(&desk),
                grace,
                home,
                remote,
            ));
        }
        Err(e) => {
            println!("{e}: cargo-test cannot be taken until the dog restarts");
            lock_desk(&desk).desk.tests.close();
        }
    }
    let (answered, mut to_deliver) = mpsc::unbounded_channel::<Vec<Delivery>>();
    for action in ACTIONS {
        let (desk, answered, shown) = (Arc::clone(&desk), answered.clone(), Arc::clone(&shown));
        shepherd.on_action(action, move |params, name| {
            let shown = availability::lock(&shown).clone();
            let (body, grants) = lock_desk(&desk).change(|d| d.answer_with(name, params, &shown));
            let _ = answered.send(grants);
            body
        });
    }
    let (stop, mut stopped) = mpsc::unbounded_channel();
    shepherd.on_shutdown(move || {
        let _ = stop.send(());
    });

    // Runners in the saved book that have since died or restarted are
    // reclaimed before the dog takes anything new.
    let mut bus = Bus::new(Instant::now());
    let restored = bus.read(flock(&client).await?, Instant::now(), &desk);
    shepherd.ready().map_err(|e| e.to_string())?;
    println!("up");
    deliver(&client, restored);
    let (flocked, mut flocks) = mpsc::unbounded_channel();
    let mut ticks = tokio::time::interval(WINDOW_TICK);
    let ended = loop {
        let grants = tokio::select! {
            event = events.next() => match event {
                Some(event) => bus.hear(event, &desk),
                None => match rejoin(&listener, &client, &desk, &mut bus).await {
                    Ok((stream, grants)) => {
                        events = stream;
                        grants
                    }
                    Err(e) => break Err(e),
                },
            },
            Some((sent, read)) = flocks.recv() => match read {
                Ok(listed) => bus.read_back(sent, listed, Instant::now(), &desk),
                Err(e) => break Err(e),
            },
            () = until(bus.read_at()) => {
                read_apart(&client, bus.reading(), flocked.clone());
                Vec::new()
            },
            Some(grants) = to_deliver.recv() => grants,
            _ = ticks.tick() => lock_desk(&desk).tick(),
            _ = stopped.recv() => break Ok(()),
        };
        deliver(&client, grants);
    };
    // Replies already queued reach the shepherd however the dog ends.
    let flushed = shepherd.flush(FLUSH_TIMEOUT).map_err(|e| e.to_string());
    ended.and(flushed)
}

// The shepherd channel's handlers run on its own threads, and nothing
// holding the desk can leave it half changed, so a poisoned lock is fine.
fn lock_desk(desk: &Mutex<Kept>) -> std::sync::MutexGuard<'_, Kept> {
    desk.lock().unwrap_or_else(PoisonError::into_inner)
}

// The shepherd restarted: the bus stream ended with its connection, which
// the reconnecting clients have since re-made under the dog's name. The
// dog subscribes again and reads the flock, since what happened while it
// was cut off is gone.
async fn rejoin(
    listener: &ReconnectingClient,
    client: &ReconnectingClient,
    desk: &Mutex<Kept>,
    bus: &mut Bus,
) -> Result<(EventStream, Vec<Delivery>), String> {
    println!("the shepherd's bus closed: waiting for it to come back");
    let events = link::subscribe(listener, link::BUDGET).await?;
    let deadline = tokio::time::Instant::now() + link::BUDGET;
    let grants = loop {
        link::wait_for(client, link::BUDGET).await?;
        match flock(client).await {
            Ok(listed) => break bus.read(listed, Instant::now(), desk),
            Err(e) if tokio::time::Instant::now() >= deadline => return Err(e),
            Err(_) => link::pause().await,
        }
    };
    println!("the shepherd is back: subscribed again");
    Ok((events, grants))
}

// Waits until `at`, or for ever with none.
async fn until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

type FlockTx = mpsc::UnboundedSender<(Sent, Result<Flock, String>)>;

// Reads the flock off the loop, so the events keep draining meanwhile.
fn read_apart(client: &Arc<ReconnectingClient>, sent: Sent, back: FlockTx) {
    let client = Arc::clone(client);
    tokio::spawn(async move {
        let _ = back.send((sent, flock(&client).await));
    });
}

struct Flock {
    names: HashMap<u32, String>,
    live: HashMap<String, u32>,
}

async fn flock(client: &ReconnectingClient) -> Result<Flock, String> {
    let reply = client
        .request(Request::ListFlock)
        .await
        .map_err(|e| format!("cannot list the flock: {e}"))?;
    let Response::Flock(sheep) = reply else {
        return Err(format!("the flock listing came back as {reply:?}"));
    };
    let names = sheep.iter().map(|s| (s.id, s.name.clone())).collect();
    let live = sheep
        .iter()
        .filter(|s| s.status == ProcStatus::Online)
        .filter_map(|s| Some((s.name.clone(), s.pid?)))
        .collect();
    Ok(Flock { names, live })
}

// Sends each grant off the loop, so the events keep draining while it waits
// on the runner.
fn deliver(client: &Arc<ReconnectingClient>, grants: Vec<Delivery>) {
    for grant in grants {
        let client = Arc::clone(client);
        tokio::spawn(async move { deliver_grant(&client, &grant).await });
    }
}

// A grant that does not arrive is not retried: the runner raises its
// totals again while it waits, and a runner that is gone is reclaimed.
async fn deliver_grant(client: &ReconnectingClient, grant: &Delivery) {
    let Delivery {
        project,
        kind,
        epoch,
    } = grant;
    let reply = client
        .request(Request::Trigger {
            selector: SelectorSpec::Name(project.as_str().to_owned()),
            action: GRANT.into(),
            params: Some(grant_params(kind, *epoch)),
        })
        .await;
    let outcome = match reply {
        Ok(Response::Triggered(rows)) => rows
            .into_iter()
            .map(|row| match row.outcome {
                ActionOutcome::Replied { body } => body,
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Ok(other) => format!("{other:?}"),
        Err(e) => e.to_string(),
    };
    println!(
        "granted {kind} to {} run {}: {outcome}",
        project.as_str(),
        epoch.0
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lease::LeaseKind;
    use crate::ports::{Pace, Timestamp};
    use crate::review_bot::{Bot, ReviewWindow};
    use crate::test::{EPOCH, FakeClock, RecordedSink};

    fn opened(file: BookFile) -> Kept {
        open(
            file,
            Box::new(FakeClock::at(EPOCH)),
            Reviewers::default(),
            RecordedSink::default().recorder(),
        )
    }

    fn window(kept: &mut Kept) -> serde_json::Value {
        let (body, _) = kept.desk.answer("status", None);
        let status: serde_json::Value = serde_json::from_str(&body).unwrap();
        let leases = status["leases"].as_array().unwrap();
        let line = leases.iter().find(|l| l["kind"] == "coderabbit").unwrap();
        line["window"].clone()
    }

    fn kept(file: BookFile) -> Kept {
        let desk = Desk::new(
            Box::new(FakeClock::at(EPOCH)),
            Reviewers::default(),
            RecordedSink::default().recorder(),
        );
        let last = desk.saved();
        Kept {
            desk,
            file,
            last,
            pace: Arc::new(LocalPace::new()),
        }
    }

    fn take(desk: &mut Desk) {
        desk.answer("take", Some("stand-in"));
    }

    // `deliver` is not async: the loop is back reading events as it returns,
    // and the trigger goes out from a task of its own.
    #[tokio::test]
    async fn a_grant_is_sent_without_the_loop_waiting_on_it() {
        use crate::home::ProjectName;
        use crate::lease::Epoch;
        use crate::test::CALL_PATIENCE;
        use shep_client::dogs::DogIdentity;
        use shep_client::testing::{
            Handshake, control_address, fake_daemon_across_handovers, sample_ack,
        };

        let dir = tempfile::tempdir().unwrap();
        let socket = control_address(dir.path());
        let shepherd = fake_daemon_across_handovers(&socket, vec![Handshake::Accept(sample_ack())]);
        let identity = DogIdentity::named("kelpie");
        let client = Arc::new(
            link::connect(&socket, &identity, "for requests")
                .await
                .unwrap(),
        );
        let grant = Delivery {
            project: ProjectName::try_from("koji").unwrap(),
            kind: LeaseKind::try_from("stand-in").unwrap(),
            epoch: Epoch(101),
        };
        deliver(&client, vec![grant]);
        let sent = async {
            loop {
                let triggers = (shepherd.envelopes().into_iter())
                    .filter(|(_, e)| matches!(&e.body, Request::Trigger { action, .. } if action == GRANT))
                    .count();
                if triggers == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(CALL_PATIENCE, sent)
            .await
            .expect("no grant reached the shepherd");
    }

    #[test]
    fn a_change_is_saved_and_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = BookFile::new(dir.path().join("book.json"));
        let mut kept = kept(file.clone());
        kept.change(take);
        let saved = file.load().unwrap().unwrap();
        assert_eq!(saved, kept.desk.saved());
        let stand_in = LeaseKind::try_from("stand-in").unwrap();
        assert!(saved.leases.iter().any(|l| l.kind == stand_in));
    }

    #[test]
    fn nothing_changed_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = BookFile::new(dir.path().join("book.json"));
        let mut kept = kept(file.clone());
        kept.change(|d| d.answer("status", None));
        assert!(!file.path().exists());
    }

    #[test]
    fn a_failed_save_is_tried_again_on_the_next_change() {
        let dir = tempfile::tempdir().unwrap();
        let file = BookFile::new(dir.path().join("dog/book.json"));
        let mut kept = kept(file.clone());
        kept.change(take);
        assert!(!file.path().exists(), "no dog folder yet");
        std::fs::create_dir(dir.path().join("dog")).unwrap();
        kept.change(|d| d.answer("take", Some("other")));
        let kinds: Vec<String> = file
            .load()
            .unwrap()
            .unwrap()
            .leases
            .iter()
            .map(|l| l.kind.to_string())
            .collect();
        assert_eq!(kinds, ["coderabbit", "other", "stand-in"]);
    }

    #[test]
    fn a_missing_or_older_book_starts_empty_with_the_window_open() {
        let dir = tempfile::tempdir().unwrap();
        let file = BookFile::new(dir.path().join("book.json"));
        let mut kept = opened(file.clone());
        assert_eq!(
            kept.desk.book.status().len(),
            1,
            "the CodeRabbit window alone"
        );
        assert_eq!(window(&mut kept)["opens"], serde_json::Value::Null);

        std::fs::write(file.path(), r#"{"leases": {"coderabbit": "koji"}}"#).unwrap();
        let mut kept = opened(file);
        assert_eq!(window(&mut kept)["opens"], serde_json::Value::Null);
    }

    // A broken book may have held a summon: losing it is the double summon
    // the book file exists to prevent.
    #[test]
    fn a_malformed_book_starts_with_the_window_closed_for_the_hour() {
        let dir = tempfile::tempdir().unwrap();
        let file = BookFile::new(dir.path().join("book.json"));
        std::fs::write(file.path(), r#"{"version": 1, "leases": "#).unwrap();
        let mut kept = opened(file.clone());
        assert_eq!(
            window(&mut kept),
            serde_json::json!({ "quota": 1, "summons": [EPOCH], "opens": EPOCH + 3600 })
        );
        kept.change(Desk::tick);
        assert_eq!(
            file.load().unwrap().unwrap(),
            kept.desk.saved(),
            "the first change replaces the broken file"
        );
    }

    #[test]
    fn a_bad_leases_table_leaves_the_rest_standing() {
        let text = "gpu_metrics_url = \"http://gpu-box:9835/metrics\"\n[leases]\ncargo-test = 0\n";
        let settings = salvage(text);
        assert!(settings.gpu_metrics_url.is_some());
        assert_eq!(settings.leases.cargo_test_capacity().get(), 3);
    }

    #[test]
    fn a_ca_cert_that_cannot_be_read_stops_the_dog_naming_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("s3cr3t-ca.pem");
        let section = format!(
            "[webhook]\nkind = \"ntfy\"\nurl = \"http://ntfy.tail1234.ts.net/t\"\n\
             ca_cert = {:?}\n[leases]\ncargo-test = 5\n",
            ca.display().to_string()
        );
        let err = read_section(&section).unwrap_err();
        assert!(err.contains("`webhook.ca_cert`"), "{err}");
        assert!(!err.contains("s3cr3t"), "{err}");

        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\n").unwrap();
        let settings = read_section(&section).unwrap();
        assert_eq!(settings.leases.cargo_test_capacity().get(), 5);
    }

    fn refusal(code: RpcErrorCode) -> RequestError {
        RequestError::Rpc(shep_client::shep_core::protocol::RpcError {
            code,
            message: "[kelpie] webhook.token: no secret named ntfy".into(),
            daemon_version: None,
        })
    }

    #[test]
    fn a_secret_the_shepherd_cannot_resolve_stops_the_dog_naming_the_reference() {
        for code in [RpcErrorCode::InvalidConfig, RpcErrorCode::Unsupported] {
            let err = section_settings(Err(refusal(code))).unwrap_err();
            assert!(err.contains("no secret named ntfy"), "{err}");
        }
    }

    #[test]
    fn a_section_that_cannot_be_asked_for_holds_the_default_settings() {
        for answer in [RequestError::Closed, refusal(RpcErrorCode::NotFound)] {
            let settings = section_settings(Err(answer)).unwrap();
            assert!(settings.webhook.is_none());
        }
    }

    #[test]
    fn a_trusted_webhook_with_no_token_stops_the_dog_naming_the_key() {
        let section = "[webhook]\nkind = \"ntfy\"\nurl = \"https://ntfy.example/t\"\n\
                       trusted = true\n[leases]\ncargo-test = 5\n";
        let err = read_section(section).unwrap_err();
        assert!(err.contains("`webhook.trusted`"), "{err}");

        let with_token = section.replace("trusted", "token = \"tk_abc\"\ntrusted");
        assert!(read_section(&with_token).unwrap().webhook.unwrap().trusted);
    }

    #[test]
    fn refused_review_windows_leave_the_leases_standing() {
        let settings =
            salvage("[reviewers.cubic]\nreviews = 20\nhours = 720\n[leases]\ncargo-test = 5\n");
        assert_eq!(settings.leases.cargo_test_capacity().get(), 5);
    }

    fn month(reviews: u32) -> ReviewWindow {
        ReviewWindow {
            reviews: std::num::NonZeroU32::new(reviews).unwrap(),
            hours: std::num::NonZeroU32::new(720).unwrap(),
        }
    }

    #[test]
    fn each_bots_window_is_its_agent_file_s() {
        let dir = tempfile::tempdir().unwrap();
        let none = windows_of(&agents_in(&dir.path().join("agents")));
        assert_eq!(
            none.window(Bot::Cubic),
            Some(month(20)),
            "kelpie's own cubic file"
        );
        assert_eq!(none.window(Bot::Coderabbit), Some(ReviewWindow::HOURLY));
        let cubic = "---\nrole: reviewer\nharness: bot\nbot: cubic\nreviews: 5\nhours: 720\n---\n";
        crate::test::write_in(dir.path(), "cubic.md", cubic);
        assert_eq!(
            windows_of(&agents_in(dir.path())).window(Bot::Cubic),
            Some(month(5))
        );
    }

    // Its summons stand, and each window's span is its bot's file's.
    #[test]
    fn a_book_saved_before_bot_files_loads_with_each_files_window() {
        let dir = tempfile::tempdir().unwrap();
        let file = BookFile::new(dir.path().join("book.json"));
        let window = |quota: u32| {
            serde_json::json!({
                "quota": quota, "quota_at": null, "summons": [EPOCH - 600], "refusal": null,
            })
        };
        let saved = serde_json::json!({
            "version": 1,
            "leases": [
                { "kind": "coderabbit", "held": null, "queue": [], "window": window(1) },
                { "kind": "cubic", "held": null, "queue": [], "window": window(20) },
            ],
            "runs": [],
            "retired": [],
        });
        std::fs::write(file.path(), saved.to_string()).unwrap();
        let mut kept = open(
            file,
            Box::new(FakeClock::at(EPOCH)),
            windows_of(&agents_in(&dir.path().join("agents"))),
            RecordedSink::default().recorder(),
        );
        assert_eq!(
            window_of(&mut kept, "coderabbit"),
            serde_json::json!({ "quota": 1, "summons": [EPOCH - 600], "opens": EPOCH + 3000 })
        );
        assert_eq!(
            window_of(&mut kept, "cubic"),
            serde_json::json!({ "quota": 20, "summons": [EPOCH - 600], "opens": null }),
            "one of cubic's twenty a month"
        );
    }

    fn window_of(kept: &mut Kept, kind: &str) -> serde_json::Value {
        let (body, _) = kept.desk.answer("status", None);
        let status: serde_json::Value = serde_json::from_str(&body).unwrap();
        let leases = status["leases"].as_array().unwrap();
        leases.iter().find(|l| l["kind"] == kind).unwrap()["window"].clone()
    }

    #[test]
    fn agent_files_that_cannot_be_used_leave_kelpies_own_windows() {
        let dir = tempfile::tempdir().unwrap();
        crate::test::write_in(
            dir.path(),
            "cubic.md",
            "---\nrole: reviewer\nharness: bot\n---\n",
        );
        let windows = windows_of(&agents_in(dir.path()));
        assert_eq!(windows, Reviewers::from_agents(&Agents::embedded()));
        assert_eq!(windows.window(Bot::Cubic), Some(month(20)));
    }

    #[test]
    fn the_dogs_start_puts_back_a_baseline_that_was_edited_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join(agents::BASELINES);
        refresh_baselines_in(dir.path());
        let shipped = std::fs::read_to_string(kept.join("triager.md")).unwrap();
        assert!(shipped.contains("Kelpie writes this file"), "{shipped}");

        let edited = kept.join("triager.md");
        std::fs::remove_file(&edited).unwrap();
        std::fs::write(&edited, "mine").unwrap();
        std::fs::remove_file(kept.join("pm.md")).unwrap();
        refresh_baselines_in(dir.path());
        assert_eq!(std::fs::read_to_string(&edited).unwrap(), shipped);
        assert!(kept.join("pm.md").is_file());
        assert!(std::fs::metadata(&edited).unwrap().permissions().readonly());
    }

    #[test]
    fn the_dogs_start_migrates_unbound_stubs_and_says_each_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pm.md"), "---\nextends: kelpie:pm\n---\n").unwrap();
        std::fs::write(
            dir.path().join("mine.md"),
            "---\nextends: kelpie:spec\nharness: pi\n---\n",
        )
        .unwrap();

        let said = migrate_agents_in(dir.path());

        assert_eq!(said.len(), 2, "{said:?}");
        assert_eq!(
            said[0],
            "agent files: pm.md: wrote claude-code claude-opus-5-5 medium"
        );
        assert!(said[1].starts_with("agent files: mine.md"), "{said:?}");
        let pm = std::fs::read_to_string(dir.path().join("pm.md")).unwrap();
        assert!(pm.contains("model: claude-opus-5-5\n"), "{pm}");
        assert_eq!(
            migrate_agents_in(dir.path()).len(),
            1,
            "only the skipped file again"
        );
    }

    #[test]
    fn the_dogs_tick_sends_a_hold_that_ended_and_every_series_again_each_minute() {
        let dir = tempfile::tempdir().unwrap();
        let (clock, samples) = (FakeClock::at(EPOCH), RecordedSink::default());
        let reviewers = Reviewers::default();
        let file = BookFile::new(dir.path().join("book.json"));
        let mut kept = open(file, Box::new(clock.clone()), reviewers, samples.recorder());
        kept.pace.hold(Timestamp(EPOCH + 30));
        kept.tick();
        assert_eq!(samples.value("kelpie_forge_held", &[]), Some(1.0));

        clock.advance(30);
        kept.tick();
        assert_eq!(samples.value("kelpie_forge_held", &[]), Some(0.0));

        let held = ("kind", "coderabbit");
        let sent = |samples: &RecordedSink| {
            (samples.sent().iter())
                .filter(|s| {
                    s.name == "kelpie_leases_held" && s.labels.contains(&("kind", held.1.into()))
                })
                .count()
        };
        let before = sent(&samples);
        clock.advance(60);
        kept.tick();
        assert_eq!(sent(&samples), before + 1, "a minute on, sent again");
    }
}
