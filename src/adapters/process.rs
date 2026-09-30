//! Child processes the runner can stop on its way out
//!
//! A runner that exits on shep's shutdown message never reaches shep's
//! stop ladder, so a child it leaves running is orphaned. Each child here
//! is kept where [`Processes::stop`] can reach it. Only this module reaps
//! them, and only under the lock, so a signalled pid is never a reused one.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// How often a running child is checked for its exit
const POLL: Duration = Duration::from_millis(50);

// Time for a child to exit on SIGTERM before it gets SIGKILL. A stop gives
// the runner shep's `kill_timeout` after its shutdown message, which its
// Flockfile entry must set to 10s or more, and the runner flushes for 2s after.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// Why a stoppable child did not run to its end
#[derive(Debug)]
pub(super) enum RunError {
    /// It could not be started or waited on
    Io(io::Error),
    /// [`Processes::stop`] ended it, or came first
    Stopped,
    /// It ran past its limit and was killed
    TimedOut,
}

/// Running children, and whether the runner is stopping
#[derive(Debug, Clone, Default)]
pub(super) struct Processes(Arc<Mutex<Running>>);

#[derive(Debug, Default)]
struct Running {
    stopping: bool,
    next: u64,
    children: Vec<(u64, Child)>,
}

impl Processes {
    /// Runs `command` to its end with stdin closed, collecting its output
    pub(super) fn output(&self, command: &mut Command) -> Result<Output, RunError> {
        self.run(command, None, &|_| {})
    }

    /// Like [`Self::output`], killing the child after `limit` if one is
    /// given, and telling `spawned` its pid as soon as it runs
    pub(super) fn output_telling(
        &self,
        command: &mut Command,
        limit: Option<Duration>,
        spawned: &dyn Fn(u32),
    ) -> Result<Output, RunError> {
        self.run(command, limit.map(|l| Instant::now() + l), spawned)
    }

    /// Like [`Self::output`], and kills the child once `limit` has passed
    pub(super) fn output_within(
        &self,
        command: &mut Command,
        limit: Duration,
    ) -> Result<Output, RunError> {
        self.run(command, Some(Instant::now() + limit), &|_| {})
    }

