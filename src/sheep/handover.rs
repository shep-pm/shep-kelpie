//! A runner replacing its image with the installed build, in place
//!
//! `shep_handover` execs the new build over this process, so the pid, the
//! shepherd channel's descriptor and the children all stay, and shep sees no
//! stop. A `handover` trigger asks. The loop acts at its quiet point, between
//! passes, where no merge, `git` or `gh` child of a pass is running, and holds
//! while a child's stdin is still being written, which the new image could not
//! finish. What crosses is each worker turn in flight, which the new image
//! finishes from the call's record on disk.
//!
//! The channel's descriptor is not named to `shep_handover`: the shepherd
//! passes it without close-on-exec and `SHEP_CHANNEL_FD` stays in the
//! environment, so the new image's `shep_channel::serve` takes it again.
//! Before it execs, the build to be run is started once as a runner that stops
//! after reading its settings, so one that would fail there is refused while
//! this image still runs. Any refusal leaves this image running with every call.

use std::any::Any;
use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use shep_handover::{Handover, State, Verdict};

use crate::adapters::{ClaudeCli, ShepLeases};
use crate::lease::Epoch;
use crate::lease::saved::BookFile;
use crate::lease::wire::{Asker, AskerCarry};
use crate::runner::{CarriedTurn, Runner};
use crate::upgrade::build::Build;

/// The trigger that asks for a handover
pub const ACTION: &str = "handover";

/// The trigger that withdraws a handover still waiting for the quiet point
pub const WITHDRAW: &str = "withdraw_handover";

/// How long a `handover` trigger waits for a refusal before it answers that
/// the request is waiting, inside the shepherd's 3s limit on an action
const ANSWER_WITHIN: Duration = Duration::from_millis(2500);

/// The variable that starts a runner in its start check: it stops once it has
/// read the settings and agent files a runner serves on
pub(super) const CHECK_VAR: &str = "KELPIE_HANDOVER_CHECK";

/// How long the installed build has to pass that check
const CHECK_WITHIN: Duration = Duration::from_secs(20);

/// How often a handover held for a stdin feed looks again
pub(super) const HOLD_POLL: Duration = Duration::from_millis(200);

/// What crosses a handover: the worker turns in flight and the runner's
/// lease totals
// wire format: changing this is a change to the handover's state format
#[derive(Debug, Serialize, Deserialize)]
pub struct Carry {
    turns: Vec<CarriedTurn>,
    // Absent from a build that carried only the turns.
    #[serde(default)]
    leases: Option<AskerCarry>,
    // The kelpie version of the image that handed over; absent from one that
    // did not say.
    #[serde(default)]
    build: Option<String>,
}

/// The lease side of a runner `epoch` of `sheep`, from the `Carry` it took
/// over with if it took one
pub(super) fn asker(carry: Option<&Carry>, epoch: Epoch, sheep: &str, book: &BookFile) -> Asker {
    carry.map_or_else(|| Asker::new(epoch), |c| c.asker(epoch, sheep, book))
}

/// Takes up what the dog's book says a runner that took over holds, once it
/// takes `grant` triggers
///
/// The dog saves a grant before it delivers one and never retries, so one
/// that came after the book was read and before the handler was up is only
/// in the book.
pub(super) fn reconcile_grants(
    handed_over: bool,
    leases: &ShepLeases,
    sheep: &str,
    wake: &Sender<()>,
) {
    if handed_over {
        leases.reconcile(sheep);
        let _ = wake.send(());
    }
}

impl Carry {
    /// The line the out log gets as the image that took this over starts, in
    /// place of the plain `up:` line of a start
    pub(super) fn took_over_line(&self, runner: &Runner) -> String {
        let status = runner.status();
        let from = self
            .build
            .as_ref()
            .map_or_else(|| "an earlier build".to_owned(), |b| format!("kelpie {b}"));
        format!(
            "took over: {}, from {from} to kelpie {}, {} turns carried, {} work items open, \
             {} rulings waiting",
            status.project,
            Build::this().kelpie,
            self.turns.len(),
            status.work_items.len(),
            status.rulings.len()
        )
    }

