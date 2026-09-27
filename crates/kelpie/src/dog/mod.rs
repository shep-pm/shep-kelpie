//! The kelpie dog: the lease book, run as a sheep under kelpie's shepherd
//!
//! It listens on the shepherd's bus for runners' lease metrics and their
//! process events, keeps the book on its [`desk`], and grants with a
//! `grant` trigger on the runner. The maintainer reaches it with
//! `shep trigger kelpie <status|take|return>`. Its flock entry needs
//! `channel = true`, and `shutdown_with_message = true` for a clean stop.

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
use crate::lease::wire::{GRANT, grant_params};
use desk::{Delivery, Desk};
use triggers::ACTIONS;

/// The dog's sheep name, which `kelpie lease` triggers
pub const NAME: &str = "kelpie";

/// How long queued replies get to reach the shepherd before the dog exits
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

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
    let desk = Arc::new(Mutex::new(Desk::new(Box::new(SystemClock), lock)));
    let (deliver, mut to_deliver) = mpsc::unbounded_channel::<Vec<Delivery>>();
    for action in ACTIONS {
        let (desk, deliver) = (Arc::clone(&desk), deliver.clone());
        shepherd.on_action(action, move |params, name| {
            let (body, grants) = lock_desk(&desk).answer(name, params);
            let _ = deliver.send(grants);
            body
        });
    }
    let (stop, mut stopped) = mpsc::unbounded_channel();
    shepherd.on_shutdown(move || {
        let _ = stop.send(());
    });

    let mut names = flock(&client).await?.names;
    shepherd.ready().map_err(|e| e.to_string())?;
    println!("up");
    let ended = loop {
        let grants = tokio::select! {
            event = events.next() => match on_event(event, &desk, &client, &mut names).await {
                Ok(grants) => grants,
                Err(e) => break Err(e),
            },
            Some(grants) = to_deliver.recv() => grants,
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
fn lock_desk(desk: &Mutex<Desk>) -> std::sync::MutexGuard<'_, Desk> {
    desk.lock().unwrap_or_else(PoisonError::into_inner)
}

type Event = Option<Result<BusEvent, shep_client::Lagged>>;

async fn on_event(
    event: Event,
    desk: &Mutex<Desk>,
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
            Ok(lock_desk(desk).metric(sheep, &name, value))
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
            Ok(lock_desk(desk).runner_is(&info.name, live))
        }
        BusEvent::Dropped { count } => {
            println!("the bus dropped {count} events: checking every runner");
            resync(desk, client, names).await
        }
        _ => Ok(Vec::new()),
    }
}

async fn resync(
    desk: &Mutex<Desk>,
    client: &Client,
    names: &mut HashMap<u32, String>,
) -> Result<Vec<Delivery>, String> {
    let listed = flock(client).await?;
    *names = listed.names;
    Ok(lock_desk(desk).resync(&listed.live))
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
}
