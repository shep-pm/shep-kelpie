use std::time::Duration;

use tokio::sync::mpsc;

use super::*;
use crate::dog::e2e::stand_in::StandIn;

const TABLE: &str = r#"{"host":"e2e-box"}"#;

// How long a test waits for a line before it calls the run stuck.
const BOUND: Duration = Duration::from_secs(10);

fn plan(stand: &StandIn, worktree: &Path) -> Plan {
    Plan::read(&stand.kelpie_home(), Some(worktree)).unwrap()
}

// A link at `at` to `target`. Windows makes a symlink only with Developer Mode
// or as an administrator (#1271).
fn link(target: &Path, at: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, at).unwrap();
    #[cfg(windows)]
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, at).unwrap();
    } else {
        std::os::windows::fs::symlink_file(target, at).unwrap();
    }
}

// A run of koji's worktree 7, with every line it sent.
async fn go(stand: &StandIn, worktree: &Path) -> (Result<i32, String>, Vec<String>) {
    let (tx, mut rx) = mpsc::channel(256);
    let plan = plan(stand, worktree);
    let ran = run(&stand.remote, &plan, tx);
    let heard = async {
        let mut lines = Vec::new();
        while let Some(line) = rx.recv().await {
            lines.push(line);
        }
        lines
    };
    let (result, lines) = tokio::join!(ran, heard);
    (result, lines)
}

#[tokio::test]
async fn a_run_copies_the_worktree_runs_the_command_and_copies_the_results_back() {
    let stand = StandIn::new();
    let script =
        "cat src/app.txt > seen.txt; mkdir -p test-results; echo report > test-results/report.txt";
    let worktree = stand.project("koji", TABLE, 7, script);
    std::fs::create_dir_all(worktree.join("src")).unwrap();
    std::fs::write(worktree.join("src/app.txt"), "the app\n").unwrap();

    let (result, _) = go(&stand, &worktree).await;

    assert_eq!(result, Ok(0));
    let host = stand.host_home("e2e-box").join("kelpie-e2e/koji");
    assert_eq!(
        std::fs::read_to_string(host.join("seen.txt")).unwrap(),
        "the app\n",
        "the command ran in the copy"
    );
    assert!(!host.join(".git").exists(), "the repo's own folder stays");
    let back = std::fs::read_to_string(worktree.join("test-results/report.txt"));
    assert_eq!(back.unwrap(), "report\n");
    assert!(
        !worktree.join("seen.txt").exists(),
        "only the results folder comes back"
    );
}

#[tokio::test]
async fn the_commands_output_reaches_the_worker_line_by_line_and_its_exit_code_is_the_answer() {
    let stand = StandIn::new();
    let script = format!(
        "mkdir -p test-results; echo first; {}; echo second; exit 7",
        stand.wait_at_gate()
    );
    let worktree = stand.project("koji", TABLE, 7, &script);
    let (tx, mut rx) = mpsc::channel(256);
    let plan = plan(&stand, &worktree);
    let ran = run(&stand.remote, &plan, tx);

    let worker = async {
        // The command is held at the gate, so only a line sent as it
        // arrived can be here.
        let first = tokio::time::timeout(BOUND, rx.recv()).await.unwrap();
        assert_eq!(first.as_deref(), Some("first"));
        stand.open_gate();
        let mut rest = Vec::new();
        while let Some(line) = rx.recv().await {
            rest.push(line);
        }
        rest
    };
    let (result, rest) = tokio::join!(ran, worker);

    assert_eq!(rest, ["second"]);
    assert_eq!(result, Ok(7));
}

#[tokio::test]
async fn results_come_back_when_the_command_fails() {
    let stand = StandIn::new();
    let script = "mkdir -p test-results; echo 'not ok' > test-results/report.txt; exit 3";
    let worktree = stand.project("koji", TABLE, 7, script);

    let (result, lines) = go(&stand, &worktree).await;

    assert_eq!(result, Ok(3));
    assert_eq!(lines, [] as [String; 0]);
    let back = std::fs::read_to_string(worktree.join("test-results/report.txt"));
    assert_eq!(back.unwrap(), "not ok\n");
}

