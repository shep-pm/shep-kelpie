//! `kelpie lease`: the maintainer's lease commands
//!
//! `run` waits for a lease, runs a command and returns the lease when the
//! command exits, however it exits. `take` and `return` hold one for
//! longer. The GPU lease is the qwen scripts' lock, taken here directly;
//! a `take` leaves a holder process behind, since a lock whose pid is
//! gone gets cleared. Every other kind is the dog's, reached through shep.

use std::io::{BufRead, BufReader, Write};
use std::process::{ExitCode, ExitStatus, Stdio};
use std::time::Duration;

use serde_json::Value;
use shep_client::Client;
use shep_client::shep_core::protocol::Request;
use shep_client::shep_core::protocol::request::{ActionOutcome, Response, SelectorSpec};
use tokio::signal::unix::{Signal, SignalKind, signal};

use super::gpu::{self, Claim, GpuLock, Waiting};
use super::{GPU, LeaseKind};
use crate::dog;

/// The usage lines for `kelpie lease`
pub const USAGE: &str = "\
       kelpie lease run <kind> -- <command> [args...]
       kelpie lease take <kind>
       kelpie lease return <kind>
       kelpie lease status
       <kind> is gpu, the qwen scripts' lock, or a lease the dog holds";

/// What a `take` holder writes as the lock's `what`, and `return` looks for
const TAKE_WHAT: &str =
    "kelpie lease take, held for the maintainer until `kelpie lease return gpu`";

/// How often a waiting maintainer asks the dog again; a local round trip
/// through shep took about 30 ms in the transport series
const DOG_POLL: Duration = Duration::from_secs(1);

