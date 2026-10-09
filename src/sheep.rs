//! A runner as a sheep: the shepherd channel wired to a [`Runner`]
//!
//! The runner's flock entry needs `channel = true`, and
//! `shutdown_with_message = true` so a stop reaches it as a message.
//! Triggers are answered at once. The runner's loop runs on a thread of its
//! own, woken by each trigger, by each call in flight that ends, and by a
//! look at the board every minute, or more often while a ruling waits on a
//! reply on the webhook's topic. It starts each agent call and goes on, and
//! never waits on one.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::adapters::{
    ClaudeCli, Curl, Gh, GpuCurl, LocalReviewer, ShepLeases, SystemClock, stop_calls,
};
use crate::coderabbit::CodeRabbit;
use crate::codex::Codex;
use crate::cubic::Cubic;
use crate::lease::Epoch;
use crate::lease::saved::BookFile;
use crate::lease::wire::{Asker, GRANT};
use crate::ports::{Leases, Ports, Routed, SandboxError};
use crate::runner::{
    ACTIONS, Pass, ProjectName, ProjectPaths, READ_EVERY, Runner, advance, answer,
};
use crate::shep_home;

/// How long queued replies get to reach the shepherd before the runner exits
///
/// With [`JOIN_BOUND`], 1.5s, inside shep's default `kill_timeout` of 1.6s.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(500);

// How often an idle runner looks at the board, or at its pull request's CI.
// A look is at most two `gh` calls, 120 an hour, against GitHub's 5,000 an
// hour for the maintainer's login.
const BOARD_POLL: Duration = Duration::from_secs(60);

// How long a stopping runner waits for its loop's thread, whose pass waits
// only on short git and gh calls. With the flush after it, it fits inside
// shep's default `kill_timeout` of 1.6s, past which shep kills the runner.
const JOIN_BOUND: Duration = Duration::from_secs(1);

mod look;

use look::Look;

/// Runs `project`'s runner until the shepherd stops it
///
/// Kelpie's home is `KELPIE_HOME`, or `$SHEP_HOME/kelpie` when that is unset.
pub fn run(project: &str) -> ExitCode {
    match serve(project) {
        Ok(()) => ExitCode::SUCCESS,
        Err(exit) => {
            eprintln!("kelpie runner {project}: {}", exit.message());
            ExitCode::from(exit.code())
        }
    }
}

/// The exit code of a runner that refused to start on something only the
/// maintainer can fix, `EX_CONFIG`
///
/// A runner's flock entry lists it in `stop_exit_codes`, so the shepherd
/// leaves it stopped instead of restarting it into the same refusal.
pub const REFUSED: u8 = 78;

// Why a runner stopped before it served.
#[derive(Debug, PartialEq)]
enum Exit {
    // A refusal a restart cannot clear.
    Refused(String),
    // Anything else, which shep restarts.
    Failed(String),
}

impl Exit {
    fn message(&self) -> &str {
        match self {
            Self::Refused(message) | Self::Failed(message) => message,
        }
    }

    fn code(&self) -> u8 {
        match self {
            Self::Refused(_) => REFUSED,
            Self::Failed(_) => 1,
        }
    }
}

