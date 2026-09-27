//! The main seam's rig: a runner on stand-ins for Claude, the forge and the
//! clock, over a real git repo in a throwaway home

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use crate::board::WorkerModel;
use crate::ports::{
    Checks, Claude, ClaudeCall, ClaudeError, ClaudeReply, Clock, Cost, Finding, Meter, MeterError,
    Ports, Reviewer, ReviewerError, Role, SessionId, Timestamp, Usage, Utilization, Window,
};
use crate::runner::{
    CHECKS_SETTLE, OpenError, ProjectName, ProjectPaths, Runner, StepReport, answer, step,
};
use crate::settings::Effort;
use crate::work_item::{CallRecord, Phase, Turn, WorkItem};

mod forge;

pub(crate) use forge::FakeForge;

/// A work item with one call, so every field of its format shows
pub(crate) fn a_work_item() -> WorkItem {
    WorkItem {
        issue: 42,
        title: "Add a thing".into(),
        branch: "kelpie/42".into(),
        worktree: "/k/wt/shep/42".into(),
        build: "/k/targets/shep/42".into(),
        worker: WorkerModel {
            model: "claude-opus-5-5".into(),
            effort: Effort::Medium,
        },
        session: SessionId("5e55".into()),
        turn: Turn::Running {
            since: Timestamp(9),
        },
        pull_request: Some(51),
        phase: Phase::Ci {
            head: Some("c0ffee".into()),
            since: Timestamp(11),
        },
        red_head: Some("bad".into()),
        calls: vec![CallRecord {
            role: Role::Worker,
            at: Timestamp(10),
            session: SessionId("5e55".into()),
            usage: Usage {
                input: 1,
                cache_write: 2,
                cache_read: 3,
                output: 4,
            },
            cost: Cost(5),
            session_cost: Cost(6),
        }],
    }
}

/// The `repo` in `settings.example.toml`, which the rig points at its own
const EXAMPLE_REPO: &str = "~/.kelpie/repos/shep";

/// The file a killed worker leaves in its worktree, to find after a restart
pub(crate) const LEFT_BEHIND: &str = "left-behind.txt";

/// What the stand-in Claude does with its next call
#[derive(Debug, Clone)]
pub(crate) enum Scripted {
    /// Answers with this usage, and this cost for the session so far
    Reply(Usage, Cost),
    /// Answers like [`Self::Reply`], with the account's usage as given by
    /// the time it does: the call itself spent it
    Spend(Utilization, Usage, Cost),
    /// Fails with this error
    Fail(ClaudeError),
    /// Leaves [`LEFT_BEHIND`] in the worktree, then dies with the runner
    Kill,
    /// Commits this file with this text on the worktree's branch, pushes
    /// it the way a worker does, and answers
    Push(&'static str, &'static str),
    /// Answers with this exact text and no cost: a review round or judge
    /// one-shot, whose reply is read rather than acted on
    Text(&'static str),
}

/// A call as the stand-in Claude saw it
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    /// The call
    pub(crate) call: ClaudeCall,
    /// The settings file it named, as it stood during the call
    pub(crate) settings: serde_json::Value,
    /// Whether its build folder existed when the call started
    pub(crate) build_existed: bool,
}

/// Records every call and answers from a script, failing once it runs out
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeClaude {
    seen: Arc<Mutex<Vec<Seen>>>,
    script: Arc<Mutex<VecDeque<Scripted>>>,
    meter: Option<FakeMeter>,
}

impl FakeClaude {
    /// The worker's own calls, in order: what every test before the review
    /// loop existed already asserted on, so a reviewer or judge call never
    /// shows up and shifts their counts.
    pub(crate) fn calls(&self) -> Vec<ClaudeCall> {
        self.seen().into_iter().map(|s| s.call).collect()
    }

    /// The worker's own calls, with the settings file each one saw
    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.all_seen()
            .into_iter()
            .filter(|s| s.call.role == Role::Worker)
            .collect()
    }

    /// Every call, worker, reviewer and judge alike, in order
    pub(crate) fn all_calls(&self) -> Vec<ClaudeCall> {
        self.all_seen().into_iter().map(|s| s.call).collect()
    }

    /// Every call, with the settings file each one saw
    pub(crate) fn all_seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub(crate) fn script(&self, steps: impl IntoIterator<Item = Scripted>) {
        self.script.lock().unwrap().extend(steps);
    }
}