/// Runs `kelpie lease <args>`
pub fn main(args: &[String]) -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let outcome = match runtime {
        Ok(runtime) => runtime.block_on(dispatch(args)),
        Err(e) => Err(format!("cannot start the async runtime: {e}")),
    };
    match outcome {
        Ok(code) => code,
        Err(message) => {
            eprintln!("kelpie lease: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(args: &[String]) -> Result<ExitCode, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["run", GPU, "--", command @ ..] if !command.is_empty() => run_gpu(command).await,
        ["run", kind, "--", command @ ..] if !command.is_empty() => {
            run_book(&kind_of(kind)?, command).await
        }
        ["take", GPU] => take_gpu().map(|()| ExitCode::SUCCESS),
        ["take", kind] => take_book(&kind_of(kind)?).await,
        ["return", GPU] => return_gpu().map(|()| ExitCode::SUCCESS),
        ["return", kind] => return_book(&kind_of(kind)?).await,
        ["status"] => status().await,
        // take reads a holder's stdout alone, so its errors go there.
        ["hold", GPU] => Ok(hold_gpu().await.unwrap_or_else(|e| {
            println!("kelpie lease: {e}");
            ExitCode::FAILURE
        })),
        _ => {
            eprintln!("usage: {}", USAGE.trim_start());
            Ok(ExitCode::from(2))
        }
    }
}

fn kind_of(kind: &str) -> Result<LeaseKind, String> {
    LeaseKind::try_from(kind).map_err(|e| e.to_string())
}

fn gpu_lock() -> GpuLock {
    GpuLock::under(&gpu::temp_dir())
}

fn report(waiting: Waiting<'_>) {
    eprintln!("{}", said(waiting));
}

// A holder reports to take, which reads its stdout.
fn tell_take(waiting: Waiting<'_>) {
    println!("{}", said(waiting));
}

fn said(waiting: Waiting<'_>) -> String {
    let pid = |pid: Option<u32>| pid.map_or_else(|| "unknown".into(), |p| p.to_string());
    match waiting {
        Waiting::Held { waited, holder } => format!(
            "kelpie lease: waiting {waited}s for the GPU, now held by pid {} running {}",
            pid(holder.pid),
            holder.what
        ),
        Waiting::Cleared(dead) => {
            format!(
                "kelpie lease: clearing a stale lock, pid {} is gone",
                pid(dead)
            )
        }
    }
}

async fn run_gpu(command: &[&str]) -> Result<ExitCode, String> {
    let lock = gpu_lock();
    let mut signals = Signals::new()?;
    let pid = std::process::id();
    let claim = Claim {
        pid,
        what: format!("kelpie lease run: {}", command.join(" ")),
    };
    tokio::select! {
        taken = lock.take(&claim, gpu::scripts_naps, report) => {
            taken.map_err(|e| format!("cannot take {}: {e}", lock.path().display()))?;
        }
        caught = signals.recv() => return Ok(caught.exit_code()),
    }
    let ran = run_command(command, &mut signals).await;
    let released = lock
        .release(pid)
        .map_err(|e| format!("cannot remove {}: {e}", lock.path().display()));
    Ok(exit_code(both(ran, released)?))
}

async fn run_book(kind: &LeaseKind, command: &[&str]) -> Result<ExitCode, String> {
    let mut signals = Signals::new()?;
    if let Some(caught) = wait_for_grant(kind, &mut signals).await? {
        return Ok(caught.exit_code());
    }
    let ran = run_command(command, &mut signals).await;
    let returned = ask_dog("return", kind.as_str())
        .await
        .map_err(|e| format!("{e}: run `kelpie lease return {kind}`"));
    Ok(exit_code(both(ran, returned)?))
}

// The command's outcome and its lease's return, with both errors if both failed.
fn both<T>(ran: Result<ExitStatus, String>, back: Result<T, String>) -> Result<ExitStatus, String> {
    match (ran, back) {
        (Ok(status), Ok(_)) => Ok(status),
        (Err(e), Ok(_)) | (Ok(_), Err(e)) => Err(e),
        (Err(ran), Err(back)) => Err(format!("{ran}, and {back}")),
    }
}

async fn take_book(kind: &LeaseKind) -> Result<ExitCode, String> {
    let mut signals = Signals::new()?;
    if let Some(caught) = wait_for_grant(kind, &mut signals).await? {
        return Ok(caught.exit_code());
    }
    println!("kelpie lease: {kind} is yours until `kelpie lease return {kind}`");
    Ok(ExitCode::SUCCESS)
}

async fn return_book(kind: &LeaseKind) -> Result<ExitCode, String> {
    let reply = ask_dog("return", kind.as_str()).await?;
    if reply["returned"] == true {
        println!("kelpie lease: returned {kind}");
    } else {
        println!("kelpie lease: you did not hold {kind}, and any wait for it is withdrawn");
    }
    Ok(ExitCode::SUCCESS)
}

// Asks the dog until it grants, or withdraws the ask on a signal and
// returns it.
async fn wait_for_grant(kind: &LeaseKind, signals: &mut Signals) -> Result<Option<Caught>, String> {
    let mut last_ahead = None;
    loop {
        let reply = ask_dog("take", kind.as_str()).await?;
        if reply["granted"] == true {
            return Ok(None);
        }
        let ahead = reply["queued"].as_u64();
        let ahead = ahead.ok_or_else(|| format!("the dog answered {reply}"))?;
        if last_ahead != Some(ahead) {
            eprintln!("kelpie lease: waiting for {kind}, {ahead} ahead");
            last_ahead = Some(ahead);
        }
        tokio::select! {
            () = tokio::time::sleep(DOG_POLL) => {}
            caught = signals.recv() => {
                if let Err(e) = ask_dog("return", kind.as_str()).await {
                    eprintln!("kelpie lease: {e}: run `kelpie lease return {kind}`");
                }
                return Ok(Some(caught));
            }
        }
    }
}

fn take_gpu() -> Result<(), String> {
    let lock = gpu_lock();
    if let Some(holder) = lock.holder().filter(|h| h.live && h.what == TAKE_WHAT) {
        let pid = holder.pid.unwrap_or_default();
        println!("kelpie lease: the GPU lock is already held for you by pid {pid}");
        return Ok(());
    }
    // The holder outlives take, so it keeps none of take's own output
    // open: a caller reading take's output to its end would wait forever.
    let exe = std::env::current_exe().map_err(|e| format!("cannot find kelpie itself: {e}"))?;
    let mut holder = std::process::Command::new(exe)
        .args(["lease", "hold", GPU])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start a holder: {e}"))?;
    let pid = holder.id();
    let stdout = holder.stdout.take().ok_or("the holder has no stdout")?;
    let mut held = false;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| format!("cannot read the holder: {e}"))?;
        if line == HELD {
            held = true;
            break;
        }
        eprintln!("{line}");
    }
    if !held {
        let status = holder.wait().map_err(|e| e.to_string())?;
        return Err(format!("the holder gave up ({status})"));
    }
    println!(
        "kelpie lease: pid {pid} holds the GPU lock at {} until `kelpie lease return gpu`",
        lock.path().display()
    );
    Ok(())
}

