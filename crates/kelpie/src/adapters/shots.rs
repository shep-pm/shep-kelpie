//! Shots in headless Chromium, of a dev server kelpie starts in a sandbox
//!
//! The dev server runs under the sandbox runtime (`srt`), the engine Claude
//! Code's own sandbox uses: writes held to the worktree, the build folder and
//! the shots folder, and the network to the project's preview domains.
//! Chromium cannot start inside that sandbox on macOS, so it runs beside it,
//! from kelpie's own copy of Playwright, and refuses the same hosts itself.

use std::fs::{self, File};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use super::process::{Processes, RunError};
use crate::ports::Shots;
use crate::preview::{self, LOCAL_HOSTS, Launch, Tools};
use crate::profile::CREDENTIALS;
use crate::shots::{Scheme, ShotsJob, ShotsRun, Viewport, plan};

/// The capture script, written beside each run's shots
const SCRIPT: &str = include_str!("shots.mjs");

/// How long a dev server gets to answer on its port. A cold Vite start took
/// under a second on the playground; a first `bun` start with its optimiser, 2.
const START: Duration = Duration::from_secs(90);

/// How long the capture script gets: 30 seconds a page at most, and room to spare
const CAPTURE: Duration = Duration::from_secs(600);

/// How often a starting dev server is checked
const POLL: Duration = Duration::from_millis(250);

/// What `srt -d` prints for a connection its proxy refused
const REFUSED: &str = "Connection blocked to ";

/// Takes shots with kelpie's own tools
///
/// Clones share their children, so one clone can stop them all.
#[derive(Debug, Clone)]
pub struct ShotsCli {
    tools: Tools,
    processes: Processes,
}

impl ShotsCli {
    /// Shots taken with the tools in `tools`
    pub fn new(tools: Tools) -> Self {
        Self {
            tools,
            processes: Processes::default(),
        }
    }

    /// Ends a run in flight, dev server included, and refuses new ones
    pub fn stop(&self) {
        self.processes.stop();
    }

    fn run(&self, job: &ShotsJob) -> Result<ShotsRun, String> {
        let launch = preview::launch(&job.worktree, job.configuration.as_deref())
            .map_err(|e| e.to_string())?;
        for tool in [self.tools.sandbox(), self.tools.playwright_cli()] {
            if !tool.is_file() {
                return Err(format!(
                    "{} is missing: run `kelpie tools install`",
                    tool.display()
                ));
            }
        }
        if !port_free(launch.port) {
            return Err(format!(
                "port {} is already in use, so the shots would show another server",
                launch.port
            ));
        }
        // A folder left from a run of other routes would mix its shots into this one.
        match fs::remove_dir_all(&job.out) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("cannot clear the shots folder: {e}"));
            }
            _ => {}
        }
        fs::create_dir_all(&job.out).map_err(|e| format!("cannot make the shots folder: {e}"))?;
        let log = job.out.join("dev-server.log");
        let server = self.start_server(job, &launch, &log)?;
        let taken = self
            .wait_for(server, launch.port, &log)
            .and_then(|()| self.capture(job, launch.port));
        self.processes.end(server);
        let mut run = taken?;
        run.problems.extend(refused_hosts(&log));
        Ok(run)
    }

    fn start_server(&self, job: &ShotsJob, launch: &Launch, log: &Path) -> Result<u64, String> {
        let settings = job.out.join("sandbox.json");
        let text = serde_json::to_string_pretty(&sandbox(job)).expect("settings are JSON");
        fs::write(&settings, text)
            .map_err(|e| format!("cannot write the sandbox's settings: {e}"))?;
        let out =
            File::create(log).map_err(|e| format!("cannot open the dev server's log: {e}"))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("cannot open the dev server's log: {e}"))?;
        let mut command = Command::new("node");
        command
            .arg(self.tools.sandbox())
            .arg("-d")
            .arg("--settings")
            .arg(&settings)
            .arg("--")
            .arg(&launch.runtime_executable)
            .args(&launch.runtime_args)
            .current_dir(&job.worktree)
            .envs(&job.env)
            // Node's fetch ignores the sandbox's proxy without it.
            .env("NODE_USE_ENV_PROXY", "1")
            .env("PORT", launch.port.to_string())
            .stdout(out)
            .stderr(err);
        self.processes.start(&mut command).map_err(|e| match e {
            RunError::Io(e) => format!("cannot start the dev server: {e}"),
            RunError::Stopped | RunError::TimedOut => "kelpie is stopping".into(),
        })
    }

    fn wait_for(&self, server: u64, port: u16, log: &Path) -> Result<(), String> {
        let deadline = Instant::now() + START;
        while Instant::now() < deadline {
            if answers(port) {
                return Ok(());
            }
            if !self.processes.alive(server) {
                return Err(format!("the dev server exited: {}", tail(log)));
            }
            thread::sleep(POLL);
        }
        Err(format!(
            "the dev server did not answer on port {port} in {}s: {}",
            START.as_secs(),
            tail(log)
        ))
    }

    fn capture(&self, job: &ShotsJob, port: u16) -> Result<ShotsRun, String> {
        let mut shots = plan(&job.routes, &job.out);
        let script = job.out.join("shots.mjs");
        let plan_file = job.out.join("plan.json");
        let report_file = job.out.join("report.json");
        let hosts: Vec<&str> = LOCAL_HOSTS
            .into_iter()
            .chain(job.domains.iter().map(String::as_str))
            .collect();
        let planned: Vec<_> = shots
            .iter()
            .map(|s| {
                let (width, height) = match s.viewport {
                    Viewport::Mobile => (390, 844),
                    Viewport::Desktop => (1280, 800),
                };
                json!({
                    "route": s.route.as_str(),
                    "width": width,
                    "height": height,
                    "mobile": s.viewport == Viewport::Mobile,
                    "scheme": match s.scheme { Scheme::Light => "light", Scheme::Dark => "dark" },
                    "file": s.file,
                })
            })
            .collect();
        let plan = json!({
            "tools": self.tools.dir(),
            "base": format!("http://localhost:{port}"),
            "hosts": hosts,
            "shots": planned,
            "report": report_file,
        });
        fs::write(&script, SCRIPT)
            .and_then(|()| fs::write(&plan_file, plan.to_string()))
            .map_err(|e| format!("cannot write the capture script: {e}"))?;
        let mut command = Command::new("node");
        command
            .arg(&script)
            .arg(&plan_file)
            .env("PLAYWRIGHT_BROWSERS_PATH", self.tools.browsers());
        let output = self
            .processes
            .output_within(&mut command, CAPTURE)
            .map_err(|e| match e {
                RunError::Io(e) => format!("cannot run node: {e}"),
                RunError::Stopped => "kelpie is stopping".into(),
                RunError::TimedOut => format!("the capture ran past {}s", CAPTURE.as_secs()),
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("the capture failed: {}", last_lines(&stderr, 5)));
        }
        let text = fs::read_to_string(&report_file)
            .map_err(|e| format!("the capture left no report: {e}"))?;
        let report = parse_report(&text)?;
        if report.len() != shots.len() {
            return Err(format!(
                "the capture reported {} shots of {}",
                report.len(),
                shots.len()
            ));
        }
        for (shot, page) in shots.iter_mut().zip(report) {
            shot.status = page.status;
            shot.problems = page.problems;
            if !page.taken {
                shot.file = None;
            }
        }
        Ok(ShotsRun {
            shots,
            ..ShotsRun::default()
        })
    }
}