    /// The lease side of the runner `epoch` of `sheep` in the new image: the
    /// old image's totals, or for a build that carried none, the dog's saved
    /// `book` for this run
    ///
    /// A grant can reach the old image after its totals were taken, so what
    /// the book says the run holds is taken up either way.
    pub(super) fn asker(&self, epoch: Epoch, sheep: &str, book: &BookFile) -> Asker {
        let saved = match book.load() {
            Ok(saved) => saved,
            Err(e) => {
                eprintln!("handover: not reading the dog's book for the lease totals, since {e}");
                None
            }
        };
        match (&self.leases, saved) {
            (Some(carried), saved) => {
                let mut asker = Asker::resume(epoch, carried.clone());
                if let Some(saved) = saved {
                    asker.reconcile(sheep, &saved);
                }
                asker
            }
            (None, Some(saved)) => Asker::from_book(epoch, sheep, &saved),
            (None, None) => Asker::new(epoch),
        }
    }

    pub(super) fn into_turns(self) -> Vec<CarriedTurn> {
        self.turns
    }

    /// The processes the turns run as
    pub(super) fn pids(&self) -> Vec<u32> {
        self.turns.iter().map(CarriedTurn::pid).collect()
    }
}

impl State for Carry {
    const FORMAT: u32 = 5;

    // Format 4 is the same turns without the totals.
    fn reads(format: u32) -> bool {
        matches!(format, 4 | 5)
    }
}

/// Replaces this image with the installed build carrying `carry`, and
/// returns only why it did not
pub(super) type Exec = Box<dyn Fn(&Carry) -> String + Send + Sync>;

/// Stops calls from ending while the handover is made, or `None` when one is
/// just ending
pub(super) type Seal = Box<dyn Fn() -> Option<Box<dyn Any + Send>> + Send + Sync>;

/// The handover a `handover` trigger asked for, until the loop acts on it
pub(super) struct Handing {
    waiting: Mutex<Option<Sender<String>>>,
    exec: Exec,
    seal: Seal,
    feeding: Box<dyn Fn() -> bool + Send + Sync>,
}

impl Handing {
    /// The handover to the installed build, held while a child's stdin is
    /// written and with `claude`'s calls kept from ending while it is made
    pub(super) fn installed(claude: ClaudeCli) -> Arc<Self> {
        Self::installed_with(claude, true)
    }

    /// The same, with a test's image as the target, which cannot be asked to
    /// start as a runner
    #[cfg(test)]
    pub(super) fn installed_unchecked(claude: ClaudeCli) -> Arc<Self> {
        Self::installed_with(claude, false)
    }

    fn installed_with(claude: ClaudeCli, check: bool) -> Arc<Self> {
        let seal: Seal = Box::new(move || {
            claude
                .seal()
                .map(|sealed| Box::new(sealed) as Box<dyn Any + Send>)
        });
        Self::with(
            Box::new(move |carry| exec_installed(carry, check)),
            seal,
            Box::new(crate::adapters::feeding),
        )
    }

    /// One that no test asks for
    #[cfg(test)]
    pub(super) fn never() -> Arc<Self> {
        Self::with(
            Box::new(|_| "no handover here".into()),
            Box::new(|| Some(Box::new(()))),
            Box::new(|| false),
        )
    }

