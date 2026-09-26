//! A runner as a sheep: the shepherd channel wired to a [`Runner`]
//!
//! The runner's flock entry needs `channel = true`, and
//! `shutdown_with_message = true` so a stop reaches it as a message.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::adapters::{ClaudeCli, Gh, SystemClock};
use crate::ports::Ports;
use crate::runner::{ACTIONS, ProjectName, ProjectPaths, Runner, answer};

/// How long queued replies get to reach the shepherd before the runner exits
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

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
    let paths = ProjectPaths::under(&kelpie_home, &project);
    let ports = Ports {
        claude: Box::new(ClaudeCli),
        forge: Box::new(Gh),
        clock: Box::new(SystemClock),
    };
    let runner = Runner::open(project, &paths, &home, ports).map_err(|e| e.to_string())?;

    let shepherd = shep_channel::serve();
    if !shepherd.is_active() {
        return Err("no shepherd channel: run it under shep with `channel = true`".into());
    }
    let status = serde_json::to_string(&runner.status()).expect("status serializes to JSON");
    println!("up: {status}");

    let runner = Arc::new(Mutex::new(runner));
    for action in ACTIONS {
        let runner = Arc::clone(&runner);
        shepherd.on_action(action, move |params, name| answer(&runner, name, params));
    }
    let (stop, stopped) = mpsc::channel();
    shepherd.on_shutdown(move || {
        let _ = stop.send(());
    });
    shepherd.ready().map_err(|e| e.to_string())?;

    // Only a shutdown message ends the wait. Without one, the shepherd's
    // stop signal ends the process instead.
    let _ = stopped.recv();
    shepherd.flush(FLUSH_TIMEOUT).map_err(|e| e.to_string())
}