impl Shots for ShotsCli {
    fn take(&self, job: &ShotsJob) -> ShotsRun {
        self.run(job).unwrap_or_else(ShotsRun::failed)
    }
}

/// One page as the capture script reports it
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Page {
    status: Option<u16>,
    taken: bool,
    problems: Vec<String>,
}

fn parse_report(text: &str) -> Result<Vec<Page>, String> {
    serde_json::from_str(text).map_err(|e| format!("unreadable capture report: {e}"))
}

// The sandbox runtime's settings for the dev server: the worker's own write
// fence and credential denies, and the preview's domains only. The shots
// folder stays out of its reach, so the server cannot touch what kelpie posts.
fn sandbox(job: &ShotsJob) -> serde_json::Value {
    let deny_read: Vec<&str> = CREDENTIALS
        .iter()
        .map(|p| p.strip_suffix("/**").unwrap_or(p))
        .collect();
    json!({
        "network": {
            "allowedDomains": job.domains,
            "deniedDomains": [],
            "strictAllowlist": true,
            "allowLocalBinding": true,
            "allowMachLookup": ["com.apple.FSEvents"],
        },
        "filesystem": {
            "denyRead": deny_read,
            "allowWrite": [job.worktree, job.build],
            "denyWrite": [],
        },
    })
}

// Whether nothing listens on `port` yet, on either loopback address.
fn port_free(port: u16) -> bool {
    let v4 = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).is_ok();
    let v6 = TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port)));
    // A machine without IPv6 on loopback cannot have anything listening there.
    v4 && v6.map_or_else(|e| e.kind() != std::io::ErrorKind::AddrInUse, |_| true)
}

fn answers(port: u16) -> bool {
    [
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
    ]
    .iter()
    .any(|addr| TcpStream::connect_timeout(addr, POLL).is_ok())
}

