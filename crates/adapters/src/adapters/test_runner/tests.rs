use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use crate::ipc::blocking::Listener;
use crate::ports::Reach;
use crate::profile::{CREDENTIALS, WorkerProfile};
use crate::test::OpenSandbox;

// What the dog saw: the ask, and whether the connection was still open when
// the command had written its marker.
struct Dog {
    asked: mpsc::Receiver<String>,
    given_back: mpsc::Receiver<()>,
}

// A dog that grants the one ask it hears at once and holds the lease until the
// connection closes.
fn dog(socket: &Path) -> Dog {
    let listener = Listener::bind(socket).unwrap();
    let (asked, ask) = mpsc::channel();
    let (back, given_back) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        asked.send(line).unwrap();
        stream.write_all(b"\"granted\"\n").unwrap();
        // Ends when the run gives the lease back.
        let _ = stream.read_to_end(&mut Vec::new());
        back.send(()).unwrap();
    });
    Dog {
        asked: ask,
        given_back,
    }
}

struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("tr").tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("wt")).unwrap();
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn worker(&self) -> Reach {
        let (wt, build) = (self.path("wt"), self.path("build"));
        let profile = WorkerProfile {
            worktree: &wt,
            build: &build,
            home: Path::new("/Users/me"),
            git_common_dir: Path::new("/k/repo/.git"),
            git_dir: Path::new("/k/repo/.git/worktrees/7"),
            branch: "kelpie/7",
            base: "main",
            kelpie_pushes: false,
            kelpie: Path::new("/k/kelpie"),
            skills: Path::new("/k/skills"),
            kelpie_home: Path::new("/k"),
            repo: Path::new("/k/repo"),
            guard_hooks: &[],
            allowed_domains: &[],
            registries: &[],
            build_env: &BTreeMap::new(),
            shep_home: Path::new("/k/shep"),
            door: &self.path("d.sock"),
            token: None,
            login_token: None,
        };
        profile.reach()
    }

    fn run(&self, command: &str, reach: Reach) -> TestRun {
        TestRun {
            command: command.to_owned(),
            cwd: self.path("wt"),
            reach,
            settings: self.path("worker/test-run.sandbox.json"),
            door: Some(self.path("d.sock")),
            ceiling: Duration::from_secs(30),
        }
    }
}

fn runner(sandbox: &OpenSandbox) -> SandboxedTests {
    SandboxedTests {
        processes: Processes::default(),
        sandbox: Arc::new(sandbox.clone()),
    }
}

#[test]
fn the_run_takes_the_workers_fence_not_the_maintainers() {
    let w = World::new();
    let dog = dog(&w.path("d.sock"));
    let sandbox = OpenSandbox::default();
    let reach = w.worker();
    let fence = reach.fence.clone().unwrap();

    let outcome = runner(&sandbox).run(&w.run("true", reach));

    assert_eq!(outcome, TestOutcome::Passed);
    let wrapped = sandbox.wrapped();
    assert_eq!(wrapped.len(), 1);
    let (policy, settings) = &wrapped[0];
    assert_eq!(policy.write, fence.write);
    assert!(policy.write.contains(&w.path("wt")));
    assert_eq!(policy.no_write, fence.no_write);
    assert_eq!(policy.sockets, [w.path("d.sock")]);
    for credential in CREDENTIALS {
        assert!(
            policy.no_read.contains(&(*credential).to_owned()),
            "{credential}"
        );
    }
    assert_eq!(settings, &w.path("worker/test-run.sandbox.json"));
    let ask = dog.asked.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(ask.contains("\"take\":\"cargo-test\""), "{ask}");

    // Nothing runs without a fence: that would be the maintainer's reach.
    let unfenced = runner(&sandbox).run(&w.run("true", Reach::default()));
    assert!(
        matches!(unfenced, TestOutcome::NotRun { .. }),
        "{unfenced:?}"
    );
    assert_eq!(
        sandbox.wrapped().len(),
        1,
        "the unfenced run reached no sandbox"
    );
}

#[test]
fn the_lease_is_held_while_the_command_runs_and_given_back_after() {
    let w = World::new();
    let dog = dog(&w.path("d.sock"));
    let sandbox = OpenSandbox::default();
    // Waits for the lease to be given back, which it must not be yet.
    let marker = w.path("wt/seen");
    let command = format!("sleep 0.3; touch {}", marker.display());
    let outcome = thread::scope(|scope| {
        let ran = scope.spawn(|| runner(&sandbox).run(&w.run(&command, w.worker())));
        thread::sleep(Duration::from_millis(100));
        assert!(
            dog.given_back.try_recv().is_err(),
            "the lease went back while the command ran"
        );
        ran.join().unwrap()
    });
    assert_eq!(outcome, TestOutcome::Passed);
    assert!(marker.exists());
    dog.given_back.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn a_failed_run_keeps_the_last_two_kib_of_what_it_wrote_to_either_stream() {
    let w = World::new();
    let _dog = dog(&w.path("d.sock"));
    let sandbox = OpenSandbox::default();
    let command =
        "echo first; echo oops >&2; head -c 3000 /dev/zero | tr '\\0' x; echo end; exit 3";

    let outcome = runner(&sandbox).run(&w.run(command, w.worker()));

    let TestOutcome::Failed { output } = outcome else {
        panic!("{outcome:?}");
    };
    assert!(
        output.len() <= crate::ports::OUTPUT_KEPT,
        "{}",
        output.len()
    );
    assert!(output.ends_with("xend\n"), "{output}");
    assert!(!output.contains("first"), "the head of the output is cut");
}

#[test]
fn a_run_past_its_ceiling_is_killed_and_timed_out() {
    let w = World::new();
    let dog = dog(&w.path("d.sock"));
    let sandbox = OpenSandbox::default();
    let mut run = w.run("sleep 30", w.worker());
    run.ceiling = Duration::from_millis(200);

    let outcome = runner(&sandbox).run(&run);

    assert_eq!(outcome, TestOutcome::TimedOut);
    dog.given_back.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn a_command_the_shell_cannot_find_and_a_dog_that_is_gone_are_not_run() {
    let w = World::new();
    let sandbox = OpenSandbox::default();

    let no_dog = runner(&sandbox).run(&w.run("true", w.worker()));
    let TestOutcome::NotRun { reason } = no_dog else {
        panic!("{no_dog:?}");
    };
    assert!(reason.contains("cannot reach the dog"), "{reason}");

    let _dog = dog(&w.path("d.sock"));
    let missing = runner(&sandbox).run(&w.run("no-such-program-here --flag", w.worker()));
    let TestOutcome::NotRun { reason } = missing else {
        panic!("{missing:?}");
    };
    assert!(reason.contains("could not start"), "{reason}");
}

#[test]
fn a_run_with_no_door_runs_without_asking_the_dog() {
    let w = World::new();
    let sandbox = OpenSandbox::default();
    let mut run = w.run("true", w.worker());
    run.door = None;

    let outcome = runner(&sandbox).run(&run);

    assert_eq!(outcome, TestOutcome::Passed);
}