impl From<String> for Exit {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for Exit {
    fn from(message: &str) -> Self {
        Self::Failed(message.to_owned())
    }
}

fn serve(project: &str) -> Result<(), Exit> {
    let project = ProjectName::try_from(project).map_err(|e| e.to_string())?;
    let shep_home = shep_home::required(shep_home::FLOCKFILE_FIX)?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let kelpie_home = crate::home::kelpie_home_of(&shep_home);
    let mut paths = ProjectPaths::under(&kelpie_home, &shep_home, &project);
    let (door, why) = crate::lease::door::worker_socket(&shep_home);
    if let Some(why) = why {
        println!("{why}");
    }
    paths.door = door;
    // A socket kelpie cannot bind would otherwise fail a call deep in a work item.
    paths.sockets_fit()?;
    if let Some(old) = crate::home::old_home() {
        crate::home::runner_may_start(&old, &kelpie_home, &project).map_err(Exit::Refused)?;
    }
    let kelpie = std::env::current_exe().map_err(|e| format!("cannot find kelpie itself: {e}"))?;
    // Every agent call runs inside the sandbox runtime, so a runner without one stops here.
    if !paths.tools.sandbox().is_file() {
        return Err(SandboxError::Missing(paths.tools.sandbox())
            .to_string()
            .into());
    }
    let shepherd = shep_channel::serve();
    let sheep = std::env::var("SHEP_NAME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| project.as_str().to_owned());
    let mut look = Look::new(
        shep_home.clone(),
        sheep,
        look::Sources {
            folder: paths.folder.clone(),
            agents: paths.agents.clone(),
        },
        home.clone(),
    );
    let loaded = look.read()?;
    let (settings, kelpie_settings) = (loaded.settings, loaded.kelpie);
    let codex_home = kelpie_settings
        .codex_home(&home, &kelpie_home)
        .map_err(|e| e.to_string())?;
    // A runner reads the gateways once, so each key's variable is unset in every call it makes.
    let gateways = kelpie_settings.gateways();
    crate::spawn::hide(gateways.key_vars());
    let claude = ClaudeCli::labelling(Arc::new(shepherd.clone()))
        .in_runtime(
            paths.tools.clone(),
            home.clone(),
            &codex_home,
            gateways.key_vars(),
        )
        .unfenced_unread(&shep_home);
    let reviewer = LocalReviewer::default().with_gateways(gateways.clone());
    let epoch = Epoch(u64::from(std::process::id()));
    let book = BookFile::new(kelpie_home.join(crate::dog::BOOK));
    let leases = Arc::new(ShepLeases::new(shepherd.clone(), Asker::new(epoch), book));
    let ports = Ports {
        agents: Arc::new(Routed::new(
            Arc::new(claude.clone()),
            Arc::new(claude.pi().with_gateways(gateways)),
            Arc::new(claude.codex(codex_home.clone())),
        )),
        forge: Box::new(Gh),
        meter: Box::new(claude.meter()),
        codex_meter: Box::new(claude.codex_meter(codex_home)),
        reviewer: Arc::new(reviewer.clone()),
        gpu: Arc::new(GpuCurl),
        local_leases: Arc::new(reviewer.clone()),
        review_bots: vec![Arc::new(CodeRabbit), Arc::new(Cubic), Arc::new(Codex)],
        alerts: Arc::new(Curl),
        leases: Arc::clone(&leases) as Arc<dyn Leases>,
        clock: Box::new(SystemClock),
    };
    let runner = Runner::open(
        project,
        settings,
        kelpie_settings,
        &paths,
        &home,
        &kelpie,
        ports,
    )
    .map_err(|e| e.to_string())?;
    for notice in runner.skill_notices() {
        eprintln!("{notice}");
    }

    if !shepherd.is_active() {
        return Err("no shepherd channel: run it under shep with `channel = true`".into());
    }
    let status = serde_json::to_string(&runner.status()).expect("status serializes to JSON");
    println!("up: {status}");

    let stopping = runner.stopping();
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
    let on_wake = Box::new(move |runner: &Mutex<Runner>| look.again(runner));
    let worker = Worker::spawn(
        Arc::clone(&runner),
        on_wake,
        wake.clone(),
        woken,
        stop.clone(),
    );
    shepherd.on_shutdown(move || {
        let _ = stop.send(Stop::Shutdown);
    });
    shepherd.ready().map_err(|e| e.to_string())?;

    // Only a shutdown message or a dead loop thread ends the wait. Each call
    // gets SIGTERM, shep's stop ends what is left once the runner exits, and
    // each work item's turn resumes from its session on restart.
    let why = stopped.recv();
    let let_go = worker.stop(JOIN_BOUND, || stop_calls(&claude, &reviewer));
    // Their ends will not be heard now, and a pass that did not let go
    // still holds the runner's lock, which this needs none of.
    let stopped = stopping.record(crate::ports::Clock::now(&SystemClock));
    if let_go {
        if let Err(e) = crate::runner::settle(&runner) {
            eprintln!("cannot save the work items' time: {e}");
        }
        crate::runner::count_stopped(&runner, &stopped);
    } else {
        // A pass still running holds the runner's lock, and shutdown must not wait for it.
        eprintln!(
            "the runner's loop was still in a pass {}s after the stop, so its time is not saved",
            JOIN_BOUND.as_secs()
        );
    }
    shepherd.flush(FLUSH_TIMEOUT).map_err(|e| e.to_string())?;
    match why {
        Ok(Stop::LoopDied) => Err(
            "the runner's loop or a call's thread panicked; each work item's turn resumes on \
             restart"
                .into(),
        ),
        _ => Ok(()),
    }
}

/// Why the runner stops
enum Stop {
    /// The shepherd sent its shutdown message
    Shutdown,
    /// The thread that runs the runner's loop panicked, or a call's thread
    /// did, whose panic the loop resumes
    LoopDied,
}

/// What the loop's thread does each time it wakes from a wait
type OnWake = Box<dyn FnMut(&Mutex<Runner>) + Send>;

/// The thread that runs the runner's loop, and the means to stop it
struct Worker {
    stopping: Arc<AtomicBool>,
    wake: Sender<()>,
    // Disconnects when the thread ends, however it ends.
    ended: Receiver<()>,
    thread: JoinHandle<()>,
}

impl Worker {
    /// Starts the thread, which tells `died` if it panics
    ///
    /// `on_wake` runs each time the thread wakes from a wait, before its next
    /// pass. Each call the runner starts tells `wake` when it ends.
    fn spawn(
        runner: Arc<Mutex<Runner>>,
        mut on_wake: OnWake,
        wake: Sender<()>,
        woken: Receiver<()>,
        died: Sender<Stop>,
    ) -> Self {
        (runner.lock())
            .unwrap_or_else(PoisonError::into_inner)
            .wake_with(wake.clone());
        let stopping = Arc::new(AtomicBool::new(false));
        let (ending, ended) = mpsc::channel::<()>();
        let flag = Arc::clone(&stopping);
        let thread = std::thread::spawn(move || {
            let _ending = ending;
            // A runner with no worker thread would answer triggers and never work.
            let run = || work(&runner, &mut on_wake, &woken, &flag);
            if catch_unwind(AssertUnwindSafe(run)).is_err() {
                let _ = died.send(Stop::LoopDied);
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
    /// A pass waits on no agent call, so the thread lets go at once, and no
    /// call starts once the ports stop. Returns whether it let go in time.
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

// Runs passes while there is anything to do, then sleeps until a trigger,
// a call's end, the next ceiling of a call in flight, or the next look at
// the board, or at the webhook's topic while a ruling waits on a reply
// there. A turn cut short by a restart is resumed on the first pass.
fn work(runner: &Mutex<Runner>, on_wake: &mut OnWake, woken: &Receiver<()>, stopping: &AtomicBool) {
    while !stopping.load(Ordering::SeqCst) {
        let notes = (runner.lock())
            .unwrap_or_else(PoisonError::into_inner)
            .take_notes();
        for note in notes {
            eprintln!("{note}");
        }
        match advance(runner) {
            Ok(Pass::Report(report)) => {
                let line = serde_json::to_string(&report).expect("a report serializes to JSON");
                println!("{line}");
                if !report.waits() {
                    continue;
                }
            }
            Ok(Pass::Started) => continue,
            Ok(Pass::Idle) => {}
            Err(e) => eprintln!("cannot save what a pass did: {e}"),
        }
        let (awaits_reply, ceiling) = {
            let runner = runner.lock().unwrap_or_else(PoisonError::into_inner);
            (runner.awaits_reply(), runner.next_ceiling())
        };
        let look = if awaits_reply {
            Duration::from_secs(READ_EVERY)
        } else {
            BOARD_POLL
        };
        let wait = ceiling.map_or(look, |ceiling| ceiling.min(look));
        if let Err(RecvTimeoutError::Disconnected) = woken.recv_timeout(wait) {
            return;
        }
        // A stop has only JOIN_BOUND to be let go, so it skips the wake's work.
        if !stopping.load(Ordering::SeqCst) {
            on_wake(runner);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use super::*;
    use crate::ports::ReviewerError;
    use crate::test::{Hold, Rig, Scripted, ScriptedRound};

    // The worker is a real thread on real time, so every wait has this ceiling.
    const PATIENCE: Duration = Duration::from_secs(10);

    // The code is what shep's `stop_exit_codes` matches to leave a runner stopped.
    #[test]
    fn a_refused_start_exits_with_the_code_the_flock_entry_stops_on() {
        let refused = Exit::Refused("old home".into());
        assert_eq!(refused.code(), REFUSED);
        assert_eq!(refused.message(), "old home");
        assert_eq!(Exit::from("no HOME").code(), 1);
    }

    /// A worker whose first turn on issue 7 is held open by `hold`
    fn in_a_turn(rig: &Rig, hold: &Hold) -> (Arc<Mutex<Runner>>, Worker) {
        let runner = Arc::new(rig.open().unwrap());
        rig.ask(&runner, "add", Some("7"));
        rig.claude.script([Scripted::Hold(hold.clone())]);
        let worker = spawn(&runner);
        assert!(hold.entered(PATIENCE), "the worker's turn never began");
        (runner, worker)
    }

    fn spawn(runner: &Arc<Mutex<Runner>>) -> Worker {
        let (wake, woken) = mpsc::channel();
        let (died, _) = mpsc::channel();
        Worker::spawn(Arc::clone(runner), Box::new(|_| {}), wake, woken, died)
    }

    // Each open work item's issue and its turn's state, as `status` shows them
    fn turns(rig: &Rig, runner: &Mutex<Runner>) -> Vec<(u64, String)> {
        let status = rig.ask(runner, "status", None);
        let items = status["work_items"].as_array().unwrap();
        (items.iter())
            .map(|item| {
                let state = item["turn"]["state"].as_str().unwrap().to_owned();
                (item["issue"].as_u64().unwrap(), state)
            })
            .collect()
    }

    fn eventually(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !done() {
            assert!(Instant::now() < deadline, "never saw {what}");
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_wake_reads_the_settings_again_but_a_stop_does_not() {
        let rig = Rig::new("shep");
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        let on_wake: OnWake = Box::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let (wake, woken) = mpsc::channel();
        let (died, _) = mpsc::channel();
        let runner = Arc::new(rig.open().unwrap());
        let worker = Worker::spawn(runner, on_wake, wake.clone(), woken, died);
        wake.send(()).unwrap();
        eventually("the trigger's wake", || wakes.load(Ordering::SeqCst) == 1);

        assert!(worker.stop(PATIENCE, || {}));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "the stop read the settings"
        );
    }

    #[test]
    fn a_stop_with_calls_running_lets_go_without_waiting_for_them() {
        let rig = Rig::new("acme");
        rig.edit_settings(|s| s.replace("active_items = 1", "active_items = 2"));
        let runner = Arc::new(rig.open().unwrap());
        rig.ask(&runner, "add", Some("7"));
        rig.ask(&runner, "add", Some("8"));
        let (seven, eight) = (Hold::default(), Hold::default());
        rig.claude
            .script([Scripted::Hold(seven.clone()), Scripted::Hold(eight.clone())]);
        let worker = spawn(&runner);
        assert!(seven.entered(PATIENCE), "#7's turn never began");
        assert!(eight.entered(PATIENCE), "#8's turn never began beside #7's");
        let running = vec![(7, "running".to_owned()), (8, "running".to_owned())];
        assert_eq!(turns(&rig, &runner), running);

        let at_stop = Arc::new(Mutex::new(None));
        let let_go = worker.stop(JOIN_BOUND, || {
            *at_stop.lock().unwrap() = Some((seven.returned(), eight.returned()));
            rig.claude.stop();
        });

        assert!(let_go, "the worker waited on its calls");
        assert_eq!(
            *at_stop.lock().unwrap(),
            Some((false, false)),
            "the stop waited for a call to end"
        );
        assert!(seven.answered(PATIENCE) && eight.answered(PATIENCE));
        assert_eq!(
            rig.claude.all_calls().len(),
            2,
            "a call began after the stop"
        );
        assert_eq!(turns(&rig, &runner), running, "a turn will not resume");
    }

    #[test]
    fn a_calls_end_wakes_the_loop_to_record_it() {
        let rig = Rig::new("shep");
        let hold = Hold::default();
        let (runner, worker) = in_a_turn(&rig, &hold);
        assert_eq!(turns(&rig, &runner), [(7, "running".to_owned())]);
        // Draining, so nothing starts once the turn's end is recorded.
        assert!(rig.ask(&runner, "drain", None)["draining"].is_object());
        hold.release();
        // Well inside the minute until the next look at the board.
        eventually("the turn's end", || {
            turns(&rig, &runner) == [(7, "ended".to_owned())]
        });
        assert!(worker.stop(JOIN_BOUND, || {}));
    }

    #[test]
    fn a_worker_waiting_for_its_next_look_lets_go_as_soon_as_it_is_asked() {
        let rig = Rig::new("shep");
        let hold = Hold::default();
        let (_runner, worker) = in_a_turn(&rig, &hold);
        // A failed review round is a step the worker waits after. It reads
        // the head the turn pushed.
        crate::test::git(
            &rig.worktree_7(),
            &["push", "--quiet", "origin", "HEAD:kelpie/7"],
        );
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
