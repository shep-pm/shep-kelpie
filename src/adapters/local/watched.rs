//! What a local round tells the runner about its place in the GPU queue

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::LocalReviewer;
use crate::lease::gpu::{Attempt, Claim, GpuLock};
use crate::ports::{Reviewer, RoundStage};
use crate::settings::{LeaseName, LocalCommand, LocalRound};
use crate::test::{git, write_script};

fn command(path: &Path, gpu_lease: bool) -> LocalRound {
    LocalRound::Command(LocalCommand {
        command: path.to_owned(),
        lease: gpu_lease.then(LeaseName::gpu),
        gpu_lease: false,
        ollama: None,
        ollama_model: None,
        paths: Vec::new(),
    })
}

fn stages() -> (Arc<Mutex<Vec<RoundStage>>>, impl Fn(RoundStage) + Sync) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    (seen, move |stage| record.lock().unwrap().push(stage))
}

fn wait_for(seen: &Mutex<Vec<RoundStage>>, stage: RoundStage) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !seen.lock().unwrap().contains(&stage) {
        assert!(Instant::now() < deadline, "never saw {stage:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

// Lets the lock go however the test ends. Otherwise a failing assertion
// leaves the round waiting and the scope joining for good.
struct Release<'a>(&'a GpuLock, u32);

impl Drop for Release<'_> {
    fn drop(&mut self) {
        let _ = self.0.release(self.1);
    }
}

// The test's parent is alive and outside the round's process group.
fn someone_elses_lock(temp: &Path) -> (GpuLock, u32) {
    let lock = GpuLock::under(temp);
    let other = std::os::unix::process::parent_id();
    let claim = Claim {
        pid: other,
        what: "someone else's round".into(),
    };
    assert_eq!(lock.try_take(&claim).unwrap(), Attempt::Taken);
    (lock, other)
}

fn a_worktree(home: &Path) -> PathBuf {
    let worktree = home.join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    git(&worktree, &["init", "--quiet", "-b", "main"]);
    git(
        &worktree,
        &["commit", "--quiet", "--allow-empty", "-m", "init"],
    );
    worktree
}

const ONE_FINDING: &str = "mkdir -p \"$QWEN_REVIEW_OUT\"\n\
     printf 'LOW|a.rs:1|seen|seen\\n' > \"$QWEN_REVIEW_OUT/round-1.txt\"\n\
     : > \"$QWEN_REVIEW_OUT/round-1.txt.done\"\n";

#[test]
fn a_round_holding_the_gpu_itself_reports_queued_then_running() {
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("tmp");
    let (lock, other) = someone_elses_lock(&temp);
    let reviewer = LocalReviewer::default()
        .with_temp_dir(temp)
        .with_naps(|_| Duration::from_millis(1));
    let (seen, watch) = stages();
    let local = command(Path::new("/bin/true"), true);
    let out = dir.path().join("out");
    thread::scope(|scope| {
        let _release = Release(&lock, other);
        let round =
            scope.spawn(|| reviewer.round_watched(&local, dir.path(), "main", &out, 1, "", &watch));
        wait_for(&seen, RoundStage::Queued);
        assert_eq!(*seen.lock().unwrap(), [RoundStage::Queued], "still waiting");
        lock.release(other).unwrap();
        // The folder is no git repo. The round fails reading its head,
        // after it took the lock.
        assert!(round.join().unwrap().is_err());
    });
    assert_eq!(
        *seen.lock().unwrap(),
        [RoundStage::Queued, RoundStage::Running]
    );
}

#[test]
fn a_round_that_finds_the_lock_free_reports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let reviewer = LocalReviewer::default().with_temp_dir(dir.path().join("tmp"));
    let (seen, watch) = stages();
    let local = command(Path::new("/bin/true"), true);
    let out = dir.path().join("out");
    let _ = reviewer.round_watched(&local, dir.path(), "main", &out, 1, "", &watch);
    assert_eq!(*seen.lock().unwrap(), []);
}

// The stand-in script queues on the lock itself, as the qwen script does.
#[test]
fn a_command_queued_on_someone_elses_lock_reports_queued_then_running() {
    let dir = tempfile::tempdir().unwrap();
    let temp = dir.path().join("tmp");
    let (lock, other) = someone_elses_lock(&temp);
    let script = dir.path().join("review");
    write_script(
        &script,
        &format!(
            "#!/bin/sh\n\
             held=\"$TMPDIR/qwen-review/gpu.lock\"\n\
             until mkdir \"$held\" 2>/dev/null; do sleep 0.05; done\n\
             echo $$ > \"$held/pid\"\n\
             sleep 0.6\n\
             {ONE_FINDING}\
             rm -rf \"$held\"\n"
        ),
    );
    let worktree = a_worktree(dir.path());
    let reviewer = LocalReviewer::default().with_temp_dir(temp);
    let (seen, watch) = stages();
    let local = command(&script, false);
    let out = dir.path().join("out");
    let findings = thread::scope(|scope| {
        let _release = Release(&lock, other);
        let round =
            scope.spawn(|| reviewer.round_watched(&local, &worktree, "main", &out, 1, "", &watch));
        wait_for(&seen, RoundStage::Queued);
        lock.release(other).unwrap();
        round.join().unwrap().unwrap()
    });
    assert_eq!(findings.len(), 1);
    assert_eq!(
        *seen.lock().unwrap(),
        [RoundStage::Queued, RoundStage::Running]
    );
    assert!(lock.holder().is_none(), "the script let the lock go");
}

#[test]
fn a_command_that_takes_no_lock_is_never_queued() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("review");
    write_script(&script, &format!("#!/bin/sh\n{ONE_FINDING}"));
    let worktree = a_worktree(dir.path());
    let reviewer = LocalReviewer::default().with_temp_dir(dir.path().join("tmp"));
    let (seen, watch) = stages();
    let local = command(&script, false);
    let out = dir.path().join("out");
    reviewer
        .round_watched(&local, &worktree, "main", &out, 1, "", &watch)
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), []);
}

// Kelpie's own hold is not a queue the command waits in.
#[test]
fn a_command_run_under_kelpies_own_hold_is_never_queued() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("review");
    write_script(&script, &format!("#!/bin/sh\nsleep 0.3\n{ONE_FINDING}"));
    let worktree = a_worktree(dir.path());
    let reviewer = LocalReviewer::default().with_temp_dir(dir.path().join("tmp"));
    let (seen, watch) = stages();
    let local = command(&script, true);
    let out = dir.path().join("out");
    reviewer
        .round_watched(&local, &worktree, "main", &out, 1, "", &watch)
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), []);
}
