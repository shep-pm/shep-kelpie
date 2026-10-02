//! The main seam's rig: a runner on stand-ins for Claude, the forge, the
//! webhook and the clock, over a real git repo in a throwaway home

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use crate::adapters::{GpuCurl, LocalReviewer};
use crate::board::WorkerModel;
use crate::coderabbit::CodeRabbit;
use crate::codex::Codex;
use crate::cubic::Cubic;
use crate::ports::{
    Checks, Clock, Cost, Meter, MeterError, Ports, Relay, Role, SessionId, Timestamp, Usage,
    Utilization, Window,
};
use crate::review_bot::Profile;
use crate::runner::{
    CHECKS_SETTLE, OpenError, ProjectName, ProjectPaths, Runner, StepReport, answer, step,
};
use crate::settings::{Effort, Settings, SettingsError};
use crate::webhook::{KelpieSettings, Webhook};
use crate::work_item::{CallRecord, Known, Phase, Seconds, TimingPhase, Timings, Turn, WorkItem};

mod alerts;
mod claude;
mod coderabbit;
mod codex;
mod cubic;
mod endpoint;
mod forge;
mod leases;
mod relay;
mod reviewer;
mod sandbox;
mod script;
mod shepherd;
mod shots;

pub(crate) use alerts::FakeAlerts;
pub(crate) use claude::{FakeClaude, Hold, LEFT_BEHIND, Scripted, Seen};
pub(crate) use endpoint::{Answer, StandInEndpoint, unreachable_url};
pub(crate) use forge::FakeForge;
pub(crate) use leases::{FakeLeases, Told};
pub(crate) use relay::FakeRelay;
pub(crate) use reviewer::{FakeReviewer, ScriptedRound};
pub(crate) use sandbox::OpenSandbox;
pub(crate) use script::write_script;
pub(crate) use shepherd::FakeShepherd;
pub(crate) use shots::{FakeShots, ScriptedShots};

/// A work item with one call, so every field of its format shows
pub(crate) fn a_work_item() -> WorkItem {
    WorkItem {
        issue: 42,
        title: "Add a thing".into(),
        branch: "kelpie/42".into(),
        rework: false,
        adopted: false,
        arrived: None,
        worktree: "/k/wt/shep/42".into(),
        build: "/k/targets/shep/42".into(),
        worker: WorkerModel {
            model: "claude-opus-5-5".into(),
            effort: Effort::Medium,
            local: false,
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
        claude_files_accepted: None,
        qwen: crate::work_item::QwenTally::default(),
        merge_refused: false,
        sent_back: false,
        asked_to_commit: false,
        merge_tried: None,
        merge_queued: None,
        summon_owed: false,
        local_rounds: 0,
        local_failures: Default::default(),
        rebased: false,
        shots: None,
        shots_comment: None,
        held: Vec::new(),
        follow_ups: None,
        timings: Some(Timings {
            created: Timestamp(5),
            since: Timestamp(12),
            seconds: Seconds::of(&[(TimingPhase::Worker, 4), (TimingPhase::Ci, 3)]),
            call: None,
            queued: false,
        }),
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
            unpriced: false,
        }],
    }
}

/// The `[app.dogs.kelpie]` table of a runner's Flockfile entry, such as
/// `settings.example.toml`, as shep hands it to the runner
pub(crate) fn project_table(entry: &str) -> serde_json::Map<String, serde_json::Value> {
    let parsed: toml::Table = toml::from_str(entry).unwrap();
    match serde_json::to_value(&parsed["app"][0]["dogs"]["kelpie"]).unwrap() {
        serde_json::Value::Object(table) => table,
        other => panic!("the entry's kelpie table is {other}"),
    }
}

/// A runner entry like `settings.example.toml` with `tables` added to its
/// kelpie table, such as an older `[app.dogs.kelpie.review.local]`
pub(crate) fn with_tables(entry: &str, tables: &str) -> String {
    const GATE: &str = "\n[app.dogs.kelpie.coderabbit]\n";
    assert!(entry.contains(GATE), "the example's CodeRabbit table moved");
    entry.replace(GATE, &format!("\n{tables}{GATE}"))
}

/// The `repo` in `settings.example.toml`, which the rig points at its own
const EXAMPLE_REPO: &str = "~/GitHub/shep";

/// The CodeRabbit gate as `settings.example.toml` sets it, and turned off
pub(crate) const CODERABBIT_ON: &str = "[app.dogs.kelpie.coderabbit]\nenabled = true\n";
pub(crate) const CODERABBIT_OFF: &str = "[app.dogs.kelpie.coderabbit]\nenabled = false\n";
/// Planning turned on, and off as `settings.example.toml` sets it
pub(crate) const PLANNING_ON: &str = "[app.dogs.kelpie.planning]\nenabled = true\n";
pub(crate) const PLANNING_OFF: &str = "[app.dogs.kelpie.planning]\nenabled = false\n";

