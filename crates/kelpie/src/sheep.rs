//! A runner as a sheep: the shepherd channel wired to a [`Runner`]
//!
//! The runner's flock entry needs `channel = true`, and
//! `shutdown_with_message = true` so a stop reaches it as a message.
//! Triggers are answered at once; the worker's turns run on a thread of
//! their own, woken by each trigger and by a look at the board every minute.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::adapters::{ClaudeCli, Gh, SystemClock};
use crate::ports::Ports;
use crate::runner::{ACTIONS, ProjectName, ProjectPaths, Runner, TurnReport, answer, step};

/// How long queued replies get to reach the shepherd before the runner exits
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

// How often an idle runner looks at the board. Each look is two `gh` calls,
// 120 an hour, against GitHub's 5,000 an hour for the maintainer's login.
const BOARD_POLL: Duration = Duration::from_secs(60);

/// Runs `project`'s runner until the shepherd stops it
///
/// Kelpie's home is `KELPIE_HOME`, or `~/.kelpie` when that is unset.
pub fn run(project: &str) -> ExitCode {
    match serve(project) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("kelpie runner {project}: {message}");
            ExitCode::FAILURE
        }
    }
}

fn serve(project: &str) -> Result<(), String> {
    let project = ProjectName::try_from(project).map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let kelpie_home = std::env::var_os("KELPIE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".kelpie"));
    let kelpie = std::env::current_exe().map_err(|e| format!("cannot find kelpie itself: {e}"))?;
    let paths = ProjectPaths::under(&kelpie_home, &project);
    let claude = ClaudeCli::default();
    let ports = Ports {
        claude: Arc::new(claude.clone()),
        forge: Box::new(Gh),
        clock: Box::new(SystemClock),
    };
    let runner = Runner::open(project, &paths, &home, &kelpie, ports).map_err(|e| e.to_string())?;

    let shepherd = shep_channel::serve();
    if !shepherd.is_active() {
        return Err("no shepherd channel: run it under shep with `channel = true`".into());
    }
    let status = serde_json::to_string(&runner.status()).expect("status serializes to JSON");
    println!("up: {status}");

    let runner = Arc::new(Mutex::new(runner));
    let (wake, woken) = mpsc::channel();
    for action in ACTIONS {
        let runner = Arc::clone(&runner);
        let wake = wake.clone();
        shepherd.on_action(action, move |params, name| {
            let reply = answer(&runner, name, params);
            let _ = wake.send(());
            reply
        });
    }
    let (stop, stopped) = mpsc::channel();
    let worker = Arc::clone(&runner);
    let died = stop.clone();
    std::thread::spawn(move || {
        // A runner with no worker thread would answer triggers and never work.
        if catch_unwind(AssertUnwindSafe(|| work(&worker, &woken))).is_err() {
            let _ = died.send(Stop::WorkerDied);
        }
    });
    shepherd.on_shutdown(move || {
        let _ = stop.send(Stop::Shutdown);
    });
    shepherd.ready().map_err(|e| e.to_string())?;

    // Only a shutdown message or a dead worker thread ends the wait. Without
    // either, the shepherd's stop signal ends the process instead. Exiting on
    // the message skips shep's stop ladder, so the worker is stopped here.
    let why = stopped.recv();
    claude.stop();
    shepherd.flush(FLUSH_TIMEOUT).map_err(|e| e.to_string())?;
    match why {
        Ok(Stop::WorkerDied) => {
            Err("the worker's thread panicked; the turn resumes on restart".into())
        }
        _ => Ok(()),
    }
}

/// Why the runner stops
enum Stop {
    /// The shepherd sent its shutdown message
    Shutdown,
    /// The thread that runs turns panicked
    WorkerDied,
}

// Runs steps while there are any, then sleeps until a trigger or the next
// look at the board. A turn cut short by a restart is resumed on the first pass.
fn work(runner: &Mutex<Runner>, woken: &Receiver<()>) {
    loop {
        match step(runner) {
            Ok(Some(report)) => {
                let line = serde_json::to_string(&report).expect("a report serializes to JSON");
                println!("{line}");
                if !matches!(report, TurnReport::BoardFailed { .. }) {
                    continue;
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("cannot save the worker's turn: {e}"),
        }
        if let Err(RecvTimeoutError::Disconnected) = woken.recv_timeout(BOARD_POLL) {
            return;
        }
    }
}
