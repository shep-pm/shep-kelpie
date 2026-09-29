//! The main seam's rig: a runner on stand-ins for Claude, the forge, the
//! webhook and the clock, over a real git repo in a throwaway home

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tempfile::TempDir;

use crate::board::WorkerModel;
use crate::ports::{
    Checks, Claude, ClaudeCall, ClaudeError, ClaudeReply, Clock, Cost, Finding, Meter, MeterError,
    Ports, Relay, Reviewer, ReviewerError, Role, SessionId, Timestamp, Usage, Utilization, Window,
};
use crate::runner::{
    CHECKS_SETTLE, OpenError, ProjectName, ProjectPaths, Runner, StepReport, answer, step,
};
use crate::settings::Effort;
use crate::webhook::{KelpieSettings, Webhook};
use crate::work_item::{CallRecord, Known, Phase, Turn, WorkItem};

mod alerts;
mod coderabbit;
mod forge;
mod leases;
mod relay;
mod shots;

pub(crate) use alerts::FakeAlerts;
pub(crate) use forge::FakeForge;
pub(crate) use leases::{FakeLeases, Told};
pub(crate) use relay::FakeRelay;
pub(crate) use shots::{FakeShots, ScriptedShots};

/// Writes an executable stand-in script that is safe to run at once.
///
/// On Linux a child forked while the file is still open for writing holds
/// it, and `exec` of it fails with ETXTBSY. Other tests fork all the time,
/// so this waits until the script has been executed once, with the probe
/// variable set, before handing it over. The script's first line after the
/// shebang exits on the probe, so the probe run does nothing.
pub(crate) fn write_script(path: &Path, contents: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let (shebang, rest) = contents.split_once('\n').expect("a script has a shebang");
    assert!(shebang.starts_with("#!"), "{shebang:?}");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(path)
        .unwrap();
    write!(
        file,
        "{shebang}\n[ -n \"$KELPIE_TEST_PROBE\" ] && exit 0\n{rest}"
    )
    .unwrap();
    drop(file);

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match Command::new(path).env("KELPIE_TEST_PROBE", "1").status() {
            Ok(_) => return,
            Err(e) if e.raw_os_error() == Some(26) && std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("cannot run the stand-in script {}: {e}", path.display()),
        }
    }
}