impl Claude for FakeClaude {
    fn run(&self, call: &ClaudeCall) -> Result<ClaudeReply, ClaudeError> {
        let settings: serde_json::Value = std::fs::read_to_string(&call.settings)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let build = settings["env"]["CARGO_TARGET_DIR"].as_str().map(Path::new);
        let build_existed = build.is_some_and(Path::is_dir);
        self.seen.lock().unwrap().push(Seen {
            call: call.clone(),
            settings,
            build_existed,
        });
        let next = self.script.lock().unwrap().pop_front();
        let next = match next {
            Some(Scripted::Spend(account, usage, cost)) => {
                if let Some(meter) = &self.meter {
                    meter.set(account);
                }
                Some(Scripted::Reply(usage, cost))
            }
            other => other,
        };
        match next {
            Some(Scripted::Reply(usage, session_cost)) => Ok(ClaudeReply {
                session_id: call.session.id().clone(),
                text: "done".into(),
                usage,
                session_cost,
            }),
            Some(Scripted::Spend(..)) => unreachable!("turned into a reply above"),
            Some(Scripted::Fail(error)) => Err(error),
            Some(Scripted::Kill) => {
                std::fs::write(call.cwd.join(LEFT_BEHIND), "work in progress\n").unwrap();
                panic!("the runner is killed mid-turn");
            }
            Some(Scripted::Text(text)) => Ok(ClaudeReply {
                session_id: call.session.id().clone(),
                text: text.to_owned(),
                usage: Usage::default(),
                session_cost: Cost(0),
            }),
            Some(Scripted::Push(file, text)) => {
                std::fs::write(call.cwd.join(file), text).unwrap();
                git(&call.cwd, &["add", file]);
                git(&call.cwd, &["commit", "--quiet", "-m", file]);
                git(&call.cwd, &["push", "--quiet", "origin", "HEAD"]);
                Ok(ClaudeReply {
                    session_id: call.session.id().clone(),
                    text: "pushed".into(),
                    usage: Usage::default(),
                    session_cost: Cost(0),
                })
            }
            None => Err(ClaudeError::Failed("the rig scripts no reply".into())),
        }
    }
}

/// A meter that reports what a test sets, and counts its reads
///
/// It starts with the account idle: nothing used in either window, the
/// week `Rig::EPOCH` begins and the session window five hours long.
#[derive(Debug, Clone)]
pub(crate) struct FakeMeter {
    reading: Arc<Mutex<Result<Utilization, MeterError>>>,
    reads: Arc<AtomicUsize>,
}

impl FakeMeter {
    fn idle() -> Self {
        Self {
            reading: Arc::new(Mutex::new(Ok(Rig::utilization(0, 0)))),
            reads: Arc::default(),
        }
    }

    /// Reports `usage` from now on
    pub(crate) fn set(&self, usage: Utilization) {
        *self.reading.lock().unwrap() = Ok(usage);
    }

    /// Fails every read with `error`, until a test sets a reading
    pub(crate) fn fail(&self, error: MeterError) {
        *self.reading.lock().unwrap() = Err(error);
    }

    /// How many times usage was read
    pub(crate) fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl Meter for FakeMeter {
    fn read(&self, _now: Timestamp) -> Result<Utilization, MeterError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.reading.lock().unwrap().clone()
    }
}

/// A clock that moves only when a test moves it
#[derive(Debug, Clone)]
pub(crate) struct FakeClock(Arc<AtomicU64>);

impl FakeClock {
    pub(crate) fn at(seconds: u64) -> Self {
        Self(Arc::new(AtomicU64::new(seconds)))
    }

    pub(crate) fn advance(&self, seconds: u64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.0.load(Ordering::SeqCst))
    }
}

/// What the stand-in reviewer answers for its next round
#[derive(Debug, Clone)]
pub(crate) enum ScriptedRound {
    /// These findings
    Findings(Vec<Finding>),
    /// Fails with this error
    Fail(ReviewerError),
}

/// One round as the stand-in reviewer saw it
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeenRound {
    pub(crate) worktree: PathBuf,
    pub(crate) out: PathBuf,
    pub(crate) round: u32,
}