#[tokio::test]
async fn only_the_results_folder_comes_back_and_a_link_on_the_host_is_not_followed() {
    let stand = StandIn::new();
    let sibling = stand.outside();
    std::fs::write(sibling.join("file.txt"), "a file the host links to\n").unwrap();
    let script = format!(
        "mkdir -p test-results/sub; echo a > test-results/a.txt; echo b > test-results/sub/b.txt; \
         echo secret > notes.txt; ln -s ../notes.txt test-results/notes-link; \
         ln -s '{0}/file.txt' test-results/file-link; ln -s '{0}' test-results/dir-link",
        sibling.display()
    );
    let worktree = stand.project("koji", TABLE, 7, &script);

    let (result, _) = go(&stand, &worktree).await;

    assert_eq!(result, Ok(0));
    let results = worktree.join("test-results");
    assert_eq!(
        std::fs::read_to_string(results.join("a.txt")).unwrap(),
        "a\n"
    );
    assert_eq!(
        std::fs::read_to_string(results.join("sub/b.txt")).unwrap(),
        "b\n"
    );
    for link in ["notes-link", "file-link", "dir-link"] {
        let found = std::fs::symlink_metadata(results.join(link));
        assert!(found.is_err(), "{link} came back");
    }
    assert!(!worktree.join("notes.txt").exists());
}

#[tokio::test]
async fn a_results_folder_that_is_a_link_in_the_worktree_is_refused() {
    let stand = StandIn::new();
    let script = "mkdir -p test-results out/results; echo r > test-results/r.txt; echo r > out/results/r.txt";
    let outside = stand.outside();

    // The results folder itself.
    let worktree = stand.project("koji", TABLE, 7, script);
    link(&outside, &worktree.join("test-results"));
    let (result, lines) = go(&stand, &worktree).await;
    assert_eq!(result, Ok(0), "the command's own code stands");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("test-results in the worktree is a link"),
        "{lines:?}"
    );

    // A folder above it.
    let nested = r#"{"host":"e2e-box","results":"out/results"}"#;
    let worktree = stand.project("golbat", nested, 3, script);
    link(&outside, &worktree.join("out"));
    let (result, lines) = go(&stand, &worktree).await;
    assert_eq!(result, Ok(0));
    assert!(
        lines[0].contains("out in the worktree is a link"),
        "{lines:?}"
    );

    let left: Vec<_> = std::fs::read_dir(&outside).unwrap().collect();
    assert!(
        left.is_empty(),
        "something came back through the link: {left:?}"
    );
}

// A junction needs no privilege, and is a reparse point as a symlink is.
#[tokio::test]
#[cfg(windows)]
async fn a_results_folder_that_is_a_junction_in_the_worktree_is_refused() {
    let stand = StandIn::new();
    let worktree = stand.project(
        "koji",
        TABLE,
        7,
        "mkdir -p test-results; echo r > test-results/r.txt",
    );
    let outside = stand.outside();
    let made = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(worktree.join("test-results"))
        .arg(&outside)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");

    let (result, lines) = go(&stand, &worktree).await;

    assert_eq!(result, Ok(0));
    assert!(
        lines[0].contains("test-results in the worktree is a link"),
        "{lines:?}"
    );
    let left: Vec<_> = std::fs::read_dir(&outside).unwrap().collect();
    assert!(
        left.is_empty(),
        "something came back through the junction: {left:?}"
    );
}

#[tokio::test]
async fn a_link_inside_the_results_folder_is_not_written_through() {
    let stand = StandIn::new();
    let script = "mkdir -p test-results/sub; echo new > test-results/report.txt; \
                  echo new > test-results/sub/b.txt";
    let worktree = stand.project("koji", TABLE, 7, script);
    let outside = stand.outside();
    std::fs::write(outside.join("target.txt"), "mine\n").unwrap();
    std::fs::create_dir_all(worktree.join("test-results")).unwrap();
    link(
        &outside.join("target.txt"),
        &worktree.join("test-results/report.txt"),
    );
    link(&outside, &worktree.join("test-results/sub"));

    let (result, lines) = go(&stand, &worktree).await;

    assert_eq!(result, Ok(0));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("the results did not come back"),
        "{lines:?}"
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("target.txt")).unwrap(),
        "mine\n"
    );
    assert!(!outside.join("b.txt").exists());
}