    pub(super) fn with(
        exec: Exec,
        seal: Seal,
        feeding: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            waiting: Mutex::new(None),
            exec,
            seal,
            feeding,
        })
    }

    /// Whether a handover is waiting for the loop's quiet point
    pub(super) fn pending(&self) -> bool {
        self.slot().is_some()
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Sender<String>>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Answers a `handover` trigger: queues the request and wakes the loop,
    /// then reports a refusal that comes soon, or that the request waits
    ///
    /// A handover that succeeds is never answered, since the process that
    /// would answer is gone.
    pub(super) fn ask(&self, wake: &Sender<()>) -> String {
        let (tell, told) = mpsc::channel();
        {
            let mut slot = self.slot();
            if slot.is_some() {
                return error("a handover is already waiting for the runner's quiet point");
            }
            *slot = Some(tell);
        }
        let _ = wake.send(());
        match told.recv_timeout(ANSWER_WITHIN) {
            Ok(why) => error(&format!("handover refused: {why}")),
            Err(RecvTimeoutError::Timeout) => {
                serde_json::json!({ "handover": "waiting for the runner's quiet point" })
                    .to_string()
            }
            Err(RecvTimeoutError::Disconnected) => error("the handover was dropped"),
        }
    }

    /// Answers a `withdraw_handover` trigger: drops the request if the loop
    /// has not taken it yet
    pub(super) fn withdraw(&self) -> String {
        let dropped = self.slot().take().is_some();
        let state = if dropped { "withdrawn" } else { "none waiting" };
        serde_json::json!({ "handover": state }).to_string()
    }

    /// Hands over if one is asked for and nothing holds it, at the loop's
    /// quiet point
    ///
    /// Returns only when it did not happen. A refusal is told to the
    /// trigger, if it still waits, and to the log.
    pub(super) fn attempt(&self, runner: &Arc<Mutex<Runner>>) {
        if !self.pending() || (self.feeding)() {
            return;
        }
        // The request stays queued until the handover is made or refused, so
        // `pending`, `withdraw` and a second `ask` never see it gone while it
        // only waits.
        if let Err(e) = crate::runner::settle(runner) {
            return self.refuse(format!("cannot save the work items' time: {e}"));
        }
        let Some(_sealed) = (self.seal)() else {
            return self.refuse("a call has just ended and is being read".into());
        };
        // Held through the exec, so nothing changes what was carried.
        let held = runner.lock().unwrap_or_else(PoisonError::into_inner);
        // A notice hook runs on a thread the new image would not have, and
        // the notices queued behind it would be dropped.
        if held.notices_running() {
            return;
        }
        // Taken before the exec, so one withdrawn by now is not made, and
        // none is seen waiting while the exec runs.
        let Some(tell) = self.slot().take() else {
            return;
        };
        let why = match held.carry_calls() {
            Ok(turns) => {
                let leases = held.carry_leases();
                let build = Some(Build::this().kelpie);
                (self.exec)(&Carry {
                    turns,
                    leases,
                    build,
                })
            }
            Err(why) => why,
        };
        refused(&tell, why);
    }

    // Refuses the request still queued, if one is.
    fn refuse(&self, why: String) {
        if let Some(tell) = self.slot().take() {
            refused(&tell, why);
        }
    }
}

// Tells a refusal to the log and to the trigger that asked, if it still waits.
fn refused(tell: &Sender<String>, why: String) {
    eprintln!("handover refused: {why}");
    let _ = tell.send(why);
}

fn error(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

// Execs the installed build, and says why it did not.
fn exec_installed(carry: &Carry, check: bool) -> String {
    // Asked once the target is resolved and before anything is written. That
    // the target reads `Carry` is the probe's to check; that it can start as
    // this runner is the check's, since a build that fails after the exec
    // would leave the runner stopped with its calls running.
    let gate = |target: &Path| {
        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        fitness(target, &args, check, &crate::adapters::feeding)
    };
    match Handover::new(carry).exec(&gate) {
        Err(e) => e.to_string(),
        Ok(never) => match never {},
    }
}

// Whether `target` may be exec'd. The feed is asked last, since the start
// check takes seconds in which a child's stdin or a relayed reply may begin.
fn fitness(target: &Path, args: &[OsString], check: bool, feeding: &dyn Fn() -> bool) -> Verdict {
    if check && let Err(why) = starts(target, args, CHECK_WITHIN) {
        return Verdict::Refused(why);
    }
    match feeding() {
        true => Verdict::Refused("a child's stdin began to be written".into()),
        false => Verdict::Carryable,
    }
}

// Runs `target` with `args` as a runner that stops once it has read its
// settings, and says why it could not.
fn starts(target: &Path, args: &[OsString], within: Duration) -> Result<(), String> {
    let mut child = Command::new(target)
        .args(args)
        .env(CHECK_VAR, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("the installed build could not be started: {e}"))?;
    let mut stderr = child.stderr.take();
    let (said, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(stderr) = stderr.as_mut() {
            let _ = stderr.by_ref().take(1 << 16).read_to_end(&mut bytes);
        }
        let _ = said.send(bytes);
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= within => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the installed build did not finish its start check in time".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("the installed build's start check failed: {e}")),
        }
    };
    if status.success() {
        return Ok(());
    }
    let said = heard
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_default();
    let said = String::from_utf8_lossy(&said);
    let last = said.lines().last().unwrap_or("it said nothing");
    Err(format!(
        "the installed build would not start as this runner: {last}"
    ))
}

#[cfg(test)]
mod tests;