/// A qwen-review stand-in. Clean (no findings) once its script runs out, so
/// tests that do not care about the review loop see it pass straight through.
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeReviewer {
    seen: Arc<Mutex<Vec<SeenRound>>>,
    script: Arc<Mutex<VecDeque<ScriptedRound>>>,
}

impl FakeReviewer {
    /// Every round asked of it, in order
    pub(crate) fn seen(&self) -> Vec<SeenRound> {
        self.seen.lock().unwrap().clone()
    }

    /// Queues answers for its next rounds, oldest first
    pub(crate) fn script(&self, rounds: impl IntoIterator<Item = ScriptedRound>) {
        self.script.lock().unwrap().extend(rounds);
    }
}

impl Reviewer for FakeReviewer {
    fn round(
        &self,
        worktree: &Path,
        out: &Path,
        round: u32,
    ) -> Result<Vec<Finding>, ReviewerError> {
        self.seen.lock().unwrap().push(SeenRound {
            worktree: worktree.to_owned(),
            out: out.to_owned(),
            round,
        });
        match self.script.lock().unwrap().pop_front() {
            Some(ScriptedRound::Findings(findings)) => Ok(findings),
            Some(ScriptedRound::Fail(e)) => Err(e),
            None => Ok(Vec::new()),
        }
    }
}

/// One project's world: kelpie's home, the maintainer's home, and the
/// project's repo cloned from a bare origin
#[derive(Debug)]
pub(crate) struct Rig {
    pub(crate) home: TempDir,
    pub(crate) project: ProjectName,
    pub(crate) claude: FakeClaude,
    pub(crate) forge: FakeForge,
    pub(crate) meter: FakeMeter,
    pub(crate) reviewer: FakeReviewer,
    pub(crate) clock: FakeClock,
}

impl Rig {
    /// Where every rig's clock starts
    pub(crate) const EPOCH: u64 = 1_790_000_000;

    /// One day, in seconds
    pub(crate) const DAY: u64 = 86_400;

    /// Usage with `week` and `session` percent spent. The week began at
    /// [`Self::EPOCH`] and the session window resets five hours on.
    pub(crate) fn utilization(week: u32, session: u32) -> Utilization {
        Utilization {
            session: Window {
                used_pct: session,
                resets_at: Timestamp(Self::EPOCH + 5 * 3600),
            },
            week: Window {
                used_pct: week,
                resets_at: Timestamp(Self::EPOCH + 7 * Self::DAY),
            },
        }
    }

    /// Where the rig says the kelpie binary is
    pub(crate) const KELPIE: &str = "/opt/kelpie/bin/kelpie";

    /// A project with the example settings, pointed at a fresh repo
    pub(crate) fn new(project: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let meter = FakeMeter::idle();
        let rig = Self {
            project: ProjectName::try_from(project).unwrap(),
            claude: FakeClaude {
                meter: Some(meter.clone()),
                ..FakeClaude::default()
            },
            forge: FakeForge::new(home.path().join("origin.git")),
            meter,
            reviewer: FakeReviewer::default(),
            clock: FakeClock::at(Self::EPOCH),
            home,
        };
        rig.make_repo();

        let example = include_str!("../settings.example.toml");
        assert!(example.contains(EXAMPLE_REPO), "the example's repo moved");
        let settings = example.replace(EXAMPLE_REPO, &rig.repo().display().to_string());
        let paths = rig.paths();
        std::fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
        std::fs::write(&paths.settings, settings).unwrap();
        rig
    }

