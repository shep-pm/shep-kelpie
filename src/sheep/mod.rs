//! A runner as a sheep: the shepherd channel wired to a [`Runner`]
//!
//! The runner's flock entry needs `channel = true`, and
//! `shutdown_with_message = true` so a stop reaches it as a message.
//! Triggers are answered at once. The loop runs on its own thread, woken by a
//! trigger that asks for work, a call in flight ending, and a look at the
//! board every minute, or sooner while a ruling waits on a webhook reply or a
//! failed step waits to rerun. It starts each agent call and never waits on
//! one. A finished runner asks the shepherd to stop its own sheep.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use shep_handover::Arrival;

use crate::adapters::{
    Base, ClaudeCli, CodexCli, Curl, CurlGithub, DogE2e, DoorBoard, DoorPace, Etags,
    GatewayClassifier, Gh, GhApps, GooseCli, GpuCurl, LocalReviewer, LoginToken, McpRules, Openssl,
    PiCli, PlaywrightBridges, PlaywrightPages, SandboxRuntime, ShepLeases, ShepherdSink, StatDisk,
    SystemGit, stop_calls,
};
use crate::coderabbit::CodeRabbit;
use crate::codex::Codex;
use crate::cubic::Cubic;
use crate::flock::control;
use crate::github::{AppTokens, AppVoice, Apps};
use crate::home::{ProjectName, ProjectPaths};
use crate::lease::Epoch;
use crate::lease::saved::BookFile;
use crate::lease::wire::GRANT;
use crate::ports::{Forge, Leases, Ports, Revalidated, Routed, SystemClock, Tickets};
use crate::runner::{ACTIONS, Handles, Pass, Posted, Runner, advance, answer, serve_socket, wakes};
use crate::settings::{Harness, Settings};
use crate::shep_home;
use crate::shepherd::{self, Dial, Socket};
use crate::upgrade::restart::{Interrupt, Patience};

/// How long queued replies get to reach the shepherd before the runner exits
///
/// With [`JOIN_BOUND`], 1.5s, inside shep's default `kill_timeout` of 1.6s.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(500);

// How long a stopping runner waits for its loop's thread, whose pass waits
// only on short git and gh calls. With the flush after it, it fits inside
// shep's default `kill_timeout` of 1.6s, past which shep kills the runner.
const JOIN_BOUND: Duration = Duration::from_secs(1);

mod handover;
mod look;
mod pace;

pub use handover::Carry;
use handover::Handing;
use look::Look;
use pace::{Reread, wait_after_pass};

