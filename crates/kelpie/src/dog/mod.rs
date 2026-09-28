//! The kelpie dog: the lease book, run as a sheep under kelpie's shepherd
//!
//! It listens on the shepherd's bus for runners' lease metrics and their
//! process events, keeps the book on its [`desk`], and grants with a
//! `grant` trigger on the runner. The maintainer reaches it with
//! `shep trigger kelpie <status|take|return>`. Its flock entry needs
//! `channel = true`, and `shutdown_with_message = true` for a clean stop.
//! The book is saved to `<kelpie home>/dog/book.json` after every change
//! and loaded on start.

pub mod desk;
pub mod triggers;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use shep_client::Client;
use shep_client::shep_core::protocol::request::{ActionOutcome, Response};
use shep_client::shep_core::protocol::{
    BusEvent, ChildMessage, ProcessEventKind, Request, SelectorSpec,
};
use shep_client::shep_core::status::ProcStatus;
use tokio::sync::mpsc;

use crate::adapters::SystemClock;
use crate::lease::gpu::{self, GpuLock};
use crate::lease::saved::{BookFile, SavedBook};
use crate::lease::wire::{GRANT, grant_params};
use desk::{Delivery, Desk};
use triggers::ACTIONS;

/// The dog's sheep name, which `kelpie lease` triggers
pub const NAME: &str = "kelpie";

/// How long queued replies get to reach the shepherd before the dog exits
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the dog looks for a review window that has opened. CodeRabbit
/// quotes waits to the minute, so a few seconds late costs nothing.
const WINDOW_TICK: Duration = Duration::from_secs(10);

/// Runs the dog until the shepherd stops it
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

// Another build's file, or none, starts an empty book. So does one that
// cannot be read: the dog still runs, and says why.
fn load(file: &BookFile) -> SavedBook {
    let empty = || SavedBook::new(Vec::new(), Vec::new(), Vec::new());
    match file.load() {
        Ok(Some(saved)) => saved,
        Ok(None) => empty(),
        Err(e) => {
            println!("{e}: starting with an empty book");
            empty()
        }
    }
}

async fn serve() -> Result<(), String> {
    let shepherd = shep_channel::serve();
    if !shepherd.is_active() {
        return Err("no shepherd channel: run it under shep with `channel = true`".into());
    }
    let socket = shepherd_socket()?;
    let connect = |what: &'static str| {
        let socket = socket.clone();
        async move {
            Client::connect(&socket).await.map_err(|e| {
                format!(
                    "cannot reach the shepherd at {} {what}: {e}",
                    socket.display()
                )
            })
        }
    };
    let (listener, client) = (connect("for events").await?, connect("for requests").await?);
    let mut events = listener
        .subscribe(vec!["channel.metric".into(), "process.*".into()])
        .await
        .map_err(|e| format!("cannot subscribe to the shepherd's bus: {e}"))?;

    let lock = GpuLock::under(&gpu::temp_dir());
    println!("the GPU lock is {}", lock.path().display());
    let file = BookFile::new(book_path()?);
    if let Some(folder) = file.path().parent() {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
    }
    println!("the book is {}", file.path().display());
    let last = load(&file);
    let desk = Desk::restore(Box::new(SystemClock), lock, last.clone());
    let desk = Arc::new(Mutex::new(Kept { desk, file, last }));
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
            event = events.next() => match on_event(event, &desk, &client, &mut names).await {
                Ok(grants) => grants,
                Err(e) => break Err(e),
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

type Event = Option<Result<BusEvent, shep_client::Lagged>>;

async fn on_event(
    event: Event,
    desk: &Mutex<Kept>,
    client: &Client,
    names: &mut HashMap<u32, String>,
) -> Result<Vec<Delivery>, String> {
    let event = match event {
        None => return Err("the shepherd closed its bus".into()),
        Some(Err(lagged)) => {
            println!("missed bus events ({lagged:?}): checking every runner");
            return resync(desk, client, names).await;
        }
        Some(Ok(event)) => event,
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
    client: &Client,
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

async fn flock(client: &Client) -> Result<Flock, String> {
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
async fn deliver_grant(client: &Client, grant: &Delivery) {
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

    #[test]
    fn the_socket_is_under_shep_home_or_kelpies_shepherd() {
        let socket = |shep_home: Option<&str>, home: Option<&str>| {
            socket_from(shep_home.map(Into::into), home.map(Into::into))
        };
        assert_eq!(
            socket(Some("/s"), Some("/home/m")),
            Ok(PathBuf::from("/s/run/shep.sock"))
        );
        assert_eq!(
            socket(None, Some("/home/m")),
            Ok(PathBuf::from("/home/m/.kelpie/shep/run/shep.sock"))
        );
        assert!(socket(None, None).is_err());
    }

    #[test]
    fn the_book_is_under_kelpie_home_or_the_home_folder() {
        let book = |kelpie_home: Option<&str>, home: Option<&str>| {
            book_path_from(kelpie_home.map(Into::into), home.map(Into::into))
        };
        assert_eq!(
            book(Some("/k"), Some("/home/m")),
            Ok(PathBuf::from("/k/dog/book.json"))
        );
        assert_eq!(
            book(None, Some("/home/m")),
            Ok(PathBuf::from("/home/m/.kelpie/dog/book.json"))
        );
        assert!(book(None, None).is_err());
    }
}
