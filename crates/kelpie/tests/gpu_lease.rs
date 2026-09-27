//! `kelpie lease` on the GPU, end to end against the real binary
//!
//! Every test takes a lock under its own scratch `TMPDIR`, never the
//! maintainer's. The ignored test runs the maintainer's own qwen-review
//! script against a scratch lock too, and stops it before any model call.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const KELPIE: &str = env!("CARGO_BIN_EXE_kelpie");
const TAKE_WHAT: &str =
    "kelpie lease take, held for the maintainer until `kelpie lease return gpu`";

// A scratch temporary folder and no shepherd, so no test reaches the
// maintainer's lock or dog.
struct Scratch {
    temp: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        Self {
            temp: tempfile::tempdir().unwrap(),
        }
    }

    fn lock(&self) -> PathBuf {
        self.temp.path().join("qwen-review/gpu.lock")
    }

    fn kelpie(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(KELPIE);
        cmd.args(args)
            .env("TMPDIR", self.temp.path())
            .env("SHEP_HOME", self.temp.path().join("no-shepherd"))
            .stdin(Stdio::null());
        cmd
    }

    fn read(&self, file: &str) -> String {
        let path = self.lock().join(file);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }
}

fn until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn wait_output(child: Child, timeout: Duration) -> Output {
    let pid = child.id();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(child.wait_with_output().unwrap()));
    match finished.recv_timeout(timeout) {
        Ok(output) => output,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
            panic!("pid {pid} did not exit within {timeout:?}, so it was killed")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("waiting on pid {pid} failed")
        }
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn run_holds_the_lock_in_the_scripts_format_while_the_command_runs() {
    let s = Scratch::new();
    let lock = s.lock();
    // The lock's path goes in as an argument, never into the script text.
    let script = r#"cat "$0/pid" "$0/what"; exit 3"#;
    let command = ["sh", "-c", script, lock.to_str().unwrap()];
    let mut args = vec!["lease", "run", "gpu", "--"];
    args.extend(command);
    let out = s.kelpie(&args).output().unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let seen = text(&out.stdout);
    let mut lines = seen.lines();
    let pid: u32 = lines.next().unwrap().parse().unwrap();
    assert_ne!(pid, 0);
    let what = format!("kelpie lease run: {}", command.join(" "));
    assert_eq!(lines.next(), Some(what.as_str()));
    assert!(!lock.exists(), "the lock outlived the command");
}

