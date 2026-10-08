//! Child processes the runner starts, each leading a process group
//!
//! The group lets one call be ended with whatever it spawned, at its
//! ceiling or when its ending is asked. A stop is shep's: the runner sends
//! each group SIGTERM and exits, and shep's stop ends every lamb it leaves. Only this module reaps the
//! children, and only under the lock, so a signalled pid is never a reused one.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::ports::Ending;

/// How often a running child is checked for its exit
const POLL: Duration = Duration::from_millis(50);

// Time for an ended call's group to exit on SIGTERM before it gets SIGKILL.
const END_GRACE: Duration = Duration::from_secs(3);

/// Why a stoppable child did not run to its end
#[derive(Debug)]
pub(super) enum RunError {
    /// It could not be started or waited on
    Io(io::Error),
    /// [`Processes::stop`] ended it, or came first
    Stopped,
    /// It ran past its limit, or its [`Ending`] was asked, and was killed
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
    #[cfg(test)]
    pub(super) fn output(&self, command: &mut Command) -> Result<Output, RunError> {
        self.run(command, Until::default(), &|_| {}, None, b"")
    }

    /// Like `output`, killing the child once `ending` is asked if one is
    /// given, and telling `spawned` its pid as soon as it runs
    pub(super) fn output_telling(
        &self,
        command: &mut Command,
        ending: Option<&Ending>,
        spawned: &dyn Fn(u32),
    ) -> Result<Output, RunError> {
        let until = Until {
            deadline: None,
            ending,
        };
        self.run(command, until, spawned, None, b"")
    }

    /// Like `output_telling`, with the child's stdout and stderr written to
    /// the files `outputs` names, emptied first, and read back and removed
    /// once it ends
    ///
    /// A pipe the child shares with a parent that makes it non-blocking, as
    /// Node does with its own, fails a write once the pipe is full, and a
    /// program that does not wait and write again dies of it. A file never
    /// fills that way.
    pub(super) fn output_to_files(
        &self,
        command: &mut Command,
        ending: Option<&Ending>,
        spawned: &dyn Fn(u32),
        outputs: [&Path; 2],
    ) -> Result<Output, RunError> {
        let until = Until {
            deadline: None,
            ending,
        };
        self.run(command, until, spawned, Some(outputs), b"")
    }

    /// Like `output`, and kills the child once `limit` has passed
    pub(super) fn output_within(
        &self,
        command: &mut Command,
        limit: Duration,
    ) -> Result<Output, RunError> {
        self.output_within_fed(command, limit, b"")
    }

    /// Like `output_within`, with `input` written to the child's stdin
    ///
    /// A secret goes this way rather than in the arguments, which any
    /// process on the machine can list.
    pub(super) fn output_within_fed(
        &self,
        command: &mut Command,
        limit: Duration,
        input: &[u8],
    ) -> Result<Output, RunError> {
        let until = Until {
            deadline: Some(Instant::now() + limit),
            ending: None,
        };
        self.run(command, until, &|_| {}, None, input)
    }

