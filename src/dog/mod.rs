//! The kelpie dog: the lease book, run by the adopted `kelpie`
//!
//! Shep starts the adopted kelpie with no arguments, and that start is the
//! dog. It asks for the shepherd channel in its `--version` answer, so
//! `shep trigger kelpie <status|take|return>` reaches it. It listens on the
//! shepherd's bus for runners' lease metrics and their process events,
//! keeps the book on its [`desk`], and grants with a `grant` trigger on the
//! runner. The book is saved to `<kelpie home>/dog/book.json` after every
//! change and loaded on start, with a review window for each reviewer
//! kelpie's `[kelpie]` section defines.

pub mod desk;
pub mod left_over;
mod link;
pub mod triggers;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use shep_client::shep_core::protocol::request::{ActionOutcome, Response};
use shep_client::shep_core::protocol::{
    BusEvent, ChildMessage, ProcessEventKind, Request, SelectorSpec,
};
use shep_client::shep_core::status::ProcStatus;
use shep_client::{EventStream, Lagged, ReconnectingClient};
use tokio::sync::mpsc;

use crate::adapters::SystemClock;
use crate::lease::gpu::{self, GpuLock};
use crate::lease::saved::{BookFile, SavedBook};
use crate::lease::wire::{GRANT, grant_params};
use crate::ports::Clock;
use crate::review_bot::Reviewers;
use crate::shep_home;
use crate::webhook::KelpieSettings;
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
pub fn run() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let served = match runtime {
        Ok(runtime) => runtime.block_on(serve()),
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

/// The shepherd's control socket: `$SHEP_HOME/run/shep.sock`, or under
/// `~/.kelpie/shep` when `SHEP_HOME` is unset
///
/// # Errors
///
/// A message when neither `SHEP_HOME` nor `HOME` is set.
pub fn shepherd_socket() -> Result<PathBuf, String> {
    socket_from(std::env::var_os("SHEP_HOME"), std::env::var_os("HOME"))
}

fn socket_from(shep_home: Option<OsString>, home: Option<OsString>) -> Result<PathBuf, String> {
    let shep_home = match (shep_home, home) {
        (Some(shep_home), _) => PathBuf::from(shep_home),
        (None, Some(home)) => PathBuf::from(home).join(".kelpie/shep"),
        (None, None) => return Err("neither SHEP_HOME nor HOME is set".into()),
    };
    Ok(shep_home.join("run/shep.sock"))
}

/// The dog's book file: `<kelpie home>/dog/book.json`, where kelpie's
/// home is `KELPIE_HOME`, or `~/.kelpie` when that is unset
///
/// # Errors
///
/// A message when neither `KELPIE_HOME` nor `HOME` is set.
pub fn book_path() -> Result<PathBuf, String> {
    book_path_from(std::env::var_os("KELPIE_HOME"), std::env::var_os("HOME"))
}

fn book_path_from(
    kelpie_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, String> {
    let kelpie_home = match (kelpie_home, home) {
        (Some(kelpie_home), _) => PathBuf::from(kelpie_home),
        (None, Some(home)) => PathBuf::from(home).join(".kelpie"),
        (None, None) => return Err("neither KELPIE_HOME nor HOME is set".into()),
    };
    Ok(kelpie_home.join("dog/book.json"))
}

// The desk and the file it is saved to, written whenever the book changes.
struct Kept {
    desk: Desk,
    file: BookFile,
    last: SavedBook,
}

impl Kept {
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
fn open(file: BookFile, clock: Box<dyn Clock>, gpu: GpuLock, reviewers: Reviewers) -> Kept {
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
    let mut desk = Desk::restore(clock, gpu, last.clone(), reviewers);
    if unread {
        for (bot, _) in reviewers.defined() {
            desk.book.summoned(&bot.lease(), now);
        }
    }
    Kept { desk, file, last }
}

// The review windows kelpie's section defines. A section that cannot be
// read books CodeRabbit's alone, as a dog did before definitions.
async fn reviewers(client: &ReconnectingClient) -> Reviewers {
    let section = client.request(Request::DogConfig {
        name: crate::shepherd::DOG.into(),
    });
    let text = match section.await {
        Ok(Response::DogSection { toml }) => toml.as_str().to_owned(),
        Ok(other) => {
            println!("no [kelpie] section in the shepherd's answer: {other:?}");
            return Reviewers::default();
        }
        Err(e) => {
            println!("cannot read the [kelpie] section: {e}");
            return Reviewers::default();
        }
    };
    if text.trim().is_empty() {
        return Reviewers::default();
    }
    match KelpieSettings::from_section(&text) {
        Ok(kelpie) => kelpie.reviewers,
        Err(e) => {
            println!("{e}: booking CodeRabbit's window alone");
            Reviewers::default()
        }
    }
}

/// Why the dog has no shepherd channel, and the fix
///
/// Kelpie asks for it at `shep adopt`, which records the ask, so an
/// adoption from before the ask has none until kelpie is adopted again.
pub fn no_channel() -> String {
    let path = std::env::current_exe()
        .map_or_else(|_| "<path to kelpie>".into(), |p| p.display().to_string());
    format!(
        "no shepherd channel, since kelpie was adopted before it asked for one: run \
         `shep adopt {path} --name {NAME}`, then `shep disable {NAME}` and `shep enable {NAME}`"
    )
}

async fn serve() -> Result<(), String> {
    let shepherd = shep_channel::serve();
    if !shepherd.is_active() {
        return Err(no_channel());
    }
    let socket = shep_home::required(shep_home::DOG_FIX)?.join("run/shep.sock");
    let identity = link::identity();
    let listener = link::connect(&socket, &identity, "for events").await?;
    let client = link::connect(&socket, &identity, "for requests").await?;
    let mut events = link::subscribe(&listener, link::BUDGET).await?;

    let lock = GpuLock::under(&gpu::temp_dir());
    println!("the GPU lock is {}", lock.path().display());
    let file = BookFile::new(book_path()?);
    if let Some(removed) = left_over::remove(&client, file.path()).await? {
        println!("{removed}");
    }
    if let Some(folder) = file.path().parent() {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
    }
    println!("the book is {}", file.path().display());
    let reviewers = reviewers(&client).await;
    let desk = Arc::new(Mutex::new(open(
        file,
        Box::new(SystemClock),
        lock,
        reviewers,
    )));
    let (deliver, mut to_deliver) = mpsc::unbounded_channel::<Vec<Delivery>>();
    for action in ACTIONS {
        let (desk, deliver) = (Arc::clone(&desk), deliver.clone());
        shepherd.on_action(action, move |params, name| {
            let (body, grants) = lock_desk(&desk).change(|d| d.answer(name, params));
            let _ = deliver.send(grants);
            body
        });
    }
    let (stop, mut stopped) = mpsc::unbounded_channel();
    shepherd.on_shutdown(move || {
        let _ = stop.send(());
    });

    // Runners in the saved book that have since died or restarted are
    // reclaimed before the dog takes anything new.
    let listed = flock(&client).await?;
    let mut names = listed.names;
    let restored = lock_desk(&desk).change(|d| d.resync(&listed.live));
    shepherd.ready().map_err(|e| e.to_string())?;
    println!("up");
    for grant in restored {
        deliver_grant(&client, &grant).await;
    }
    let mut ticks = tokio::time::interval(WINDOW_TICK);
    let ended = loop {
        let grants = tokio::select! {
            event = events.next() => {
                let read = match event {
                    Some(event) => on_event(event, &desk, &client, &mut names).await,
                    None => match rejoin(&listener, &client, &desk, &mut names).await {
                        Ok((stream, grants)) => {
                            events = stream;
                            Ok(grants)
                        }
                        Err(e) => Err(e),
                    },
                };
                match read {
                    Ok(grants) => grants,
                    Err(e) => break Err(e),
                }
            },
            Some(grants) = to_deliver.recv() => grants,
            _ = ticks.tick() => lock_desk(&desk).change(Desk::tick),
            _ = stopped.recv() => break Ok(()),
        };
        for grant in grants {
            deliver_grant(&client, &grant).await;
        }
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
    names: &mut HashMap<u32, String>,
) -> Result<(EventStream, Vec<Delivery>), String> {
    println!("the shepherd's bus closed: waiting for it to come back");
    let events = link::subscribe(listener, link::BUDGET).await?;
    let deadline = tokio::time::Instant::now() + link::BUDGET;
    let grants = loop {
        link::wait_for(client, link::BUDGET).await?;
        match resync(desk, client, names).await {
            Ok(grants) => break grants,
            Err(e) if tokio::time::Instant::now() >= deadline => return Err(e),
            Err(_) => link::pause().await,
        }
    };
    println!("the shepherd is back: subscribed again");
    Ok((events, grants))
}

async fn on_event(
    event: Result<BusEvent, Lagged>,
    desk: &Mutex<Kept>,
    client: &ReconnectingClient,
    names: &mut HashMap<u32, String>,
) -> Result<Vec<Delivery>, String> {
    let event = match event {
        Err(lagged) => {
            println!("missed bus events ({lagged:?}): checking every runner");
            return resync(desk, client, names).await;
        }
        Ok(event) => event,
    };
    match event {
        BusEvent::Channel {
            id,
            message: ChildMessage::Metric { name, value },
        } => {
            if !names.contains_key(&id) {
                *names = flock(client).await?.names;
            }
            let Some(sheep) = names.get(&id) else {
                return Ok(Vec::new());
            };
            Ok(lock_desk(desk).change(|d| d.metric(sheep, &name, value)))
        }
        BusEvent::Process { event, info, .. } => {
            names.insert(info.id, info.name.clone());
            let live = match event {
                ProcessEventKind::Restart => info.pid,
                ProcessEventKind::Exit
                | ProcessEventKind::Stop
                | ProcessEventKind::Errored
                | ProcessEventKind::Delete => None,
                _ => return Ok(Vec::new()),
            };
            Ok(lock_desk(desk).change(|d| d.runner_is(&info.name, live)))
        }
        BusEvent::Dropped { count } => {
            println!("the bus dropped {count} events: checking every runner");
            resync(desk, client, names).await
        }
        _ => Ok(Vec::new()),
    }
}

async fn resync(
    desk: &Mutex<Kept>,
    client: &ReconnectingClient,
    names: &mut HashMap<u32, String>,
) -> Result<Vec<Delivery>, String> {
    let listed = flock(client).await?;
    *names = listed.names;
    Ok(lock_desk(desk).change(|d| d.resync(&listed.live)))
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
    use crate::test::FakeClock;

    const NOW: u64 = 1_790_000_000;

    fn opened(dir: &std::path::Path, file: BookFile) -> Kept {
        open(
            file,
            Box::new(FakeClock::at(NOW)),
            GpuLock::under(dir),
            Reviewers::default(),
        )
    }

    fn window(kept: &mut Kept) -> serde_json::Value {
        let (body, _) = kept.desk.answer("status", None);
        let status: serde_json::Value = serde_json::from_str(&body).unwrap();
        let leases = status["leases"].as_array().unwrap();
        let line = leases.iter().find(|l| l["kind"] == "coderabbit").unwrap();
        line["window"].clone()
    }

    fn kept(dir: &std::path::Path, file: BookFile) -> Kept {
        let gpu = GpuLock::under(dir);
        let desk = Desk::new(
            Box::new(FakeClock::at(1_790_000_000)),
            gpu,
            Reviewers::default(),
        );
        let last = desk.saved();
        Kept { desk, file, last }
    }

    fn take(desk: &mut Desk) {
        desk.answer("take", Some("stand-in"));
    }

    #[test]
    fn a_change_is_saved_and_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = BookFile::new(dir.path().join("book.json"));
        let mut kept = kept(dir.path(), file.clone());
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
        let mut kept = kept(dir.path(), file.clone());
        kept.change(|d| d.answer("status", None));
        assert!(!file.path().exists());
    }

    #[test]
    fn a_failed_save_is_tried_again_on_the_next_change() {
        let dir = tempfile::tempdir().unwrap();
        let file = BookFile::new(dir.path().join("dog/book.json"));
        let mut kept = kept(dir.path(), file.clone());
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
        let mut kept = opened(dir.path(), file.clone());
        assert_eq!(
            kept.desk.book.status().len(),
            1,
            "the CodeRabbit window alone"
        );
        assert_eq!(window(&mut kept)["opens"], serde_json::Value::Null);

        std::fs::write(file.path(), r#"{"leases": {"coderabbit": "koji"}}"#).unwrap();
        let mut kept = opened(dir.path(), file);
        assert_eq!(window(&mut kept)["opens"], serde_json::Value::Null);
    }

    // A broken book may have held a summon: losing it is the double summon
    // the book file exists to prevent.
    #[test]
    fn a_malformed_book_starts_with_the_window_closed_for_the_hour() {
        let dir = tempfile::tempdir().unwrap();
        let file = BookFile::new(dir.path().join("book.json"));
        std::fs::write(file.path(), r#"{"version": 1, "leases": "#).unwrap();
        let mut kept = opened(dir.path(), file.clone());
        assert_eq!(
            window(&mut kept),
            serde_json::json!({ "quota": 1, "summons": [NOW], "opens": NOW + 3600 })
        );
        kept.change(Desk::tick);
        assert_eq!(
            file.load().unwrap().unwrap(),
            kept.desk.saved(),
            "the first change replaces the broken file"
        );
    }

    #[test]
    fn the_socket_is_under_shep_home_or_kelpies_shepherd() {
        let socket = |shep_home: Option<&str>, home: Option<&str>| {
            socket_from(shep_home.map(Into::into), home.map(Into::into))
        };
        assert_eq!(
            socket(Some("/s"), Some("/home/me")),
            Ok(PathBuf::from("/s/run/shep.sock"))
        );
        assert_eq!(
            socket(None, Some("/home/me")),
            Ok(PathBuf::from("/home/me/.kelpie/shep/run/shep.sock"))
        );
        assert!(socket(None, None).is_err());
    }

    #[test]
    fn the_book_is_under_kelpie_home_or_the_home_folder() {
        let book = |kelpie_home: Option<&str>, home: Option<&str>| {
            book_path_from(kelpie_home.map(Into::into), home.map(Into::into))
        };
        assert_eq!(
            book(Some("/k"), Some("/home/me")),
            Ok(PathBuf::from("/k/dog/book.json"))
        );
        assert_eq!(
            book(None, Some("/home/me")),
            Ok(PathBuf::from("/home/me/.kelpie/dog/book.json"))
        );
        assert!(book(None, None).is_err());
    }
}