    fn make_repo(&self) {
        let root = self.home.path();
        let origin = self.origin();
        git(
            root,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                path(&origin),
            ],
        );
        git(
            root,
            &["clone", "--quiet", path(&origin), path(&self.repo())],
        );
        std::fs::write(self.repo().join("README.md"), "a project\n").unwrap();
        git(&self.repo(), &["add", "README.md"]);
        git(&self.repo(), &["commit", "--quiet", "-m", "first"]);
        git(&self.repo(), &["push", "--quiet", "origin", "main"]);
    }

    fn origin(&self) -> PathBuf {
        self.home.path().join("origin.git")
    }

    /// Lands a commit on origin's `main` from another clone, as a merge
    /// elsewhere would, and returns its hash
    pub(crate) fn land_on_origin(&self, file: &str) -> String {
        let other = self.home.path().join("other");
        if !other.exists() {
            git(
                self.home.path(),
                &["clone", "--quiet", path(&self.origin()), path(&other)],
            );
        }
        std::fs::write(other.join(file), "landed elsewhere\n").unwrap();
        git(&other, &["add", file]);
        git(&other, &["commit", "--quiet", "-m", "landed elsewhere"]);
        git(&other, &["push", "--quiet", "origin", "main"]);
        git(&other, &["rev-parse", "HEAD"])
    }

    /// The project's checkout
    pub(crate) fn repo(&self) -> PathBuf {
        self.home.path().join("repos").join(self.project.as_str())
    }

    pub(crate) fn paths(&self) -> ProjectPaths {
        ProjectPaths::under(&self.home.path().join("kelpie"), &self.project)
    }

    pub(crate) fn edit_settings(&self, edit: impl FnOnce(String) -> String) {
        let file = self.paths().settings;
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, edit(text)).unwrap();
    }

    /// Starts a runner, as a restarted sheep would, on the rig's stand-ins
    pub(crate) fn open(&self) -> Result<Mutex<Runner>, OpenError> {
        let ports = Ports {
            claude: Arc::new(self.claude.clone()),
            forge: Box::new(self.forge.clone()),
            meter: Box::new(self.meter.clone()),
            reviewer: Arc::new(self.reviewer.clone()),
            clock: Box::new(self.clock.clone()),
        };
        Runner::open(
            self.project.clone(),
            &self.paths(),
            self.home.path(),
            Path::new(Self::KELPIE),
            ports,
        )
        .map(Mutex::new)
    }

    /// Sends a trigger the way `shep trigger <project> <action>` does
    pub(crate) fn ask(
        &self,
        runner: &Mutex<Runner>,
        action: &str,
        params: Option<&str>,
    ) -> serde_json::Value {
        serde_json::from_str(&answer(runner, action, params)).expect("a JSON reply")
    }

    /// A running project whose worker's first turn pushed `work.txt` on
    /// `kelpie/7` and opened draft pull request 71, with CI not yet reported
    ///
    /// Its qwen-review loop ran two clean rounds first, so the gate tests
    /// all start where the gate itself begins: the worker's pull request
    /// open and its review settled.
    ///
    /// Returns the pull request's head.
    pub(crate) fn with_pull_request(project: &str) -> (Self, Mutex<Runner>, String) {
        let rig = Self::new(project);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "start", None);
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([
            Scripted::Push("work.txt", "work\n"),
            Scripted::Text("CLEAN"),
        ]);
        step(&runner).unwrap(); // the worker's first turn: opens the pull request
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
        (rig, runner, head)
    }

    /// [`Rig::with_pull_request`], with CI green and the worker parked on
    /// merge ruling 1 about the returned head
    pub(crate) fn parked(project: &str) -> (Self, Mutex<Runner>, String) {
        let (rig, runner, head) = Self::with_pull_request(project);
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id: 1, .. })
        ));
        (rig, runner, head)
    }

    /// Steps once and, if nothing happened, waits out CI's settling and
    /// steps again: how a CI verdict is reached
    pub(crate) fn verdict(&self, runner: &Mutex<Runner>) -> Option<StepReport> {
        if let Some(report) = step(runner).unwrap() {
            return Some(report);
        }
        self.clock.advance(CHECKS_SETTLE);
        step(runner).unwrap()
    }

    /// The worktree kelpie makes for issue 7
    pub(crate) fn worktree_7(&self) -> PathBuf {
        self.home
            .path()
            .join("kelpie/wt")
            .join(self.project.as_str())
            .join("7")
    }
}

fn path(p: &Path) -> &str {
    p.to_str().expect("a UTF-8 temporary path")
}

/// Runs git in `cwd` and returns its trimmed stdout
///
/// The maintainer's own git config never reaches the rig: no signing, no hooks.
pub(crate) fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=rig",
            "-c",
            "user.email=rig@example.invalid",
        ])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "git {args:?} in {}: {stderr}",
        cwd.display()
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