    fn run(
        &self,
        command: &mut Command,
        deadline: Option<Instant>,
        spawned: &dyn Fn(u32),
    ) -> Result<Output, RunError> {
        // Its own process group, led by its own pid, so a program it spawns
        // and leaves behind (a build, a test run) is reachable by signalling
        // the group, not just the one pid this struct tracks.
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(RunError::Io)?;
        let pid = child.id();
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let id = {
            let mut running = self.lock();
            if running.stopping {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Stopped);
            }
            running.next += 1;
            let id = running.next;
            running.children.push((id, child));
            id
        };
        spawned(pid);
        let status = self.wait(id, deadline)?;
        // A stopped child's own children may hold its pipes open, so its
        // output is left unread.
        if self.lock().stopping {
            return Err(RunError::Stopped);
        }
        Ok(Output {
            status,
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
    }

    /// Starts `command` in its own process group, stdin closed, and leaves
    /// it running, its output going where the caller pointed it
    ///
    /// [`Self::end`] stops it, and so does [`Self::stop`].
    pub(super) fn start(&self, command: &mut Command) -> Result<u64, RunError> {
        let mut child = command
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(RunError::Io)?;
        let mut running = self.lock();
        if running.stopping {
            stop_child(&mut child, Duration::ZERO);
            return Err(RunError::Stopped);
        }
        running.next += 1;
        let id = running.next;
        running.children.push((id, child));
        Ok(id)
    }

    /// The process id of child `id` from [`Self::start`], also its group's id
    pub(super) fn pid(&self, id: u64) -> Option<u32> {
        let running = self.lock();
        running
            .children
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, c)| c.id())
    }

    /// Whether child `id` from [`Self::start`] is still running
    pub(super) fn alive(&self, id: u64) -> bool {
        let mut running = self.lock();
        let Some((_, child)) = running.children.iter_mut().find(|(i, _)| *i == id) else {
            return false;
        };
        matches!(child.try_wait(), Ok(None))
    }

    /// Stops child `id` from [`Self::start`], and whatever it spawned
    pub(super) fn end(&self, id: u64) {
        let child = {
            let mut running = self.lock();
            let at = running.children.iter().position(|(i, _)| *i == id);
            at.map(|at| running.children.remove(at).1)
        };
        if let Some(mut child) = child {
            stop_child(&mut child, STOP_GRACE);
        }
    }

    /// Whether [`Self::stop`] has been called
    pub(super) fn stopping(&self) -> bool {
        self.lock().stopping
    }

    /// Ends every running child and refuses new ones
    ///
    /// Each gets SIGTERM, so it can end its own children, and SIGKILL if it
    /// is still running after a grace period.
    pub(super) fn stop(&self) {
        {
            let mut running = self.lock();
            running.stopping = true;
            for (_, child) in &running.children {
                signal_group(child.id(), "TERM");
            }
        }
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline && !self.lock().children.is_empty() {
            thread::sleep(POLL);
        }
        for (_, child) in &mut self.lock().children {
            signal_group(child.id(), "KILL");
            let _ = child.kill();
        }
    }

    fn wait(&self, id: u64, deadline: Option<Instant>) -> Result<ExitStatus, RunError> {
        loop {
            let past_deadline = {
                let mut running = self.lock();
                let at = running
                    .children
                    .iter()
                    .position(|(i, _)| *i == id)
                    .expect("only wait removes a child");
                // Checked before the deadline below, so a child that has
                // already exited by the time a poll lands is never reported
                // as timed out, however close the two were.
                if let Some(status) = running.children[at].1.try_wait().map_err(RunError::Io)? {
                    running.children.remove(at);
                    return Ok(status);
                }
                deadline
                    .is_some_and(|d| Instant::now() >= d)
                    .then(|| running.children.remove(at).1)
            };
            if let Some(mut child) = past_deadline {
                // The same stop ladder `stop` uses, so a build or test the
                // worker started and left running past the ceiling is ended
                // too, not just the `claude` process this struct tracked.
                stop_child(&mut child, STOP_GRACE);
                return Err(RunError::TimedOut);
            }
            thread::sleep(POLL);
        }
    }

    fn lock(&self) -> MutexGuard<'_, Running> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// Sends `signal` ("TERM" or "KILL") to the process group `pid` leads, which
// `process_group(0)` at spawn made it the leader of.
//
// Not `kill -<signal> -<pid>`: measured on procps-ng 4.0.2 (Debian
// bookworm), that parses without error and signals nothing, because a
// second `-N`-shaped argument after a signal spec is read as another signal
// spec, leaving no pid to send to at all. `pkill -<signal> -g <pgid>` names
// the process group through its own flag instead of through a negative pid,
// and was measured to reach the group leader and a process it had spawned,
// on both procps-ng 4.0.2 and macOS's BSD pkill.
fn signal_group(pgid: u32, signal: &str) {
    let _ = Command::new("pkill")
        .args([format!("-{signal}"), "-g".to_owned(), pgid.to_string()])
        .stdin(Stdio::null())
        .status();
}

