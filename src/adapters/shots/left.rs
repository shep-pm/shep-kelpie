//! A shots step that fails or is cut short leaves no process behind

use std::collections::BTreeMap;
use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::preview::Route;

// A stand-in for the sandbox runtime. It starts a server that ignores
// SIGTERM, as a dev server's own children can, and records its pid beside the
// settings file. In `exit` mode it then exits at once, leaving the server in
// its group; in `stay` mode it runs until a signal ends it.
const FAKE_SRT: &str = r#"
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const settings = process.argv[process.argv.indexOf('--settings') + 1];
const server = spawn('sh', ['-c', 'trap "" TERM; exec sleep 300'], { stdio: 'ignore' });
fs.writeFileSync(path.join(path.dirname(settings), 'server.pid'), String(server.pid));
if (process.env.FAKE_SRT_MODE === 'exit') {
  server.unref();
  process.exit(1);
}
setInterval(() => {}, 1000);
"#;

struct Rig {
    dir: tempfile::TempDir,
    cli: ShotsCli,
    job: ShotsJob,
}

impl Rig {
    fn new(mode: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let tools = Tools::under(dir.path());
        fs::create_dir_all(tools.sandbox().parent().unwrap()).unwrap();
        fs::write(tools.sandbox(), FAKE_SRT).unwrap();
        fs::create_dir_all(tools.playwright_cli().parent().unwrap()).unwrap();
        fs::write(tools.playwright_cli(), "").unwrap();
        let worktree = dir.path().join("wt");
        fs::create_dir(&worktree).unwrap();
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let job = ShotsJob {
            worktree,
            build: dir.path().join("build"),
            out: dir.path().join("out"),
            launch: Ok(Launch {
                name: "dev".into(),
                runtime_executable: "true".into(),
                runtime_args: vec![],
                port,
                cwd: None,
            }),
            routes: vec![Route::try_from("/".to_owned()).unwrap()],
            domains: vec![],
            env: BTreeMap::from([("FAKE_SRT_MODE".to_owned(), PathBuf::from(mode))]),
            server_pid: dir.path().join("dev-server.pid"),
            shep_home: "/srv/shep".into(),
        };
        Self {
            cli: ShotsCli::new(tools),
            job,
            dir,
        }
    }

    fn server_pid(&self) -> Option<u32> {
        fs::read_to_string(self.job.out.join("server.pid"))
            .ok()?
            .parse()
            .ok()
    }

    fn wait_for_server(&self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(pid) = self.server_pid() {
                return pid;
            }
            assert!(Instant::now() < deadline, "the fake runtime never started");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

// `kill -0` succeeds on a zombie, so `ps`'s state column is read instead.
fn running(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&output.stdout);
    !stat.trim().is_empty() && !stat.trim().starts_with('Z')
}

fn gone_within(pid: u32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if !running(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    !running(pid)
}

#[test]
fn a_dev_server_that_exits_leaves_no_child_of_its_group_running() {
    let rig = Rig::new("exit");
    let run = rig.cli.take(&rig.job);
    assert!(
        run.failed
            .as_deref()
            .is_some_and(|f| f.starts_with("the dev server exited")),
        "{:?}",
        run.failed
    );
    let server = rig.server_pid().expect("the fake runtime never started");
    assert!(
        gone_within(server, Duration::from_secs(5)),
        "the server it started outlived the run"
    );
    assert!(!rig.dir.path().join("dev-server.pid").exists());
}

#[test]
fn a_run_stopped_midway_leaves_no_server_running() {
    let rig = Rig::new("stay");
    let (done, finished) = mpsc::channel();
    let cli = rig.cli.clone();
    let job = rig.job.clone();
    thread::spawn(move || {
        let _ = done.send(cli.take(&job));
    });
    let server = rig.wait_for_server();
    rig.cli.stop();
    finished
        .recv_timeout(Duration::from_secs(15))
        .expect("the stopped run never returned");
    assert!(
        gone_within(server, Duration::from_secs(5)),
        "the server it started outlived the run"
    );
}