/// Runs `project`'s runner until the shepherd stops it
///
/// Kelpie's home is `KELPIE_HOME`, or `$SHEP_HOME/kelpie` when that is unset.
pub fn run(project: &str, arrival: Option<Arrival<Carry>>) -> ExitCode {
    let carried = arrival.as_ref().map(|a| a.state.pids());
    match serve(project, arrival) {
        Ok(()) => ExitCode::SUCCESS,
        Err(exit) => {
            // The turns a handover left running would be orphaned, and a restart
            // resumes their sessions beside them.
            if let Some(pids) = carried {
                crate::adapters::end_groups(&pids);
            }
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

// Refuses a start beside a project that runs its checkout or repo, which two would share.
fn unshared(look: &Look, settings: &Settings) -> Result<(), Exit> {
    let Some(taken) = look.taken(settings)? else {
        return Ok(());
    };
    let (project, sheep) = (look.sheep(), &taken.sheep);
    Err(Exit::Refused(format!(
        "{}. Give {project} a checkout and repo of its own with `shep kelpie set \
         git.checkout <path>` (and `git.remote <owner/name>`), or remove {project}'s table \
         if it was copied from {sheep}'s",
        taken.clash()
    )))
}

fn serve(project: &str, arrival: Option<Arrival<Carry>>) -> Result<(), Exit> {
    // The file this build started from, before anything can replace it.
    crate::home::running();
    let project = ProjectName::try_from(project).map_err(|e| e.to_string())?;
    let shep_home = shep_home::required(shep_home::FLOCKFILE_FIX)?;
    let home = crate::home::user_home().ok_or(crate::home::HOME_UNSET)?;
    let kelpie_home = crate::home::kelpie_home_of(&shep_home);
    let mut paths = ProjectPaths::under(&kelpie_home, &shep_home, &project);
    let (door, why) = crate::lease::door::checked_socket(&shep_home);
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
    SandboxRuntime::new().check().map_err(|e| e.to_string())?;
    let sheep = std::env::var("SHEP_NAME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| project.as_str().to_owned());
    let dial: Arc<dyn Dial> = Arc::new(Socket);
    let mut look = Look::new(
        Arc::clone(&dial),
        shep_home.clone(),
        sheep.clone(),
        look::Sources {
            folder: paths.folder.clone(),
            agents: paths.agents.clone(),
        },
        home.clone(),
    );
    let (settings, kelpie_settings) = look.read().map(|loaded| (loaded.settings, loaded.kelpie))?;
    unshared(&look, &settings)?;
    settings.browser.supported().map_err(Exit::Refused)?;
    let codex_home = kelpie_settings
        .codex_home(&home, &kelpie_home)
        .map_err(|e| e.to_string())?;
    // A runner reads the gateways once, so each key's variable is unset in every call it makes.
    // A handover starts the installed build to here before it execs it, so a build
    // that would stop on the settings is refused while the old image still runs.
    if std::env::var_os(handover::CHECK_VAR).is_some() {
        return Ok(());
    }
    let shepherd = shep_channel::serve();
    let gateways = kelpie_settings.gateways();
    crate::spawn::hide(gateways.key_vars());
    // The runner's `env` holds the token `claude` logs in with, for `claude` alone.
    let login = LoginToken::take();
    let base = Base::labelling(Arc::new(shepherd.clone()))
        .in_runtime(home.clone(), &codex_home, gateways.key_vars())
        .unfenced_unread(&home, &shep_home);
    let reviewer = LocalReviewer::default().with_gateways(gateways.clone());
    let epoch = Epoch(u64::from(std::process::id()));
    let book = BookFile::new(kelpie_home.join(crate::dog::BOOK));
    let handed_over = arrival.is_some();
    let asker = handover::asker(arrival.as_ref().map(|a| &a.state), epoch, &sheep, &book);
    let leases = Arc::new(ShepLeases::new(Arc::new(shepherd.clone()), asker, book));
    let app_tokens = Arc::new(AppTokens::new(
        Apps::under(&kelpie_home),
        Box::new(CurlGithub),
        Box::new(Openssl),
        Box::new(SystemClock),
    ));
    let revalidated = Revalidated::default();
    let etags = Etags::new(revalidated.clone());
    let app_reads = GhApps::new(app_tokens.clone(), &kelpie_home).with_etags(etags.clone());
    let flock = shepherd::block_on(async {
        let client = shepherd::connect(&*dial, &shep_home)
            .await
            .map_err(|e| e.describe(&shep_home))?;
        control::repos(&client, &home).await
    })?;
    let gh = Arc::new(Gh::new(etags));
    let ports = Ports {
        agents: Arc::new(Routed::new(vec![
            (
                Harness::ClaudeCode,
                Arc::new(ClaudeCli::on(&base).with_login(login)),
            ),
            (
                Harness::Pi,
                Arc::new(PiCli::on(&base).with_gateways(gateways.clone())),
            ),
            (
                Harness::Codex,
                Arc::new(CodexCli::on(&base, codex_home.clone())),
            ),
            (
                Harness::Goose,
                Arc::new(GooseCli::on(&base).with_gateways(gateways)),
            ),
        ])),
        forge: Arc::clone(&gh) as Arc<dyn Forge>,
        tickets: Arc::clone(&gh) as Arc<dyn Tickets>,
        app_reads: Some(Arc::new(app_reads)),
        revalidated,
        voice: Arc::new(AppVoice::new(app_tokens, Box::new(CurlGithub))),
        meter: Arc::new(base.meter()),
        codex_meter: Arc::new(base.codex_meter(codex_home)),
        reviewer: Arc::new(reviewer.clone()),
        git: Arc::new(SystemGit),
        gpu: Arc::new(GpuCurl),
        disk: Arc::new(StatDisk),
        local_leases: Arc::new(reviewer.clone()),
        review_bots: vec![Arc::new(CodeRabbit), Arc::new(Cubic), Arc::new(Codex)],
        alerts: Arc::new(Curl),
        test_runner: Arc::new(base.test_runner()),
        dev_servers: Arc::new(base.dev_servers()),
        e2e_runner: Arc::new(DogE2e),
        pages: Arc::new(PlaywrightPages),
        bridges: Arc::new(PlaywrightBridges),
        harness_mcp: Arc::new(McpRules),
        leases: Arc::clone(&leases) as Arc<dyn Leases>,
        metrics: Arc::new(ShepherdSink::new(shepherd.clone())),
        classifier: Arc::new(GatewayClassifier),
        clock: Arc::new(SystemClock),
        pace: Arc::new(DoorPace::new(paths.door.clone())),
        agent_board: Arc::new(DoorBoard::new(paths.door.clone())),
        flock,
    };
    let finished: OnFinished = {
        let (dial, shep_home, project) = (Arc::clone(&dial), shep_home.clone(), project.clone());
        Box::new(move |runner| stop_own_sheep(&dial, &shep_home, &project, runner))
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
    match &arrival {
        Some(arrival) => println!("{}", arrival.state.took_over_line(&runner)),
        None => println!("{}", runner.up_line()),
    }
    let stopping = runner.stopping();
    let postings = runner.postings();
    let runner = Arc::new(Mutex::new(runner));
    let (wake, woken) = mpsc::channel();
    // The turns a previous image left running are this one's before its first pass.
    if let Some(arrival) = arrival {
        let mut runner = runner.lock().unwrap_or_else(PoisonError::into_inner);
        runner.wake_with(wake.clone());
        runner.adopt_calls(arrival.state.into_turns());
    }
    let handover = Handing::installed(base.clone());
    let asked = wake.clone();
    let handing = Arc::clone(&handover);
    shepherd.on_action(handover::ACTION, move |_, _| handing.ask(&asked));
    let handing = Arc::clone(&handover);
    shepherd.on_action(crate::shepherd::WITHDRAW, move |_, _| handing.withdraw());
    for action in ACTIONS {
        let runner = Arc::clone(&runner);
        let wake = wake.clone();
        let postings = Arc::clone(&postings);
        shepherd.on_action(action, move |params, name| {
            on_trigger(&runner, &postings, name, params, &wake)
        });
    }
    // A grant lands in the lease adapter, and the step it wakes reads it.
    let granted = wake.clone();
    let handler = Arc::clone(&leases);
    shepherd.on_action(GRANT, move |params, _| {
        let reply = match handler.grant(params.unwrap_or_default()) {
            Ok(kind) => serde_json::json!({ "granted": kind }),
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
        let _ = granted.send(());
        reply.to_string()
    });
    handover::reconcile_grants(handed_over, &leases, &sheep, &wake);
    let (stop, stopped) = mpsc::channel();
    let on_wake = Box::new(move |runner: &Mutex<Runner>| look.again(runner));
    let worker = Worker::spawn(
        Arc::clone(&runner),
        Hooks {
            on_wake,
            finished,
            handover,
        },
        wake.clone(),
        woken,
        stop.clone(),
    );
    shepherd.on_shutdown(move || {
        let _ = stop.send(Stop::Shutdown);
    });
    // The verbs ask here first and fall back to `shep trigger`, so a runner
    // that cannot open its socket says so and goes on.
    let socket = serve_socket(
        &paths.socket,
        Handles {
            runner: Arc::clone(&runner),
            postings: Arc::clone(&postings),
            wake: wake.clone(),
        },
    );
    shepherd.ready().map_err(|e| e.to_string())?;

    // Only a shutdown message or a dead loop thread ends the wait. Each call
    // gets SIGTERM, shep's stop ends what is left once the runner exits, and
    // each work item's turn resumes from its session on restart.
    let why = stopped.recv();
    if let Some(socket) = socket {
        socket.end();
    }
    let let_go = worker.stop(JOIN_BOUND, || stop_calls(&base, &reviewer));
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

// Answers the trigger `action`, and wakes the loop for one that asks for a pass.
//
// A `drain` or `undrain` answered while a pass holds the runner's lock leaves
// its ask for the pass, and is the one thing that wakes the loop for it.
fn on_trigger(
    runner: &Mutex<Runner>,
    postings: &Posted,
    action: &str,
    params: Option<&str>,
    wake: &Sender<()>,
) -> String {
    let reply = answer(runner, postings, action, params);
    if wakes(action) || postings.needs_wake() {
        let _ = wake.send(());
    }
    reply
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

/// What the loop's thread does once the runner finished, which it must not
/// wait on, and which tells the runner if the stop fails
type OnFinished = Box<dyn FnMut(&Arc<Mutex<Runner>>) + Send>;

/// What the loop's thread runs beside its passes
struct Hooks {
    /// Each time it wakes from a wait, and between two passes once `INTERVAL` has passed
    on_wake: OnWake,
    /// Each time the runner's stop is due, once it finished
    finished: OnFinished,
    /// The handover to the installed build, tried at the quiet point
    handover: Arc<Handing>,
}

// Asks the shepherd, on a thread of its own, to stop `project`'s runner as
// `pause` does: the pause sends this runner triggers, which its loop must
// keep answering, and the stop's shutdown ends this process before it
// returns. A stop that fails is the runner's to log, and lets `start` in.
fn stop_own_sheep(
    dial: &Arc<dyn Dial>,
    shep_home: &Path,
    project: &ProjectName,
    runner: &Arc<Mutex<Runner>>,
) {
    let (dial, shep_home, project) = (Arc::clone(dial), shep_home.to_owned(), project.clone());
    let failed = |runner: &Mutex<Runner>, why: &str| {
        (runner.lock())
            .unwrap_or_else(PoisonError::into_inner)
            .stop_failed(why);
    };
    let held = Arc::clone(runner);
    let stop = move || {
        let say = &mut |line: String| eprintln!("{line}");
        let stopped = shepherd::block_on(async {
            let client = (shepherd::connect(&*dial, &shep_home).await)
                .map_err(|e| e.describe(&shep_home))?;
            control::pause(
                &client,
                &project,
                Patience::default(),
                Interrupt::Never,
                say,
            )
            .await
        });
        match stopped {
            Ok(lines) => lines.iter().for_each(|line| eprintln!("{line}")),
            Err(e) => failed(&held, &e),
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("finished".into())
        .spawn(stop)
    {
        failed(
            runner,
            &format!("cannot start the thread that stops it: {e}"),
        );
    }
}

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
    /// The thread runs `hooks` beside its passes. Each call the runner
    /// starts tells `wake` when it ends.
    fn spawn(
        runner: Arc<Mutex<Runner>>,
        mut hooks: Hooks,
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
            let run = || work(&runner, &mut hooks, &woken, &flag);
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

// Runs passes while there is anything to do, then sleeps until a trigger
// that asks for work, a call's end, the next ceiling of a call in flight,
// the next look at the board or retry of a failed step, or the next read
// of the webhook's topic while a ruling waits on a reply there. A turn cut short by a restart is resumed on the first pass.
fn work(
    runner: &Arc<Mutex<Runner>>,
    hooks: &mut Hooks,
    woken: &Receiver<()>,
    stopping: &AtomicBool,
) {
    let mut reread = Reread::at(&runner.lock().unwrap_or_else(PoisonError::into_inner));
    while !stopping.load(Ordering::SeqCst) {
        let (notes, stop) = {
            let mut runner = runner.lock().unwrap_or_else(PoisonError::into_inner);
            (runner.take_notes(), runner.take_stop())
        };
        for note in notes {
            eprintln!("{note}");
        }
        if stop {
            (hooks.finished)(runner);
        }
        let advanced = advance(runner);
        // A pass that returns a report to wait on may have started a call too.
        log_started(runner);
        match advanced {
            Ok(Pass::Report(report)) => {
                let line = (runner.lock())
                    .unwrap_or_else(PoisonError::into_inner)
                    .log_step(&report);
                println!("{line}");
                if !report.waits() {
                    busy_point(runner, hooks, stopping, &mut reread);
                    continue;
                }
            }
            Ok(Pass::Started) => {
                busy_point(runner, hooks, stopping, &mut reread);
                continue;
            }
            Ok(Pass::Idle) => {}
            Err(e) => eprintln!("cannot save what a pass did: {e}"),
        }
        let mut wait = wait_after_pass(&runner.lock().unwrap_or_else(PoisonError::into_inner));
        if hooks.handover.pending() {
            wait = wait.min(handover::HOLD_POLL);
        }
        if let Err(RecvTimeoutError::Disconnected) = woken.recv_timeout(wait) {
            return;
        }
        // A stop has only JOIN_BOUND to be let go, so it skips the wake's work.
        if !stopping.load(Ordering::SeqCst) {
            // The quiet point: no pass is running, so no child of one is.
            hooks.handover.attempt(runner);
            (hooks.on_wake)(runner);
            reread.done(&runner.lock().unwrap_or_else(PoisonError::into_inner));
            log_started(runner);
        }
    }
}

// What a runner that always has work to do does between two passes, which no
// wait comes between: the pending handover is tried, and the settings are
// read again once INTERVAL has passed since they last were.
fn busy_point(
    runner: &Arc<Mutex<Runner>>,
    hooks: &mut Hooks,
    stopping: &AtomicBool,
    reread: &mut Reread,
) {
    if stopping.load(Ordering::SeqCst) {
        return;
    }
    hooks.handover.attempt(runner);
    let due = reread.due(&runner.lock().unwrap_or_else(PoisonError::into_inner));
    if due {
        (hooks.on_wake)(runner);
        reread.done(&runner.lock().unwrap_or_else(PoisonError::into_inner));
        log_started(runner);
    }
}

// Logs the `session-started` step of each call started since the last time,
// as soon as it starts and before any wait, for `shep kelpie watch`.
fn log_started(runner: &Mutex<Runner>) {
    let mut runner = runner.lock().unwrap_or_else(PoisonError::into_inner);
    for report in runner.take_started() {
        println!("{}", runner.log_step(&report));
    }
}

#[cfg(test)]
mod tests;
