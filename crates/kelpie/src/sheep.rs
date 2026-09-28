//! A runner as a sheep: the shepherd channel wired to a [`Runner`]
//!
//! The runner's flock entry needs `channel = true`, and
//! `shutdown_with_message = true` so a stop reaches it as a message.
//! Triggers are answered at once; the worker's turns run on a thread of
//! their own, woken by each trigger and by a look at the board every minute.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::adapters::{
    ClaudeCli, Curl, Gh, QwenReviewer, RelayCli, ShepLeases, ShotsCli, SystemClock,
};
use crate::lease::Epoch;
use crate::lease::wire::{Asker, GRANT};
use crate::ports::{Leases, Ports};
use crate::runner::{ACTIONS, ProjectName, ProjectPaths, Runner, answer, step};

/// How long queued replies get to reach the shepherd before the runner exits
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

// How often an idle runner looks at the board, or at its pull request's CI.
// A look is at most two `gh` calls, 120 an hour, against GitHub's 5,000 an
// hour for the maintainer's login.
const BOARD_POLL: Duration = Duration::from_secs(60);

// How long a stopping runner waits for its worker's thread to end. A stop or
// restart gives the runner shep's `kill_timeout` after the shutdown message,
// 1.6s unless its Flockfile entry says more, and 3s of stop ladder and 2s of
// flush follow this wait. The whole stop needs about 7s, so the entry needs
// `kill_timeout = "10s"` or more.
const JOIN_BOUND: Duration = Duration::from_secs(2);

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
    let shepherd = shep_channel::serve();
    let claude = ClaudeCli::default();
    let reviewer = QwenReviewer::new(&home);
    let shots = ShotsCli::new(paths.tools.clone());
    let epoch = Epoch(u64::from(std::process::id()));
    let leases = Arc::new(ShepLeases::new(shepherd.clone(), Asker::new(epoch)));
    let ports = Ports {
        claude: Arc::new(claude.clone()),
        forge: Box::new(Gh),
        meter: Box::new(claude.meter()),
        reviewer: Arc::new(reviewer.clone()),
        shots: Arc::new(shots.clone()),
        relay: Arc::new(RelayCli::new(home.clone(), kelpie_home.join("relay"))),
        alerts: Arc::new(Curl),
        leases: Arc::clone(&leases) as Arc<dyn Leases>,
        clock: Box::new(SystemClock),
    };
    let runner = Runner::open(project, &paths, &home, &kelpie, ports).map_err(|e| e.to_string())?;

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
    // A grant lands in the lease adapter, and the step it wakes reads it.
    let granted = wake.clone();
    shepherd.on_action(GRANT, move |params, _| {
        let reply = match leases.grant(params.unwrap_or_default()) {
            Ok(kind) => serde_json::json!({ "granted": kind }),
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
        let _ = granted.send(());
        reply.to_string()
    });
    let (stop, stopped) = mpsc::channel();
    let worker = Worker::spawn(Arc::clone(&runner), wake.clone(), woken, stop.clone());
    shepherd.on_shutdown(move || {
        let _ = stop.send(Stop::Shutdown);
    });
    shepherd.ready().map_err(|e| e.to_string())?;

    // Only a shutdown message or a dead worker thread ends the wait. Without
    // either, the shepherd's stop signal ends the process instead. Exiting on
    // the message skips shep's stop ladder, so the worker is stopped here.
    let why = stopped.recv();
    let let_go = worker.stop(JOIN_BOUND, || {
        claude.stop();
        reviewer.stop();
        shots.stop();
    });
    if !let_go {
        eprintln!(
            "the worker was still in a step {}s after the stop; its calls are ended under it",
            JOIN_BOUND.as_secs()
        );
    }
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

/// The thread that runs the worker's turns, and the means to stop it
struct Worker {
    stopping: Arc<AtomicBool>,
    wake: Sender<()>,
    // Disconnects when the thread ends, however it ends.
    ended: Receiver<()>,
    thread: JoinHandle<()>,
}

impl Worker {
    /// Starts the thread, which tells `died` if it panics
    fn spawn(
        runner: Arc<Mutex<Runner>>,
        wake: Sender<()>,
        woken: Receiver<()>,
        died: Sender<Stop>,
    ) -> Self {
        let stopping = Arc::new(AtomicBool::new(false));
        let (ending, ended) = mpsc::channel::<()>();
        let flag = Arc::clone(&stopping);
        let thread = std::thread::spawn(move || {
            let _ending = ending;
            // A runner with no worker thread would answer triggers and never work.
            if catch_unwind(AssertUnwindSafe(|| work(&runner, &woken, &flag))).is_err() {
                let _ = died.send(Stop::WorkerDied);
            }
        });
        Self {
            stopping,
            wake,
            ended,
            thread,
        }
    }

    /// Asks the thread to stop, waits up to `bound` for it, then runs `stop_ports`
    ///
    /// A step in flight runs to its end first, so the ports stop only once
    /// the worker has let go of them. Returns whether it let go in time.
    fn stop(self, bound: Duration, stop_ports: impl FnOnce()) -> bool {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
        let let_go = matches!(
            self.ended.recv_timeout(bound),
            Err(RecvTimeoutError::Disconnected)
        );
        if let_go {
            // It has returned already, and caught its own panic.
            let _ = self.thread.join();
        }
        stop_ports();
        let_go
    }
}

// Runs steps while there are any, then sleeps until a trigger or the next
// look at the board. A turn cut short by a restart is resumed on the first pass.
fn work(runner: &Mutex<Runner>, woken: &Receiver<()>, stopping: &AtomicBool) {
    while !stopping.load(Ordering::SeqCst) {
        match step(runner) {
            Ok(Some(report)) => {
                let line = serde_json::to_string(&report).expect("a report serializes to JSON");
                println!("{line}");
                if !report.waits() {
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Instant;

    use super::*;
    use crate::ports::ReviewerError;
    use crate::test::{Hold, Rig, Scripted, ScriptedRound};

    // The worker is a real thread on real time, so every wait has this ceiling.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// A worker whose first turn on issue 7 is held open by `hold`
    fn in_a_turn(rig: &Rig, hold: &Hold) -> Worker {
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.claude.script([Scripted::Hold(hold.clone())]);
        let worker = spawn(runner);
        assert!(hold.entered(PATIENCE), "the worker's turn never began");
        worker
    }

    fn spawn(runner: Mutex<Runner>) -> Worker {
        let (wake, woken) = mpsc::channel();
        let (died, _) = mpsc::channel();
        Worker::spawn(Arc::new(runner), wake, woken, died)
    }

    fn eventually(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !done() {
            assert!(Instant::now() < deadline, "never saw {what}");
            std::thread::yield_now();
        }
    }

    /// How many calls the state file records for the work item
    fn saved_calls(state: &Path) -> usize {
        let text = std::fs::read_to_string(state).unwrap();
        let state: serde_json::Value = serde_json::from_str(&text).unwrap();
        state["work_item"]["calls"].as_array().map_or(0, Vec::len)
    }

    #[test]
    fn the_ports_stop_after_the_turn_in_flight_has_ended_and_been_saved() {
        let rig = Rig::new("shep");
        let hold = Hold::default();
        let worker = in_a_turn(&rig, &hold);
        let signalled = Arc::clone(&worker.stopping);
        let at_stop = Arc::new(Mutex::new(None));
        let stopping = std::thread::spawn({
            let (hold, state, at_stop) = (hold.clone(), rig.paths().state, Arc::clone(&at_stop));
            move || {
                worker.stop(PATIENCE, || {
                    *at_stop.lock().unwrap() = Some((hold.returned(), saved_calls(&state)));
                })
            }
        });
        // The turn ends only once the stop has been asked for.
        eventually("the stop's signal", || signalled.load(Ordering::SeqCst));
        hold.release();

        assert!(
            stopping.join().unwrap(),
            "the worker did not let go in time"
        );
        assert_eq!(*at_stop.lock().unwrap(), Some((true, 1)));
        assert_eq!(
            rig.claude.all_calls().len(),
            1,
            "a step began after the stop"
        );
        assert!(
            rig.reviewer.seen().is_empty(),
            "a review began after the stop"
        );
    }

    #[test]
    fn a_turn_that_outlasts_the_bound_has_its_ports_stopped_under_it() {
        let rig = Rig::new("shep");
        let hold = Hold::default();
        let worker = in_a_turn(&rig, &hold);
        let at_stop = Arc::new(Mutex::new(None));

        let started = Instant::now();
        let let_go = worker.stop(Duration::from_millis(50), || {
            *at_stop.lock().unwrap() = Some(hold.returned());
            // Stopping the real ports ends a call in flight.
            hold.release();
        });

        assert!(!let_go);
        assert!(
            started.elapsed() < PATIENCE,
            "the stop waited past its bound"
        );
        assert_eq!(*at_stop.lock().unwrap(), Some(false));
        assert!(hold.answered(PATIENCE));
    }

    #[test]
    fn a_worker_waiting_for_its_next_look_lets_go_as_soon_as_it_is_asked() {
        let rig = Rig::new("shep");
        let hold = Hold::default();
        let worker = in_a_turn(&rig, &hold);
        // A failed review round is a step the worker waits after.
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        let down = ReviewerError::Failed("the GPU is gone".into());
        rig.reviewer.script([ScriptedRound::Fail(down)]);
        hold.release();
        eventually("the review round", || !rig.reviewer.seen().is_empty());

        assert!(
            worker.stop(JOIN_BOUND, || {}),
            "the worker slept through the stop"
        );
    }
}