// Each host the sandbox's proxy refused the dev server, once.
fn refused_hosts(log: &Path) -> Vec<String> {
    let text = fs::read_to_string(log).unwrap_or_default();
    let mut hosts: Vec<&str> = Vec::new();
    for line in text.lines() {
        if let Some((_, host)) = line.split_once(REFUSED) {
            let host = host.split_whitespace().next().unwrap_or(host);
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
    }
    hosts
        .into_iter()
        .map(|h| format!("the dev server was refused {h}: not a preview domain"))
        .collect()
}

fn tail(log: &Path) -> String {
    let text = fs::read_to_string(log).unwrap_or_default();
    let kept: String = text
        .lines()
        .filter(|l| !l.starts_with("[SandboxDebug]"))
        .collect::<Vec<_>>()
        .join("\n");
    last_lines(&kept, 5)
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim().lines().collect();
    lines[lines.len().saturating_sub(n)..].join(" / ")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::preview::Route;

    #[test]
    fn a_recorded_report_reads_page_by_page() {
        let pages = parse_report(include_str!("../../fixtures/shots-report.json")).unwrap();
        assert_eq!(pages.len(), 4);
        assert_eq!(pages[0].status, Some(200));
        assert!(pages[0].taken);
        assert!(
            pages[0]
                .problems
                .iter()
                .any(|p| p.starts_with("blocked https://cdn.leekduck.com/")
                    && p.ends_with("cdn.leekduck.com is not a preview domain")),
            "{:?}",
            pages[0].problems
        );
    }

    #[test]
    fn hosts_the_proxy_refused_are_named_once_each() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("dev-server.log");
        fs::write(
            &log,
            "  VITE ready\n\
             [SandboxDebug] Connection blocked to registry.npmjs.org:443\n\
             [SandboxDebug] Connection blocked to registry.npmjs.org:443\n\
             [SandboxDebug] Connection blocked to api.example.com:443\n",
        )
        .unwrap();
        assert_eq!(
            refused_hosts(&log),
            [
                "the dev server was refused registry.npmjs.org:443: not a preview domain",
                "the dev server was refused api.example.com:443: not a preview domain",
            ]
        );
        assert_eq!(tail(&log), "VITE ready");
    }

    #[test]
    fn the_dev_server_writes_only_its_folders_and_reads_no_credential() {
        let job = ShotsJob {
            worktree: "/k/wt/lab/7".into(),
            build: "/k/targets/lab/7".into(),
            out: "/k/shots/lab/7/abc1234".into(),
            configuration: None,
            routes: vec![Route::try_from("/".to_owned()).unwrap()],
            domains: vec!["leekduck.com".into()],
            env: BTreeMap::new(),
        };
        let s = sandbox(&job);
        assert_eq!(
            s["filesystem"]["allowWrite"],
            json!(["/k/wt/lab/7", "/k/targets/lab/7"])
        );
        assert_eq!(s["network"]["allowedDomains"], json!(["leekduck.com"]));
        assert_eq!(s["network"]["strictAllowlist"], true);
        let deny = s["filesystem"]["denyRead"].as_array().unwrap();
        assert!(deny.contains(&json!("~/.ssh")));
        assert!(deny.contains(&json!("~/.kelpie/settings.toml")));
    }

    #[test]
    fn a_taken_port_fails_the_run_before_anything_starts() {
        let dir = tempfile::tempdir().unwrap();
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let worktree = dir.path().join("wt");
        fs::create_dir_all(worktree.join(".claude")).unwrap();
        fs::write(
            worktree.join(".claude/launch.json"),
            format!(
                r#"{{"configurations": [{{"name": "dev", "runtimeExecutable": "true", "port": {port}}}]}}"#
            ),
        )
        .unwrap();
        let tools = dir.path().join("tools");
        let cli = Tools::under(dir.path());
        for tool in [cli.sandbox(), cli.playwright_cli()] {
            fs::create_dir_all(tool.parent().unwrap()).unwrap();
            fs::write(tool, "").unwrap();
        }
        let run = ShotsCli::new(Tools::under(dir.path())).take(&ShotsJob {
            worktree,
            build: dir.path().join("build"),
            out: dir.path().join("out"),
            configuration: None,
            routes: vec![Route::try_from("/".to_owned()).unwrap()],
            domains: vec![],
            env: BTreeMap::new(),
        });
        assert_eq!(
            run.failed,
            Some(format!(
                "port {port} is already in use, so the shots would show another server"
            ))
        );
        assert!(tools.is_dir());
        assert!(!dir.path().join("out").exists(), "nothing ran");
    }

    #[test]
    fn missing_tools_say_how_to_install_them() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("wt");
        fs::create_dir_all(worktree.join(".claude")).unwrap();
        fs::write(
            worktree.join(".claude/launch.json"),
            r#"{"configurations": [{"name": "dev", "runtimeExecutable": "true", "port": 1}]}"#,
        )
        .unwrap();
        let run = ShotsCli::new(Tools::under(dir.path())).take(&ShotsJob {
            worktree,
            build: dir.path().join("build"),
            out: dir.path().join("out"),
            configuration: None,
            routes: vec![],
            domains: vec![],
            env: BTreeMap::new(),
        });
        let reason = run.failed.unwrap();
        assert!(
            reason.ends_with("is missing: run `kelpie tools install`"),
            "{reason}"
        );
    }
}