    fn run(
        &self,
        command: &mut Command,
        until: Until<'_>,
        spawned: &dyn Fn(u32),
        outputs: Option<[&Path; 2]>,
        input: &[u8],
    ) -> Result<Output, RunError> {
        let (out, err) = match outputs {
            Some([out, err]) => (
                Stdio::from(File::create(out).map_err(RunError::Io)?),
                Stdio::from(File::create(err).map_err(RunError::Io)?),
            ),
            None => (Stdio::piped(), Stdio::piped()),
        };
        // Its own process group, led by its own pid, so a program it spawns
        // and leaves behind (a build, a test run) is reachable by signalling
        // the group, not just the one pid this struct tracks.
        let stdin = match input.is_empty() {
            true => Stdio::null(),
            false => Stdio::piped(),
        };
        let mut child = command
            .stdin(stdin)
            .stdout(out)
            .stderr(err)
            .process_group(0)
            .spawn()
            .map_err(RunError::Io)?;
        let pid = child.id();
        // A few hundred bytes, which a pipe takes whole before the child reads.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input);
        }
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let id = {
            let mut running = self.lock();
            if running.stopping {
                signal_group(pid, "KILL");
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
        let status = self.wait(id, until)?;
        // A stopped child's own children may hold its pipes open, so its
        // output is left unread.
        if self.lock().stopping {
            return Err(RunError::Stopped);
        }
        let (stdout, stderr) = (
            stdout.join().unwrap_or_default(),
            stderr.join().unwrap_or_default(),
        );
        Ok(match outputs {
            Some([out, err]) => Output {
                status,
                stdout: take(out)?,
                stderr: take(err)?,
            },
            None => Output {
                status,
                stdout,
                stderr,
            },
        })
    }

    /// Writes `input` to `command`'s stdin and holds it open until a line of
    /// its stdout is `wanted`, then ends it
    ///
    /// `None` when it closed its stdout first. Its stderr is dropped.
    pub(super) fn answer_within(
        &self,
        command: &mut Command,
        input: &str,
        limit: Duration,
        wanted: &dyn Fn(&str) -> bool,
    ) -> Result<Option<String>, RunError> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(RunError::Io)?;
        let stdin = child.stdin.take();
        let lines = read_lines(child.stdout.take());
        let id = self.keep(child)?;
        // Dropped only once the answer is in: the program may exit on EOF.
        let written = stdin.map(|mut stdin| stdin.write_all(input.as_bytes()).map(|()| stdin));
        let deadline = Instant::now() + limit;
        let answer = match written {
            Some(Err(e)) => Err(RunError::Io(e)),
            _ => loop {
                if self.stopping() {
                    break Err(RunError::Stopped);
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break Err(RunError::TimedOut);
                }
                match lines.recv_timeout(left.min(POLL)) {
                    Ok(line) if wanted(&line) => break Ok(Some(line)),
                    Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break Ok(None),
                }
            },
        };
        self.end(id);
        answer
    }

