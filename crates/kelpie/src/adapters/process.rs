//! Child processes the runner can stop on its way out
//!
//! A runner that exits on shep's shutdown message never reaches shep's
//! stop ladder, so a child it leaves running is orphaned. Each child here
//! is kept where [`Processes::stop`] can reach it. Only this module reaps
//! them, and only under the lock, so a signalled pid is never a reused one.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// How often a running child is checked for its exit
const POLL: Duration = Duration::from_millis(50);

// Time for a child to exit on SIGTERM before it gets SIGKILL. Shep gives a
// runner 8s after its shutdown message, and the runner flushes for 2s after.
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
        self.run(command, None)
    }

    /// Like [`Self::output`], and kills the child once `limit` has passed
    pub(super) fn output_within(
        &self,
        command: &mut Command,
        limit: Duration,
    ) -> Result<Output, RunError> {
        self.run(command, Some(Instant::now() + limit))
    }

    fn run(&self, command: &mut Command, deadline: Option<Instant>) -> Result<Output, RunError> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(RunError::Io)?;
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

    /// Ends every running child and refuses new ones
    ///
    /// Each gets SIGTERM, so it can end its own children, and SIGKILL if it
    /// is still running after a grace period.
    pub(super) fn stop(&self) {
        {
            let mut running = self.lock();
            running.stopping = true;
            for (_, child) in &running.children {
                let _ = Command::new("kill")
                    .arg(child.id().to_string())
                    .stdin(Stdio::null())
                    .status();
            }
        }
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline && !self.lock().children.is_empty() {
            thread::sleep(POLL);
        }
        for (_, child) in &mut self.lock().children {
            let _ = child.kill();
        }
    }

    fn wait(&self, id: u64, deadline: Option<Instant>) -> Result<ExitStatus, RunError> {
        loop {
            {
                let mut running = self.lock();
                let at = running
                    .children
                    .iter()
                    .position(|(i, _)| *i == id)
                    .expect("only wait removes a child");
                if let Some(status) = running.children[at].1.try_wait().map_err(RunError::Io)? {
                    running.children.remove(at);
                    return Ok(status);
                }
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    let (_, mut child) = running.children.remove(at);
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunError::TimedOut);
                }
            }
            thread::sleep(POLL);
        }
    }

    fn lock(&self) -> MutexGuard<'_, Running> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
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