/// A launch file like the playground's
const LAUNCH: &str = r#"{"version": "0.0.1", "configurations": [{"name": "dev", "runtimeExecutable": "bun", "runtimeArgs": ["run", "dev"], "port": 3000}]}"#;

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
    pub(crate) fn idle() -> Self {
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

/// One project's world: kelpie's home, the maintainer's home, and the
/// project's repo cloned from a bare origin
#[derive(Debug)]
pub(crate) struct Rig {
    pub(crate) home: TempDir,
    pub(crate) project: ProjectName,
    pub(crate) claude: FakeClaude,
    pub(crate) forge: FakeForge,
    pub(crate) meter: FakeMeter,
    pub(crate) codex_meter: FakeMeter,
    pub(crate) reviewer: FakeReviewer,
    pub(crate) local_leases: LocalReviewer,
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
        let clock = FakeClock::at(Self::EPOCH);
        let rig = Self {
            project: ProjectName::try_from(project).unwrap(),
            claude: FakeClaude::metered(meter.clone()),
            forge: FakeForge::new(home.path().join("origin.git")),
            meter,
            codex_meter: FakeMeter::idle(),
            reviewer: FakeReviewer::default(),
            local_leases: LocalReviewer::default().with_temp_dir(home.path().join("tmp")),
            relay: Arc::new(FakeRelay::default()),
            alerts: FakeAlerts::on(clock.clone()),
            leases: FakeLeases::default(),
            shots: FakeShots::default(),
            clock,
            home,
        };
        rig.make_repo();

        // CodeRabbit is off unless a test turns it on, and planning is off as
        // the example has it: most tests are about what comes before them.
        let example = include_str!("../settings.example.toml");
        assert!(example.contains(EXAMPLE_REPO), "the example's repo moved");
        assert!(example.contains(CODERABBIT_ON), "the example's gate moved");
        assert!(
            example.contains(PLANNING_OFF),
            "the example's planning moved"
        );
        let settings = example
            .replace(EXAMPLE_REPO, &rig.repo().display().to_string())
            .replace(CODERABBIT_ON, CODERABBIT_OFF);
        let paths = rig.paths();
        std::fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
        std::fs::write(&paths.settings, settings).unwrap();
        // The example's local round, which the runner checks is there as it starts.
        let script = rig.home.path().join(".claude/scripts/qwen-review.sh");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        write_script(&script, "#!/bin/sh\nexit 1\n");
        let kelpie = format!(
            "[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n",
            Self::WEBHOOK_URL
        );
        std::fs::write(&paths.kelpie_settings, kelpie).unwrap();
        rig.write_totp_secret();
        rig
    }

    /// The webhook the rig's kelpie settings name
    pub(crate) fn webhook(&self) -> Webhook {
        KelpieSettings::load(&self.paths().kelpie_settings)
            .unwrap()
            .webhook
            .expect("the rig's kelpie settings name a webhook")
    }

    /// Kelpie's own settings as the rig's file holds them, empty when it is gone
    pub(crate) fn kelpie_settings(&self) -> KelpieSettings {
        self.try_kelpie_settings().unwrap()
    }

    fn try_kelpie_settings(&self) -> Result<KelpieSettings, SettingsError> {
        match std::fs::read_to_string(self.paths().kelpie_settings) {
            Ok(text) => KelpieSettings::from_section(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KelpieSettings::default()),
            Err(e) => panic!("cannot read the rig's kelpie settings: {e}"),
        }
    }

    /// Replaces the rig's kelpie settings file with `text`
    pub(crate) fn set_kelpie_settings(&self, text: &str) {
        std::fs::write(self.paths().kelpie_settings, text).unwrap();
    }

    /// Sets the project's `ruling_channels`, as its settings file would
    pub(crate) fn set_ruling_channels(&self, list: &str) {
        let table = "[app.dogs.kelpie]\n";
        self.edit_settings(|s| {
            assert!(s.contains(table), "the rig's entry has no kelpie table");
            s.replacen(table, &format!("{table}ruling_channels = {list}\n"), 1)
        });
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

    /// Lands `file` holding `text` on origin's `main`, and returns the commit
    pub(crate) fn land(&self, file: &str, text: &str) -> String {
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
        // The worker's rules name the old home as `~/.kelpie`.
        let home = self.home.path().join("shep/kelpie");
        let as_written = format!("~/.kelpie/{}", path.strip_prefix(&home).unwrap().display());
        let as_is = path.to_str().unwrap();
        let deny = seen.settings["permissions"]["deny"].as_array().unwrap();
        for rule in deny.iter().map(|r| r.as_str().unwrap()) {
            assert!(
                !denies(rule, &as_written) && !denies(rule, as_is),
                "{rule} hides {as_written}"
            );
        }
    }

    /// The project's checkout
    pub(crate) fn repo(&self) -> PathBuf {
        self.home.path().join("repos").join(self.project.as_str())
    }

    pub(crate) fn paths(&self) -> ProjectPaths {
        // Kelpie's home under the shepherd's, as the runner works it out.
        let shep = self.home.path().join("shep");
        ProjectPaths::under(&shep.join("kelpie"), &shep, &self.project)
    }

    /// Turns the CodeRabbit gate on, as the example settings have it for shep
    pub(crate) fn coderabbit_on(&self) {
        self.edit_settings(|s| s.replace(CODERABBIT_OFF, CODERABBIT_ON));
    }

    /// Turns planning on, which the example settings leave off
    pub(crate) fn planning_on(&self) {
        self.edit_settings(|s| s.replace(PLANNING_OFF, PLANNING_ON));
    }

    /// Makes the project's merge authority `auto`, read when a runner next opens
    pub(crate) fn merge_auto(&self) {
        let (ask, auto) = ("merge_authority = \"ask\"", "merge_authority = \"auto\"");
        self.edit_settings(|s| {
            assert!(s.contains(ask), "the example's merge authority moved");
            s.replace(ask, auto)
        });
    }

    pub(crate) fn edit_settings(&self, edit: impl FnOnce(String) -> String) {
        let file = self.paths().settings;
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, edit(text)).unwrap();
    }

    /// The project's settings as its table now stands
    pub(crate) fn settings(&self) -> Settings {
        self.try_settings().unwrap()
    }

    // The rig keeps the runner's Flockfile entry where the old settings file was.
    fn try_settings(&self) -> Result<Settings, SettingsError> {
        let paths = self.paths();
        let entry = std::fs::read_to_string(&paths.settings).unwrap();
        let folder = paths.settings.parent().unwrap();
        let (project, home) = (self.project.as_str(), self.home.path());
        Settings::from_table(&project_table(&entry), project, home, folder)
    }

    /// Starts a runner, as a restarted sheep would, on the rig's stand-ins
    pub(crate) fn open(&self) -> Result<Mutex<Runner>, OpenError> {
        self.open_with(vec![Arc::new(CodeRabbit), Arc::new(Cubic), Arc::new(Codex)])
    }

    /// Starts a runner whose review bot rounds summon the bots of `review_bots`
    pub(crate) fn open_with(
        &self,
        review_bots: Vec<Arc<dyn Profile>>,
    ) -> Result<Mutex<Runner>, OpenError> {
        let ports = Ports {
            agents: Arc::new(self.claude.clone()),
            forge: Box::new(self.forge.clone()),
            meter: Box::new(self.meter.clone()),
            codex_meter: Box::new(self.codex_meter.clone()),
            reviewer: Arc::new(self.reviewer.clone()),
            gpu: Arc::new(GpuCurl),
            local_leases: Arc::new(self.local_leases.clone()),
            review_bots,
            relay: Arc::clone(&self.relay) as Arc<dyn Relay>,
            alerts: Arc::new(self.alerts.clone()),
            leases: Arc::new(self.leases.clone()),
            shots: Arc::new(self.shots.clone()),
            clock: Box::new(self.clock.clone()),
        };
        let paths = self.paths();
        Runner::open(
            self.project.clone(),
            self.try_settings()?,
            self.try_kelpie_settings()?,
            &paths,
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
        Self::with_pull_request_set(project, |_| {})
    }

    /// [`Rig::with_pull_request`], after `setup` changed the rig's settings
    pub(crate) fn with_pull_request_set(
        project: &str,
        setup: impl FnOnce(&Self),
    ) -> (Self, Mutex<Runner>, String) {
        let rig = Self::new(project);
        setup(&rig);
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
        Self::parked_set(project, |_| {})
    }

    /// [`Rig::parked`], after `setup` changed the rig's settings
    pub(crate) fn parked_set(
        project: &str,
        setup: impl FnOnce(&Self),
    ) -> (Self, Mutex<Runner>, String) {
        let (rig, runner, head) = Self::with_pull_request_set(project, setup);
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

pub(crate) fn write_in(folder: &Path, file: &str, text: &str) {
    let path = folder.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
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