/// The line a holder prints once the lock is its own
const HELD: &str = "held";

// The holder behind `take gpu`. Everything it says goes to take over its
// stdout, ending with HELD. It then lives until `return gpu` ends it,
// ignoring the hangup a closed terminal sends.
async fn hold_gpu() -> Result<ExitCode, String> {
    let _hangups = signal(SignalKind::hangup()).map_err(|e| e.to_string())?;
    let lock = gpu_lock();
    let claim = Claim {
        pid: std::process::id(),
        what: TAKE_WHAT.into(),
    };
    lock.take(&claim, gpu::scripts_naps, tell_take)
        .await
        .map_err(|e| format!("cannot take {}: {e}", lock.path().display()))?;
    // A take that is gone cannot hear HELD, and nobody would return the lock.
    let mut stdout = std::io::stdout();
    if let Err(e) = writeln!(stdout, "{HELD}").and_then(|()| stdout.flush()) {
        let _ = lock.release(claim.pid);
        return Err(format!("take went away, so the lock is let go: {e}"));
    }
    std::future::pending().await
}

fn return_gpu() -> Result<(), String> {
    let lock = gpu_lock();
    let Some(holder) = lock.holder() else {
        return Err("the GPU lock is free: there is nothing to return".into());
    };
    let pid = holder
        .pid
        .filter(|_| holder.what == TAKE_WHAT)
        .ok_or_else(|| {
            format!(
                "the GPU lock is held by pid {} running {}, not by `kelpie lease take gpu`",
                holder
                    .pid
                    .map_or_else(|| "unknown".into(), |p| p.to_string()),
                holder.what
            )
        })?;
    // The lock goes first, so the holder's end cannot leave a stale one.
    lock.release(pid)
        .map_err(|e| format!("cannot remove {}: {e}", lock.path().display()))?;
    signal_process(pid, "TERM");
    println!("kelpie lease: returned the GPU lock");
    Ok(())
}

async fn status() -> Result<ExitCode, String> {
    match ask_dog("status", "").await {
        Ok(status) => {
            println!("{status}");
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            // The GPU needs no dog: its lock says who holds it.
            let lock = gpu_lock();
            let holder = lock.holder();
            let gpu = serde_json::json!({
                "kind": GPU,
                "lock": lock.path(),
                "since": holder.as_ref().and_then(|h| h.since),
                "holder": holder,
            });
            println!("{}", serde_json::json!({ "leases": [gpu] }));
            eprintln!("kelpie lease: the dog did not answer, so this is the GPU alone: {e}");
            Ok(ExitCode::FAILURE)
        }
    }
}

// Sends a trigger to the dog and returns its JSON reply, or its error.
async fn ask_dog(action: &str, params: &str) -> Result<Value, String> {
    let socket = dog::shepherd_socket()?;
    let client = Client::connect(&socket)
        .await
        .map_err(|e| format!("cannot reach the shepherd at {}: {e}", socket.display()))?;
    let reply = client
        .request(Request::Trigger {
            selector: SelectorSpec::Name(dog::NAME.into()),
            action: action.into(),
            params: Some(params.to_owned()).filter(|p| !p.is_empty()),
        })
        .await
        .map_err(|e| format!("cannot ask the dog: {e}"))?;
    let Response::Triggered(rows) = reply else {
        return Err(format!("the shepherd answered {reply:?}"));
    };
    let body = match rows.into_iter().next().map(|row| row.outcome) {
        Some(ActionOutcome::Replied { body }) => body,
        Some(other) => return Err(format!("the dog did not answer: {other:?}")),
        None => return Err(format!("no sheep named {} is running", dog::NAME)),
    };
    let value: Value =
        serde_json::from_str(&body).map_err(|_| format!("the dog answered {body:?}"))?;
    match value.get("error") {
        Some(Value::String(error)) => Err(format!("the dog refused: {error}")),
        Some(error) => Err(format!("the dog refused: {error}")),
        None => Ok(value),
    }
}