#[tokio::test]
async fn a_hard_link_in_the_results_folder_is_replaced_not_written_through() {
    let stand = StandIn::new();
    let worktree = stand.project(
        "koji",
        TABLE,
        7,
        "mkdir -p test-results; echo new > test-results/report.txt",
    );
    let outside = stand.outside();
    std::fs::write(outside.join("target.txt"), "mine\n").unwrap();
    std::fs::create_dir_all(worktree.join("test-results")).unwrap();
    std::fs::hard_link(
        outside.join("target.txt"),
        worktree.join("test-results/report.txt"),
    )
    .unwrap();

    let (result, lines) = go(&stand, &worktree).await;

    assert_eq!(result, Ok(0));
    assert_eq!(lines, [] as [String; 0]);
    assert_eq!(
        std::fs::read_to_string(outside.join("target.txt")).unwrap(),
        "mine\n"
    );
    let back = std::fs::read_to_string(worktree.join("test-results/report.txt"));
    assert_eq!(back.unwrap(), "new\n");
}

// The run is on a thread and runtime of its own, so a copy that blocks on the
// FIFO fails this test by timeout instead of hanging it.
#[tokio::test]
#[cfg_attr(windows, ignore = "a FIFO is a Unix file")]
async fn a_fifo_in_the_results_folder_does_not_hold_the_run() {
    let stand = StandIn::new();
    let script = format!(
        "mkdir -p test-results; {}; echo new > test-results/report.txt",
        stand.wait_at_gate()
    );
    let worktree = stand.project("koji", TABLE, 7, &script);
    let (tx, mut rx) = mpsc::channel(256);
    let (remote, plan) = (stand.remote.clone(), plan(&stand, &worktree));
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _ = done.send(runtime.block_on(run(&remote, &plan, tx)));
    });

    // The FIFO appears after the copy out, which would block on it.
    while stand.ssh_pid().is_none() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::fs::create_dir_all(worktree.join("test-results")).unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(worktree.join("test-results/report.txt"))
        .status();
    assert!(made.unwrap().success());
    stand.open_gate();

    let result = ended
        .recv_timeout(BOUND)
        .expect("the run was held by the FIFO");
    assert_eq!(result, Ok(0));
    assert_eq!(rx.recv().await, None);
    let back = std::fs::read_to_string(worktree.join("test-results/report.txt"));
    assert_eq!(back.unwrap(), "new\n");
}

#[tokio::test]
async fn the_local_side_starts_only_ssh_and_rsync() {
    let stand = StandIn::new();
    let worktree = stand.project("koji", TABLE, 7, "mkdir -p test-results");

    let (result, _) = go(&stand, &worktree).await;

    assert_eq!(result, Ok(0));
    #[cfg(unix)]
    let me = std::process::id();
    let started = stand.started();
    let names: Vec<&str> = started
        .iter()
        .map(|s| s.split(' ').next().unwrap())
        .collect();
    assert_eq!(names, ["rsync", "ssh", "rsync"]);
    // Each started by the dog itself, not by a shell it ran. On Windows the
    // stand-in's parent is the `.cmd` that starts its script, so only Unix says.
    #[cfg(unix)]
    for line in &started {
        assert_eq!(
            line.split(' ').nth(1),
            Some(me.to_string().as_str()),
            "{line}"
        );
    }
    assert!(
        started[1].ends_with("-tt e2e-box cd 'kelpie-e2e/koji' && './scripts/e2e-remote.sh'"),
        "{}",
        started[1]
    );
}

#[tokio::test]
async fn a_copy_that_cannot_start_is_an_error_and_nothing_runs() {
    let stand = StandIn::new();
    let worktree = stand.project("koji", TABLE, 7, "echo ran");
    let remote = Remote {
        rsync: stand.outside().join("no-rsync"),
        ..stand.remote.clone()
    };
    let (tx, _rx) = mpsc::channel(8);

    let result = run(&remote, &plan(&stand, &worktree), tx).await;

    let why = result.unwrap_err();
    assert!(why.contains("cannot start"), "{why}");
    assert!(stand.started().is_empty());
}

// Cargo sets it for every test binary it runs, and no run reads it.
const SET: &str = "CARGO_PKG_DESCRIPTION";

#[tokio::test]
async fn ssh_and_rsync_never_see_a_hidden_variable() {
    assert!(std::env::var_os(SET).is_some(), "{SET} is set under cargo");
    crate::spawn::hide([SET.to_owned()]);
    let stand = StandIn::new();
    let worktree = stand.project("koji", TABLE, 7, "mkdir -p test-results");

    let (result, _) = go(&stand, &worktree).await;

    assert_eq!(result, Ok(0));
    for program in ["ssh", "rsync"] {
        let seen = stand.env_of(program);
        assert!(seen.contains("PATH="), "{program}: {seen}");
        assert!(!seen.contains(SET), "{program} saw it");
    }
}