    // Keeps `child` where `stop` reaches it, or ends it if the runner is stopping.
    fn keep(&self, mut child: Child) -> Result<u64, RunError> {
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

    /// Stops child `id`, and whatever it spawned
    ///
    /// Its group gets SIGTERM, then SIGKILL if any of it is still running
    /// after a grace period.
    pub(super) fn end(&self, id: u64) {
        let Some(pid) = self.with_child(id, |child| child.id()) else {
            return;
        };
        signal_group(pid, "TERM");
        let deadline = Instant::now() + END_GRACE;
        while Instant::now() < deadline {
            let exited = self.with_child(id, |child| matches!(child.try_wait(), Ok(Some(_))));
            // A zombie leader still counts as a member, so the group is
            // asked about once the leader is reaped.
            if exited.unwrap_or(true) && !group_running(pid) {
                self.forget(id);
                return;
            }
            thread::sleep(POLL);
        }
        signal_group(pid, "KILL");
        if let Some(mut child) = self.forget(id) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    // Runs `f` on child `id` under the lock, while it is listed.
    fn with_child<T>(&self, id: u64, f: impl FnOnce(&mut Child) -> T) -> Option<T> {
        let mut running = self.lock();
        let child = running.children.iter_mut().find(|(i, _)| *i == id);
        child.map(|(_, child)| f(child))
    }

    // Takes child `id` off the list.
    fn forget(&self, id: u64) -> Option<Child> {
        let mut running = self.lock();
        let at = running.children.iter().position(|(i, _)| *i == id)?;
        Some(running.children.remove(at).1)
    }

    /// Whether [`Self::stop`] has been called
    pub(super) fn stopping(&self) -> bool {
        self.lock().stopping
    }

    /// Refuses new children, sends each running one's group SIGTERM, and
    /// leaves the rest to shep
    ///
    /// shep's stop ends every lamb once the runner exits, but finds them by
    /// parent, so the group signal reaches a member already reparented to
    /// init. Nothing here waits. A child that ends comes back as
    /// [`RunError::Stopped`].
    pub(super) fn stop(&self) {
        let mut running = self.lock();
        running.stopping = true;
        for (_, child) in &running.children {
            signal_group(child.id(), "TERM");
        }
    }

    fn wait(&self, id: u64, until: Until<'_>) -> Result<ExitStatus, RunError> {
        loop {
            let passed = {
                let mut running = self.lock();
                let at = running
                    .children
                    .iter()
                    .position(|(i, _)| *i == id)
                    .expect("only wait and `end` take a child off the list");
                // Checked before the deadline below, so a child that has
                // already exited by the time a poll lands is never reported
                // as timed out, however close the two were.
                if let Some(status) = running.children[at].1.try_wait().map_err(RunError::Io)? {
                    running.children.remove(at);
                    return Ok(status);
                }
                until.passed()
            };
            if passed {
                // The whole group, so a build or test the worker started
                // and left running past the ceiling is ended too, not just
                // the `claude` process this struct tracked.
                self.end(id);
                return Err(RunError::TimedOut);
            }
            thread::sleep(POLL);
        }
    }

    fn lock(&self) -> MutexGuard<'_, Running> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// What ends a child before it exits: a deadline, the call's ending, or neither
#[derive(Debug, Clone, Copy, Default)]
struct Until<'a> {
    deadline: Option<Instant>,
    ending: Option<&'a Ending>,
}

impl Until<'_> {
    fn passed(self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d) || self.ending.is_some_and(Ending::asked)
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
    let _ = crate::spawn::command("pkill")
        .args([format!("-{signal}"), "-g".to_owned(), pgid.to_string()])
        .stdin(Stdio::null())
        .status();
}

fn group_running(pgid: u32) -> bool {
    crate::spawn::command("pgrep")
        .args(["-g", &pgid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// SIGTERM to `child`'s whole process group, then SIGKILL once `grace` has
// passed with any of it still running. A leader that exits on SIGTERM can
// leave a member that ignores it, so the wait is for the group, and the
// leader is reaped on the way since a zombie still counts as a member.
// `child.kill()` also runs as a fallback for a system with no `pkill` on
// `PATH`, though that alone would miss anything the child had spawned.
fn stop_child(child: &mut Child, grace: Duration) {
    let pid = child.id();
    signal_group(pid, "TERM");
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) && !group_running(pid) {
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

// The whole of the file at `path`, which is then removed: a call's output
// holds its commands' output, which is not kept once read.
fn take(path: &Path) -> Result<Vec<u8>, RunError> {
    let bytes = std::fs::read(path).map_err(RunError::Io)?;
    let _ = std::fs::remove_file(path);
    Ok(bytes)
}

// Each line of `pipe` as it comes, until it closes.
fn read_lines(pipe: Option<impl Read + Send + 'static>) -> Receiver<String> {
    let (send, lines) = mpsc::channel();
    thread::spawn(move || {
        let Some(pipe) = pipe else { return };
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { return };
            if send.send(line).is_err() {
                return;
            }
        }
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_comes_back_while_stdin_is_still_open() {
        // The script exits only on EOF, so the answer arrives with stdin open.
        let processes = Processes::default();
        let script = "read first; echo \"noise\"; echo \"got $first\"; cat >/dev/null";
        let answer = processes.answer_within(
            Command::new("sh").args(["-c", script]),
            "ask\n",
            Duration::from_secs(10),
            &|line| line.starts_with("got"),
        );
        assert_eq!(answer.unwrap().as_deref(), Some("got ask"));

        let silent = processes.answer_within(
            Command::new("sh").args(["-c", "echo nothing wanted"]),
            "",
            Duration::from_secs(10),
            &|line| line.starts_with("got"),
        );
        assert_eq!(silent.unwrap(), None);
    }

    #[test]
    fn an_answer_that_never_comes_times_out() {
        let processes = Processes::default();
        let answer = processes.answer_within(
            Command::new("sh").args(["-c", "cat >/dev/null"]),
            "ask\n",
            Duration::from_millis(200),
            &|_| true,
        );
        assert!(matches!(answer, Err(RunError::TimedOut)), "{answer:?}");
    }

    #[test]
    fn a_child_writing_to_files_gets_them_empty_and_its_output_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let (out, err) = (dir.path().join("out"), dir.path().join("err"));
        std::fs::write(&out, "left from before").unwrap();
        let processes = Processes::default();
        // More than a pipe holds, written by a child no one reads while it runs.
        let script = "head -c 1048576 /dev/zero | tr '\\0' x; echo oops >&2";
        let output = processes
            .output_to_files(
                Command::new("sh").args(["-c", script]),
                None,
                &|_| {},
                [&out, &err],
            )
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 1_048_576);
        assert!(output.stdout.iter().all(|&b| b == b'x'));
        assert_eq!(output.stderr, b"oops\n");
        assert!(!out.exists() && !err.exists());
    }

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

    // Real time: the child is a real process, ended from another thread as
    // the runner ends a turn past its ceiling.
    #[test]
    fn a_child_whose_ending_is_asked_is_ended_with_its_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild-started");
        let pid_file = dir.path().join("grandchild.pid");
        let script = format!(
            "sh -c 'touch \"$1\"; sleep 30' _ {marker} &
             echo $! > {pid_file}
             wait",
            marker = shell_quote(&marker),
            pid_file = shell_quote(&pid_file),
        );
        let ending = Ending::default();
        let asker = ending.clone();
        let started = Instant::now();
        let (done, finished) = mpsc::channel();
        let processes = Processes::default();
        let running = processes.clone();
        thread::spawn(move || {
            let mut command = Command::new("sh");
            command.args(["-c", &script]);
            let _ = done.send(running.output_telling(&mut command, Some(&ending), &|_| {}));
        });
        while !marker.exists() {
            assert!(started.elapsed() < Duration::from_secs(10), "no grandchild");
            thread::sleep(POLL);
        }
        asker.end();
        let result = finished
            .recv_timeout(END_GRACE + Duration::from_secs(10))
            .expect("the ended call never returned");
        assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
        let grandchild: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_is_alive(grandchild) {
            assert!(
                Instant::now() < deadline,
                "the grandchild outlived its call"
            );
            thread::sleep(POLL);
        }
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

    // Real time: real calls that note SIGTERM and keep running, ended by the
    // test as shep's stop would end them. The test bounds its own waits.
    #[test]
    fn a_stop_signals_each_call_s_group_without_waiting_and_refuses_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let processes = Processes::default();
        let (done, finished) = mpsc::channel();
        let (told, pids) = mpsc::channel();
        let calls = ["one", "two"].map(|name| {
            let (ready, termed) = (
                dir.path().join(name),
                dir.path().join(format!("{name}.term")),
            );
            let script = format!(
                "trap 'touch {}' TERM; touch {}; while :; do sleep 0.1; done",
                shell_quote(&termed),
                shell_quote(&ready),
            );
            let (running, done, told) = (processes.clone(), done.clone(), told.clone());
            thread::spawn(move || {
                let mut command = Command::new("sh");
                command.args(["-c", &script]);
                let result = running.output_telling(&mut command, None, &|pid| {
                    let _ = told.send(pid);
                });
                let _ = done.send(result);
            });
            (ready, termed)
        });
        let pids: Vec<u32> = (0..2)
            .map(|_| {
                pids.recv_timeout(Duration::from_secs(10))
                    .expect("no child")
            })
            .collect();
        let started = Instant::now();
        while !calls.iter().all(|(ready, _)| ready.exists()) {
            assert!(started.elapsed() < Duration::from_secs(10), "no trap set");
            thread::sleep(POLL);
        }

        let stopping = Instant::now();
        processes.stop();
        assert!(
            stopping.elapsed() < Duration::from_secs(1),
            "the stop waited"
        );

        let next = processes.output(Command::new("sh").args(["-c", "exit 0"]));
        assert!(matches!(next, Err(RunError::Stopped)), "{next:?}");
        while !calls.iter().all(|(_, termed)| termed.exists()) {
            assert!(
                stopping.elapsed() < Duration::from_secs(10),
                "a group had no SIGTERM"
            );
            thread::sleep(POLL);
        }
        assert!(
            pids.iter().all(|&pid| process_is_alive(pid)),
            "the stop ended a call"
        );
        for pid in pids {
            signal_group(pid, "KILL");
        }
        for _ in 0..2 {
            let result = finished
                .recv_timeout(Duration::from_secs(10))
                .expect("an ended call never returned");
            assert!(matches!(result, Err(RunError::Stopped)), "{result:?}");
        }
    }
}