// Runs `command` to its end. A hangup or terminate is passed on to it; an
// interrupt from the terminal has already reached it.
async fn run_command(command: &[&str], signals: &mut Signals) -> Result<ExitStatus, String> {
    let (program, args) = command.split_first().ok_or("no command to run")?;
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    loop {
        tokio::select! {
            status = child.wait() => return status.map_err(|e| e.to_string()),
            caught = signals.recv() => {
                if let (Some(pid), Some(name)) = (child.id(), caught.forward()) {
                    signal_process(pid, name);
                }
            }
        }
    }
}

fn signal_process(pid: u32, name: &str) {
    let _ = std::process::Command::new("kill")
        .args([format!("-{name}"), pid.to_string()])
        .stderr(Stdio::null())
        .status();
}

fn exit_code(status: ExitStatus) -> ExitCode {
    use std::os::unix::process::ExitStatusExt;
    let code = status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// A signal kelpie caught while holding or waiting for a lease
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Caught {
    Interrupt,
    Terminate,
    Hangup,
}

impl Caught {
    // The shell's convention: 128 plus the signal's number.
    fn exit_code(self) -> ExitCode {
        ExitCode::from(match self {
            Self::Hangup => 129,
            Self::Interrupt => 130,
            Self::Terminate => 143,
        })
    }

    fn forward(self) -> Option<&'static str> {
        match self {
            Self::Interrupt => None,
            Self::Terminate => Some("TERM"),
            Self::Hangup => Some("HUP"),
        }
    }
}

// Listening replaces each signal's default of ending kelpie at once, so
// a lease is returned however the command ends.
struct Signals {
    interrupt: Signal,
    terminate: Signal,
    hangup: Signal,
}

impl Signals {
    fn new() -> Result<Self, String> {
        let listen = |kind| signal(kind).map_err(|e| format!("cannot listen for signals: {e}"));
        Ok(Self {
            interrupt: listen(SignalKind::interrupt())?,
            terminate: listen(SignalKind::terminate())?,
            hangup: listen(SignalKind::hangup())?,
        })
    }

    async fn recv(&mut self) -> Caught {
        tokio::select! {
            _ = self.interrupt.recv() => Caught::Interrupt,
            _ = self.terminate.recv() => Caught::Terminate,
            _ = self.hangup.recv() => Caught::Hangup,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;

    fn exited(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code << 8)
    }

    #[test]
    fn both_keeps_every_error() {
        let ok = || Ok::<_, String>(exited(0));
        assert_eq!(both(ok(), Ok::<(), String>(())), Ok(exited(0)));
        assert_eq!(
            both(Err("ran".into()), Ok::<(), String>(())),
            Err("ran".into())
        );
        assert_eq!(both(ok(), Err::<(), _>("back".into())), Err("back".into()));
        assert_eq!(
            both(Err("ran".into()), Err::<(), _>("back".into())),
            Err("ran, and back".into())
        );
    }

    #[test]
    fn exit_codes_follow_the_shell() {
        assert_eq!(exit_code(exited(3)), ExitCode::from(3));
        assert_eq!(exit_code(ExitStatus::from_raw(15)), ExitCode::from(143));
        assert_eq!(Caught::Interrupt.exit_code(), ExitCode::from(130));
        assert_eq!(Caught::Hangup.exit_code(), ExitCode::from(129));
        assert_eq!(
            Caught::Interrupt.forward(),
            None,
            "the terminal already sent it"
        );
        assert_eq!(Caught::Terminate.forward(), Some("TERM"));
    }
}