#[test]
fn run_removes_the_lock_when_interrupted() {
    let s = Scratch::new();
    // Its own process group, so the interrupt reaches kelpie and the
    // command together, as a terminal's does.
    let child = {
        use std::os::unix::process::CommandExt;
        s.kelpie(&["lease", "run", "gpu", "--", "sleep", "30"])
            .process_group(0)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    until("the lock", Duration::from_secs(5), || s.lock().exists());
    let group = format!("-{}", child.id());
    Command::new("kill")
        .args(["-INT", "--", &group])
        .status()
        .unwrap();
    let out = wait_output(child, Duration::from_secs(5));
    assert_eq!(out.status.code(), Some(130), "{}", text(&out.stderr));
    assert!(!s.lock().exists(), "the lock outlived the interrupt");
}

#[test]
fn run_passes_a_terminate_on_and_removes_the_lock() {
    let s = Scratch::new();
    let child = s
        .kelpie(&["lease", "run", "gpu", "--", "sleep", "30"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    until("the lock", Duration::from_secs(5), || s.lock().exists());
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let out = wait_output(child, Duration::from_secs(5));
    assert_eq!(out.status.code(), Some(143), "{}", text(&out.stderr));
    assert!(!s.lock().exists(), "the lock outlived the command");
}

#[test]
fn a_command_that_cannot_start_leaves_no_lock() {
    let s = Scratch::new();
    let out = s
        .kelpie(&["lease", "run", "gpu", "--", "/no/such/program"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("cannot run /no/such/program"));
    assert!(!s.lock().exists());
}

#[test]
fn take_holds_the_lock_past_its_own_exit_and_a_waiter_goes_after_return() {
    let s = Scratch::new();
    let take = s.kelpie(&["lease", "take", "gpu"]).output().unwrap();
    assert!(take.status.success(), "{}", text(&take.stderr));
    let holder: u32 = s.read("pid").trim().parse().unwrap();
    assert!(alive(holder), "the holder died with take");
    assert_eq!(s.read("what"), format!("{TAKE_WHAT}\n"));

    let again = s.kelpie(&["lease", "take", "gpu"]).output().unwrap();
    assert!(text(&again.stdout).contains("already held for you"));

    let waiter = s
        .kelpie(&["lease", "run", "gpu", "--", "true"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(
        s.read("pid").trim(),
        holder.to_string(),
        "the waiter jumped in"
    );

    let back = s.kelpie(&["lease", "return", "gpu"]).output().unwrap();
    assert!(back.status.success(), "{}", text(&back.stderr));
    until("the holder to end", Duration::from_secs(5), || {
        !alive(holder)
    });

    // The scripts' first nap is 15 s, so the waiter looks again after it.
    let out = wait_output(waiter, Duration::from_secs(25));
    assert!(out.status.success(), "{}", text(&out.stderr));
    let said = text(&out.stderr);
    assert!(
        said.contains(&format!("waiting 0s for the GPU, now held by pid {holder}")),
        "{said}"
    );
    assert!(!s.lock().exists());
}

#[test]
fn return_refuses_a_lock_it_did_not_take() {
    let s = Scratch::new();
    let mut round = Command::new("sleep").arg("30").spawn().unwrap();
    std::fs::create_dir_all(s.lock()).unwrap();
    std::fs::write(s.lock().join("pid"), format!("{}\n", round.id())).unwrap();
    std::fs::write(s.lock().join("what"), "round 2 in /tmp/hunks\n").unwrap();

    let out = s.kelpie(&["lease", "return", "gpu"]).output().unwrap();
    round.kill().unwrap();
    round.wait().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains(&format!(
            "held by pid {} running round 2 in /tmp/hunks, not by `kelpie lease take gpu`",
            round.id()
        )),
        "{}",
        text(&out.stderr)
    );
    assert!(s.lock().exists(), "return removed someone else's lock");
}

#[test]
fn return_with_nothing_taken_says_so() {
    let s = Scratch::new();
    let out = s.kelpie(&["lease", "return", "gpu"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("the GPU lock is free"));
}

#[test]
fn status_reads_the_gpu_from_its_lock_when_no_dog_answers() {
    let s = Scratch::new();
    let take = s.kelpie(&["lease", "take", "gpu"]).output().unwrap();
    assert!(take.status.success(), "{}", text(&take.stderr));
    let holder: u32 = s.read("pid").trim().parse().unwrap();

    let out = s.kelpie(&["lease", "status"]).output().unwrap();
    s.kelpie(&["lease", "return", "gpu"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "no dog is running");
    assert!(text(&out.stderr).contains("the dog did not answer"));
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let gpu = &status["leases"][0];
    assert_eq!(gpu["kind"], "gpu");
    assert_eq!(gpu["lock"], s.lock().display().to_string());
    assert_eq!(gpu["holder"]["pid"], holder);
    assert_eq!(gpu["holder"]["what"], TAKE_WHAT);
    assert!(gpu["since"].as_u64().is_some(), "{gpu}");
}

// A sheep's environment: what the pinned shepherd hands its sheep.
fn lock_seen_by(env: &[(&str, &Path)]) -> String {
    let scratch = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(KELPIE);
    cmd.args(["lease", "status"])
        .env_clear()
        .env("HOME", std::env::var_os("HOME").unwrap())
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("SHEP_HOME", scratch.path());
    for (name, value) in env {
        cmd.env(name, value);
    }
    let out = cmd.output().unwrap();
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    status["leases"][0]["lock"].as_str().unwrap().to_owned()
}

#[test]
#[cfg(target_os = "macos")]
fn a_sheep_without_tmpdir_finds_the_lock_the_maintainers_shell_uses() {
    let getconf = Command::new("getconf")
        .arg("DARWIN_USER_TEMP_DIR")
        .output()
        .unwrap();
    let user_temp = text(&getconf.stdout).trim().to_owned();
    assert_eq!(
        Path::new(&lock_seen_by(&[])),
        Path::new(&user_temp).join("qwen-review/gpu.lock")
    );
}

#[test]
fn a_sheep_given_tmpdir_uses_it() {
    let scratch = tempfile::tempdir().unwrap();
    assert_eq!(
        Path::new(&lock_seen_by(&[("TMPDIR", scratch.path())])),
        scratch.path().join("qwen-review/gpu.lock")
    );
}

#[test]
fn an_unknown_lease_command_prints_usage() {
    let s = Scratch::new();
    let out = s.kelpie(&["lease", "run", "gpu", "--"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("kelpie lease take <kind>"));
}

fn qwen_review() -> PathBuf {
    Path::new(&std::env::var_os("HOME").unwrap()).join(".claude/scripts/qwen-review.sh")
}

// Asks for a file that does not exist, so the script dies right after it
// takes the lock and never calls the model.
#[test]
#[ignore = "runs the maintainer's qwen-review.sh against a scratch lock, about 20 s"]
fn a_qwen_run_waits_behind_take_and_proceeds_after_return() {
    let s = Scratch::new();
    let hunks = s.temp.path().join("hunks");
    std::fs::create_dir(&hunks).unwrap();
    let qwen = |probe: bool| {
        let mut cmd = Command::new(qwen_review());
        cmd.args(["--dir", hunks.to_str().unwrap(), "--files", "missing.rs"])
            .env("TMPDIR", s.temp.path())
            .env("QWEN_LOCK_PROBE", if probe { "1" } else { "0" })
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    };

    let take = s.kelpie(&["lease", "take", "gpu"]).output().unwrap();
    assert!(take.status.success(), "{}", text(&take.stderr));
    let probe = qwen(true).output().unwrap();
    assert_eq!(probe.status.code(), Some(4), "{}", text(&probe.stderr));
    assert!(text(&probe.stderr).contains("kelpie lease take"));

    let round = qwen(false).spawn().unwrap();
    std::thread::sleep(Duration::from_secs(2));
    let back = s.kelpie(&["lease", "return", "gpu"]).output().unwrap();
    assert!(back.status.success(), "{}", text(&back.stderr));

    let out = wait_output(round, Duration::from_secs(30));
    let said = text(&out.stderr);
    assert!(said.contains("waiting 0s for the GPU"), "{said}");
    assert!(said.contains("GPU free after"), "{said}");
    assert!(said.contains("no existing files to review"), "{said}");
    assert!(!s.lock().exists());
}
