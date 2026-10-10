//! The main seam's rig: a runner on stand-ins for Claude, the forge, the
//! webhook and the clock, over a real git repo in a throwaway home

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;

use crate::adapters::{GpuCurl, LocalReviewer};
use crate::coderabbit::CodeRabbit;
use crate::codex::Codex;
use crate::cubic::Cubic;
use crate::ports::{
    Checks, Clock, Cost, Meter, MeterError, Ports, Role, SessionId, Timestamp, Usage, Utilization,
    Window,
};
use crate::review_bot::Profile;
use crate::runner::{
    CHECKS_SETTLE, OpenError, ProjectName, ProjectPaths, Runner, SETTLE_LEAST, StepReport, answer,
    step,
};
use crate::settings::{Settings, SettingsError};
use crate::webhook::{KelpieSettings, Webhook};
use crate::work_item::{CallRecord, Known, Phase, Seconds, TimingPhase, Timings, Turn, WorkItem};

mod alerts;
mod claude;
mod coderabbit;
mod codex;
mod cubic;
mod elsewhere;
mod endpoint;
mod forge;
mod leases;
mod reviewer;
mod sandbox;
mod script;
mod shepherd;

pub(crate) use alerts::FakeAlerts;
pub(crate) use claude::{FakeClaude, Hold, LEFT_BEHIND, Scripted, Seen};
pub(crate) use elsewhere::Elsewhere;
pub(crate) use endpoint::{Answer, StandInEndpoint, Take, unreachable_url};
pub(crate) use forge::FakeForge;
pub(crate) use leases::{FakeLeases, Told};
pub(crate) use reviewer::{FakeReviewer, ScriptedRound};
pub(crate) use sandbox::OpenSandbox;
pub(crate) use script::write_script;
pub(crate) use shepherd::FakeShepherd;

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
        agent: "opus-high".to_owned().try_into().unwrap(),
        pinned: true,
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
        bot_reads: Default::default(),
        bots_skipped: Vec::new(),
        known: Known {
            labels: vec!["review please".into()],
            ready: false,
            head: None,
        },
        reviewed_heads: Vec::new(),
        sent_unread: Vec::new(),
        nit_fix_heads: Vec::new(),
        noted_from: None,
        late_from: None,
        claude_files_accepted: None,
        qwen: crate::work_item::QwenTally::default(),
        merge_refused: false,
        sent_back: false,
        asked_to_commit: false,
        merge_tried: None,
        merge_queued: None,
        summon_owed: false,
        summons_owed: Default::default(),
        bots_after_ci: false,
        seat: crate::work_item::Seat::Held,
        threads_sent: Vec::new(),
        resolve_failures: 0,
        reviewers_skipped: Vec::new(),
        unreviewed: None,
        local_failures: Default::default(),
        local_unreviewed: Vec::new(),
        local_unreviewed_by: None,
        rebased: false,
        held: Vec::new(),
        follow_ups: None,
        summary: None,
        timings: Some(Timings {
            created: Timestamp(5),
            since: Timestamp(12),
            seconds: Seconds::of(&[(TimingPhase::Worker, 4), (TimingPhase::Ci, 3)]),
            call: None,
            queued: false,
        }),
        attached: None,
        counts: crate::work_item::Counts {
            review_rounds: 2,
            fix_turns: 1,
            rulings: 1,
            ci_fix_turns: 1,
            worker_unreported: 0,
            reviewer_unreported: 0,
        },
        calls: vec![CallRecord {
            role: Role::Worker,
            at: Timestamp(10),
            session: SessionId("5e55".into()),
            usage: Usage {
                input: 1,
                cache_write: 2,
                cache_write_5m: 0,
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
/// kelpie table, such as an older `[app.dogs.kelpie.review]`
pub(crate) fn with_tables(entry: &str, tables: &str) -> String {
    const PACING: &str = "\n[app.dogs.kelpie.pacing]\n";
    assert!(entry.contains(PACING), "the example's pacing table moved");
    entry.replace(PACING, &format!("\n{tables}{PACING}"))
}

/// The `git.checkout` in `settings.example.toml`, which the rig points at its own
const EXAMPLE_REPO: &str = "~/GitHub/shep";

/// The reviewers `settings.example.toml` lists, commented out
const EXAMPLE_REVIEWERS: &str = "# reviewers = [\"qwen\", \"defect-hunter\"]\n";

/// The rig's reviewers: its qwen round and then its own Claude session, `claude`
pub(crate) const RIG_REVIEWERS: &str = "reviewers = [\"qwen\", \"claude\"]\n";

/// The rig's own session reviewer, as the older Claude round ran: Sonnet 5
/// at medium, asked for findings in kelpie's format
pub(crate) const CLAUDE_REVIEWER: &str = "---\nrole: reviewer\nharness: claude-code\n\
model: claude-sonnet-5\neffort: medium\n---\nReview the change against {{BASE}} for \
defects. Output one finding per line as SEVERITY|file:line|what|why, or exactly CLEAN.\n";

/// The rig's reviewers with CodeRabbit read last
pub(crate) const RIG_REVIEWERS_AND_CODERABBIT: &str =
    "reviewers = [\"qwen\", \"claude\", \"coderabbit\"]\n";

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
    pub(crate) alerts: FakeAlerts,
    pub(crate) leases: FakeLeases,
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
            // A held lock is looked at again soon, so a test waits little.
            local_leases: LocalReviewer::default()
                .with_temp_dir(home.path().join("tmp"))
                .with_naps(|_| Duration::from_millis(100)),
            alerts: FakeAlerts::on(clock.clone()),
            leases: FakeLeases::default(),
            clock,
            home,
        };
        rig.make_repo();

        let example = include_str!("../settings.example.toml");
        assert!(example.contains(EXAMPLE_REPO), "the example's repo moved");
        // Most tests are about what comes after the review, so the rig's
        // project runs a short one: a local round and then a Claude session. A
        // test of a new project's review takes that out with [`Rig::default_review`].
        assert!(
            example.contains(EXAMPLE_REVIEWERS),
            "the example's reviewers moved"
        );
        let settings = example
            .replace(EXAMPLE_REPO, &rig.repo().display().to_string())
            .replace(EXAMPLE_REVIEWERS, RIG_REVIEWERS);
        let paths = rig.paths();
        std::fs::create_dir_all(&paths.folder).unwrap();
        std::fs::write(rig.settings_file(), settings).unwrap();
        rig.write_agent("claude", CLAUDE_REVIEWER);
        // The maintainer's qwen-review script, which the runner checks is there as it starts.
        let script = rig.home.path().join(".claude/scripts/qwen-review.sh");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        write_script(&script, "#!/bin/sh\nexit 1\n");
        let kelpie = format!(
            "[webhook]\nkind = \"ntfy\"\nurl = \"{}\"\n",
            Self::WEBHOOK_URL
        );
        std::fs::write(rig.kelpie_settings_file(), kelpie).unwrap();
        rig.write_totp_secret();
        rig
    }

    /// The webhook the rig's kelpie settings name
    pub(crate) fn webhook(&self) -> Webhook {
        self.kelpie_settings()
            .webhook
            .expect("the rig's kelpie settings name a webhook")
    }

    /// Kelpie's own settings as the rig's file holds them, empty when it is gone
    pub(crate) fn kelpie_settings(&self) -> KelpieSettings {
        self.try_kelpie_settings().unwrap()
    }

    fn try_kelpie_settings(&self) -> Result<KelpieSettings, SettingsError> {
        match std::fs::read_to_string(self.kelpie_settings_file()) {
            Ok(text) => KelpieSettings::from_section(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KelpieSettings::default()),
            Err(e) => panic!("cannot read the rig's kelpie settings: {e}"),
        }
    }

    /// Replaces the rig's kelpie settings file with `text`
    pub(crate) fn set_kelpie_settings(&self, text: &str) {
        std::fs::write(self.kelpie_settings_file(), text).unwrap();
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

    /// The file the rig keeps the project's table in, as the Flockfile entry's `[app.dogs.kelpie]`
    pub(crate) fn settings_file(&self) -> PathBuf {
        self.paths().folder.join("settings.toml")
    }

    /// The file the rig keeps kelpie's `[kelpie]` section in
    pub(crate) fn kelpie_settings_file(&self) -> PathBuf {
        self.paths().kelpie_home.join("settings.toml")
    }

    pub(crate) fn paths(&self) -> ProjectPaths {
        // Kelpie's home under the shepherd's, as the runner works it out.
        let shep = self.home.path().join("shep");
        ProjectPaths::under(&shep.join("kelpie"), &shep, &self.project)
    }

    /// Lists CodeRabbit after the rig's reviewers, so it reads each pass last
    pub(crate) fn coderabbit_on(&self) {
        self.edit_settings(|s| {
            assert!(s.contains(RIG_REVIEWERS), "the rig's reviewers moved");
            s.replace(RIG_REVIEWERS, RIG_REVIEWERS_AND_CODERABBIT)
        });
    }

    /// Makes the project's `git.merging` `auto`, read when a runner next opens
    pub(crate) fn merge_auto(&self) {
        let (ask, auto) = ("merging = \"ask\"", "merging = \"auto\"");
        self.edit_settings(|s| {
            assert!(s.contains(ask), "the example's `git.merging` moved");
            s.replace(ask, auto)
        });
    }

    /// Sets the project's `git.issues` to `filing`, read when a runner next opens
    pub(crate) fn issues(&self, filing: &str) {
        let ask = "issues = \"ask\"";
        self.edit_settings(|s| {
            assert!(s.contains(ask), "the example's `git.issues` moved");
            s.replace(ask, &format!("issues = \"{filing}\""))
        });
    }

    /// Sets the project's `ci.fix_attempts` to `cap`, read when a runner next opens
    pub(crate) fn fix_attempts(&self, cap: i64) {
        let none = "fix_attempts = -1";
        self.edit_settings(|s| {
            assert!(s.contains(none), "the example's `ci.fix_attempts` moved");
            s.replace(none, &format!("fix_attempts = {cap}"))
        });
    }

    /// Writes kelpie's agent file `name.md` holding `text`, read when a runner next opens
    pub(crate) fn write_agent(&self, name: &str, text: &str) {
        write_in(&self.paths().agents, &format!("{name}.md"), text);
    }

    /// Lists `names` as the project's implementers, read when a runner next opens
    pub(crate) fn implementers(&self, names: &[&str]) {
        let listed = format!("implementers = {names:?}");
        self.edit_settings(|s| {
            assert!(
                s.contains("\nimplementers = "),
                "the example's implementers moved"
            );
            let lines = s
                .lines()
                .map(|line| match line.starts_with("implementers = ") {
                    true => listed.as_str(),
                    false => line,
                });
            lines.map(|line| format!("{line}\n")).collect()
        });
    }

    /// Makes the project a new one, which lists no reviewers: the qwen round,
    /// since the rig has the script, and then `defect-hunter`
    pub(crate) fn default_review(&self) {
        self.edit_settings(|s| s.replace(RIG_REVIEWERS, ""));
    }

    /// Lists `names` as the project's reviewers, read when a runner next opens
    pub(crate) fn reviewers(&self, names: &[&str]) {
        let listed = format!("reviewers = {names:?}\n");
        self.edit_settings(|s| {
            assert!(s.contains(RIG_REVIEWERS), "the rig's reviewers moved");
            s.replace(RIG_REVIEWERS, &listed)
        });
    }

    pub(crate) fn edit_settings(&self, edit: impl FnOnce(String) -> String) {
        let file = self.settings_file();
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
        let entry = std::fs::read_to_string(self.settings_file()).unwrap();
        let folder = &paths.folder;
        let (project, home) = (self.project.as_str(), self.home.path());
        Settings::from_table(&project_table(&entry), project, home, folder)
    }

    /// Starts a runner, as a restarted sheep would, on the rig's stand-ins
    pub(crate) fn open(&self) -> Result<Mutex<Runner>, OpenError> {
        self.open_with(vec![Arc::new(CodeRabbit), Arc::new(Cubic), Arc::new(Codex)])
    }

    /// Starts a runner that reads each review bot by its profile in `review_bots`
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
            alerts: Arc::new(self.alerts.clone()),
            leases: Arc::new(self.leases.clone()),
            clock: Arc::new(self.clock.clone()),
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
    /// Its review ran each reviewer once, a clean qwen round and then a clean
    /// Claude round, so the gate tests all start where the gate itself
    /// begins: the worker's pull request open and its one pass over.
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

    /// Moves the clock on to the board's next read, past the first three
    /// waits of a step that failed
    pub(crate) fn next_look(&self) {
        self.clock.advance(crate::runner::BOARD_POLL.as_secs());
    }

    /// Steps once and, if nothing happened, waits out CI's settling and
    /// steps again: how a CI verdict is reached
    pub(crate) fn verdict(&self, runner: &Mutex<Runner>) -> Option<StepReport> {
        let report = step(runner).unwrap();
        if report.is_some() {
            return report;
        }
        self.clock.advance(CHECKS_SETTLE);
        step(runner).unwrap()
    }

    /// Steps a bot's round from the review it covers the head with: the
    /// step that sees the review reads its threads, and the one
    /// [`SETTLE_LEAST`] on reads them again and lands the round
    pub(crate) fn threads_read(&self, runner: &Mutex<Runner>) -> Option<StepReport> {
        assert_eq!(step(runner).unwrap(), None, "the threads read once");
        self.clock.advance(SETTLE_LEAST);
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

/// A repo at `home/repo` with one commit on `main`, and a worktree of it at
/// `home/wt` on its own branch, the way kelpie cuts one
pub(crate) fn linked_worktree(home: &Path) -> (PathBuf, PathBuf) {
    let repo = home.join("repo");
    let worktree = home.join("wt");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "-b", "main"]);
    git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "init"]);
    git(
        &repo,
        &["worktree", "add", "--quiet", "-b", "wt", path(&worktree)],
    );
    (repo, worktree)
}