/// A work item with one call, so every field of its format shows
pub(crate) fn a_work_item() -> WorkItem {
    WorkItem {
        issue: 42,
        title: "Add a thing".into(),
        branch: "kelpie/42".into(),
        rework: false,
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
        conflict: Some(crate::work_item::Conflict {
            head: "c0ffee".into(),
            main: "a11ce".into(),
            turns: 1,
        }),
        resume: None,
        review_call: crate::work_item::ReviewCallState::Idle,
        coderabbit: crate::work_item::CodeRabbitTally::default(),
        known: Known {
            labels: vec!["review please".into()],
            ready: false,
            head: None,
        },
        qwen: crate::work_item::QwenTally::default(),
        shots: None,
        shots_comment: None,
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

/// The CodeRabbit gate as `settings.example.toml` sets it, and turned off
pub(crate) const CODERABBIT_ON: &str = "[coderabbit]\nenabled = true\n";
const CODERABBIT_OFF: &str = "[coderabbit]\nenabled = false\n";

/// A launch file like the playground's
const LAUNCH: &str = r#"{"version": "0.0.1", "configurations": [{"name": "dev", "runtimeExecutable": "bun", "runtimeArgs": ["run", "dev"], "port": 3000}]}"#;

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
    /// Merges `origin/main` into the worktree's branch, keeping main's
    /// side of any conflict, and pushes it without force, as a worker
    /// resolving a conflict does
    MergeMain,
    /// Answers with this exact text and no cost: a review round or judge
    /// one-shot, whose reply is read rather than acted on
    Text(&'static str),
    /// Answers like [`Self::Text`], with this cost for the session
    Billed(&'static str, Cost),
    /// Answers with this final message
    Say(&'static str),
    /// Blocks until the test releases it, then answers
    Hold(Hold),
}

/// A call in flight that a test lets go of when it chooses
#[derive(Debug, Clone, Default)]
pub(crate) struct Hold(Arc<(Mutex<Held>, Condvar)>);

#[derive(Debug, Default)]
struct Held {
    entered: bool,
    released: bool,
    returned: bool,
}

impl Hold {
    /// Waits up to `within` for the call to begin, and says whether it did
    pub(crate) fn entered(&self, within: Duration) -> bool {
        let (held, changed) = &*self.0;
        let held = held.lock().unwrap();
        let (held, _) = changed
            .wait_timeout_while(held, within, |h| !h.entered)
            .unwrap();
        held.entered
    }

    /// Lets the call answer
    pub(crate) fn release(&self) {
        let (held, changed) = &*self.0;
        held.lock().unwrap().released = true;
        changed.notify_all();
    }

    /// Whether the call has answered
    pub(crate) fn returned(&self) -> bool {
        self.0.0.lock().unwrap().returned
    }

    /// Waits up to `within` for the call to answer, and says whether it did
    pub(crate) fn answered(&self, within: Duration) -> bool {
        let (held, changed) = &*self.0;
        let held = held.lock().unwrap();
        let (held, _) = changed
            .wait_timeout_while(held, within, |h| !h.returned)
            .unwrap();
        held.returned
    }

    fn block(&self) {
        let (held, changed) = &*self.0;
        let mut held = held.lock().unwrap();
        held.entered = true;
        changed.notify_all();
        let mut held = changed.wait_while(held, |h| !h.released).unwrap();
        held.returned = true;
        changed.notify_all();
    }
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

    /// Queues answers for its next calls, oldest first
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
            Some(Scripted::Billed(text, cost)) => Ok(ClaudeReply {
                session_id: call.session.id().clone(),
                text: text.to_owned(),
                usage: Usage::default(),
                session_cost: cost,
            }),
            Some(Scripted::Say(text)) => Ok(ClaudeReply {
                session_id: call.session.id().clone(),
                text: text.into(),
                usage: Usage::default(),
                session_cost: Cost(0),
            }),
            Some(Scripted::Hold(hold)) => {
                hold.block();
                Ok(ClaudeReply {
                    session_id: call.session.id().clone(),
                    text: "done".into(),
                    usage: Usage::default(),
                    session_cost: Cost(0),
                })
            }
            Some(Scripted::Push(file, text)) => {
                let path = call.cwd.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
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
            Some(Scripted::MergeMain) => {
                git(&call.cwd, &["fetch", "--quiet", "origin", "main"]);
                git(
                    &call.cwd,
                    &[
                        "merge",
                        "--quiet",
                        "-X",
                        "theirs",
                        "--no-edit",
                        "origin/main",
                    ],
                );
                git(&call.cwd, &["push", "--quiet", "origin", "HEAD"]);
                Ok(ClaudeReply {
                    session_id: call.session.id().clone(),
                    text: "merged".into(),
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
    pub(crate) relay: Arc<FakeRelay>,
    pub(crate) alerts: FakeAlerts,
    pub(crate) leases: FakeLeases,
    pub(crate) shots: FakeShots,
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

    /// The rig's webhook URL: a credential, so finding it anywhere else is a leak
    pub(crate) const WEBHOOK_URL: &str = "https://alerts.example.invalid/hook/kelpie-s3cr3t";

    /// The part of [`Self::WEBHOOK_URL`] no output may carry
    pub(crate) const WEBHOOK_SECRET: &str = "s3cr3t";

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
            relay: Arc::new(FakeRelay::default()),
            alerts: FakeAlerts::default(),
            leases: FakeLeases::default(),
            shots: FakeShots::default(),
            clock: FakeClock::at(Self::EPOCH),
            home,
        };
        rig.make_repo();

        // CodeRabbit is off unless a test turns it on: most tests are about
        // what comes before it or does not involve it.
        let example = include_str!("../settings.example.toml");
        assert!(example.contains(EXAMPLE_REPO), "the example's repo moved");
        assert!(example.contains(CODERABBIT_ON), "the example's gate moved");
        let settings = example
            .replace(EXAMPLE_REPO, &rig.repo().display().to_string())
            .replace(CODERABBIT_ON, CODERABBIT_OFF);
        let paths = rig.paths();
        std::fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
        std::fs::write(&paths.settings, settings).unwrap();
        let kelpie = format!(
            "[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n",
            Self::WEBHOOK_URL
        );
        std::fs::write(&paths.kelpie_settings, kelpie).unwrap();
        rig
    }

    /// The webhook the rig's kelpie settings name
    pub(crate) fn webhook(&self) -> Webhook {
        KelpieSettings::load(&self.paths().kelpie_settings)
            .unwrap()
            .webhook
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
        self.land(file, "landed elsewhere\n")
    }

    fn land(&self, file: &str, text: &str) -> String {
        let other = self.home.path().join("other");
        if !other.exists() {
            git(
                self.home.path(),
                &["clone", "--quiet", path(&self.origin()), path(&other)],
            );
        }
        let at = other.join(file);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, text).unwrap();
        git(&other, &["add", file]);
        git(&other, &["commit", "--quiet", "-m", "landed elsewhere"]);
        git(&other, &["push", "--quiet", "origin", "main"]);
        git(&other, &["rev-parse", "HEAD"])
    }

    /// Lands `.claude/launch.json` on origin's `main`, as a repo with a
    /// preview carries it, and returns the commit
    pub(crate) fn land_launch_file(&self) -> String {
        self.land(crate::preview::LAUNCH_FILE, LAUNCH)
    }

    /// Pushes a commit of `file` to `branch` on origin from another clone,
    /// as the maintainer would by hand, and returns its hash
    pub(crate) fn push_by_hand(&self, branch: &str, file: &str) -> String {
        let hand = self.home.path().join("by-hand");
        if !hand.exists() {
            git(
                self.home.path(),
                &["clone", "--quiet", path(&self.origin()), path(&hand)],
            );
        }
        git(&hand, &["fetch", "--quiet", "origin"]);
        let start = match self.forge.head_of(branch) {
            Some(_) => format!("origin/{branch}"),
            None => "origin/main".to_owned(),
        };
        git(&hand, &["checkout", "--quiet", "-B", branch, &start]);
        std::fs::write(hand.join(file), "pushed by hand\n").unwrap();
        git(&hand, &["add", file]);
        git(&hand, &["commit", "--quiet", "-m", file]);
        git(&hand, &["push", "--quiet", "origin", branch]);
        git(&hand, &["rev-parse", "HEAD"])
    }

    /// Asserts the worker reads `path` under the settings of the call `seen`,
    /// and that a commit from its worktree would not carry it
    pub(crate) fn assert_worker_reads(&self, seen: &Seen, path: &Path) {
        assert!(
            !path.starts_with(&seen.call.cwd),
            "a commit would carry {path:?}"
        );
        // The worker's rules name kelpie's home as `~/.kelpie`.
        let home = self.home.path().join("kelpie");
        let as_written = format!("~/.kelpie/{}", path.strip_prefix(&home).unwrap().display());
        let as_is = path.to_str().unwrap();
        let deny = seen.settings["permissions"]["deny"].as_array().unwrap();
        for rule in deny.iter().map(|r| r.as_str().unwrap()) {
            assert!(
                !denies(rule, &as_written) && !denies(rule, as_is),
                "{rule} hides {as_written}"
            );
        }
        let deny_read = &seen.settings["sandbox"]["filesystem"]["denyRead"];
        for folder in deny_read.as_array().into_iter().flatten() {
            let folder = folder.as_str().unwrap();
            assert!(!path.starts_with(folder), "the sandbox hides it: {folder}");
        }
    }

    /// The project's checkout
    pub(crate) fn repo(&self) -> PathBuf {
        self.home.path().join("repos").join(self.project.as_str())
    }

    pub(crate) fn paths(&self) -> ProjectPaths {
        ProjectPaths::under(&self.home.path().join("kelpie"), &self.project)
    }

    /// Turns the CodeRabbit gate on, as the example settings have it for shep
    pub(crate) fn coderabbit_on(&self) {
        self.edit_settings(|s| s.replace(CODERABBIT_OFF, CODERABBIT_ON));
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
            relay: Arc::clone(&self.relay) as Arc<dyn Relay>,
            alerts: Arc::new(self.alerts.clone()),
            leases: Arc::new(self.leases.clone()),
            shots: Arc::new(self.shots.clone()),
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
        self.paths().worktree(7)
    }

    /// The build folder kelpie makes for issue 7
    pub(crate) fn build_7(&self) -> PathBuf {
        self.paths().build(7)
    }
}

// Whether a `Read(...)` deny rule covers `path`, written as the rule writes it
fn denies(rule: &str, path: &str) -> bool {
    let Some(glob) = rule.strip_prefix("Read(").and_then(|r| r.strip_suffix(')')) else {
        return false;
    };
    match glob.strip_suffix("**") {
        Some(folder) if !folder.contains('*') => path.starts_with(folder),
        None if !glob.contains('*') => path == glob,
        _ => panic!("teach this test to read {rule}"),
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