/// Stops process group `pgid`, which no `Processes` holds: SIGTERM, then
/// SIGKILL once [`STOP_GRACE`] has passed with any of it still running
pub(super) fn stop_group(pgid: u32) {
    let alive = || {
        Command::new("pgrep")
            .args(["-g", &pgid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    signal_group(pgid, "TERM");
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline && alive() {
        thread::sleep(POLL);
    }
    signal_group(pgid, "KILL");
}

// SIGTERM to `child`'s whole process group, then SIGKILL once `grace` has
// passed with it still running. `child.kill()` also runs as a fallback for a
// system with no `kill` binary on `PATH`, though that alone would miss
// anything the child had spawned.
fn stop_child(child: &mut Child, grace: Duration) {
    let pid = child.id();
    signal_group(pid, "TERM");
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(POLL);
    }
    signal_group(pid, "KILL");
    let _ = child.kill();
    let _ = child.wait();
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn a_child_runs_to_its_end_with_its_output() {
        let processes = Processes::default();
        let output = processes
            .output(Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]))
            .unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"err\n");
    }

    // Real time: a real process that would outlive the test's own bound
    #[test]
    fn a_child_past_its_limit_is_killed() {
        let processes = Processes::default();
        let started = Instant::now();
        let result = processes.output_within(
            Command::new("sh").args(["-c", "sleep 30"]),
            Duration::from_millis(200),
        );
        assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(processes.lock().children.is_empty());
    }

    // Real time: models a worker whose build or test process outlives it.
    // The child and its grandchild both ignore SIGTERM, so only the group
    // SIGKILL after the grace period ends either of them.
    #[test]
    fn a_child_past_its_limit_takes_its_ignoring_grandchild_down_too() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild-started");
        let pid_file = dir.path().join("grandchild.pid");
        let processes = Processes::default();
        let script = format!(
            "trap '' TERM
             sh -c 'trap \"\" TERM; touch \"$1\"; sleep 30' _ {marker} &
             echo $! > {pid_file}
             wait",
            marker = shell_quote(&marker),
            pid_file = shell_quote(&pid_file),
        );
        let started = Instant::now();
        let result = processes.output_within(
            Command::new("sh").args(["-c", &script]),
            Duration::from_millis(200),
        );
        assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert!(marker.exists(), "the grandchild never started");

        let grandchild: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut alive = true;
        while Instant::now() < deadline {
            alive = process_is_alive(grandchild);
            if !alive {
                break;
            }
            thread::sleep(POLL);
        }
        assert!(!alive, "the grandchild survived the ceiling");
    }

    fn shell_quote(path: &std::path::Path) -> String {
        format!("'{}'", path.display())
    }

    // `kill -0` succeeds on a zombie: SIGKILL ended it, but nothing has
    // reaped it yet. `ps`'s state column reports `Z` for exactly that case,
    // and nothing at all once it is gone.
    fn process_is_alive(pid: u32) -> bool {
        let output = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&output.stdout);
        let stat = stat.trim();
        !stat.is_empty() && !stat.starts_with('Z')
    }

    // Real time: the child is a real process, and the test bounds its own
    // wait with recv_timeout.
    #[test]
    fn stop_ends_a_running_child_and_refuses_the_next() {
        let processes = Processes::default();
        let (done, finished) = mpsc::channel();
        let running = processes.clone();
        let started = Instant::now();
        thread::spawn(move || {
            let result = running.output(Command::new("sh").args(["-c", "sleep 30"]));
            let _ = done.send(result);
        });
        while processes.lock().children.is_empty() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no child started"
            );
            thread::sleep(POLL);
        }
        processes.stop();
        let result = finished
            .recv_timeout(Duration::from_secs(10))
            .expect("the stopped call never returned");
        assert!(matches!(result, Err(RunError::Stopped)), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(10));

        let next = processes.output(Command::new("sh").args(["-c", "exit 0"]));
        assert!(matches!(next, Err(RunError::Stopped)), "{next:?}");
    }

    #[test]
    fn a_child_that_ignores_sigterm_gets_sigkill() {
        let dir = tempfile::tempdir().unwrap();
        let trapped = dir.path().join("trapped");
        let processes = Processes::default();
        let (done, finished) = mpsc::channel();
        let running = processes.clone();
        let script = format!("trap '' TERM; touch '{}'; sleep 30", trapped.display());
        thread::spawn(move || {
            let _ = done.send(running.output(Command::new("sh").args(["-c", &script])));
        });
        let started = Instant::now();
        while !trapped.exists() {
            assert!(started.elapsed() < Duration::from_secs(10), "no trap set");
            thread::sleep(POLL);
        }
        processes.stop();
        let result = finished
            .recv_timeout(STOP_GRACE + Duration::from_secs(10))
            .expect("the stopped call never returned");
        assert!(matches!(result, Err(RunError::Stopped)), "{result:?}");
    }
}
