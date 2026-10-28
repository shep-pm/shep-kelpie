//! The main seam's rig: a runner on stand-ins for Claude, the forge, the
//! webhook and the clock, over a real git repo in a throwaway home

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::adapters::{GpuCurl, LocalReviewer};
use crate::coderabbit::CodeRabbit;
use crate::codex::Codex;
use crate::cubic::Cubic;
use crate::github::AppVoice;
use crate::home::{ProjectName, ProjectPaths};
use crate::ports::{
    Agents, CLEAN, CallPacer, Checks, Clock, Forge, HarnessMcp, LocalPace, Pace, Ports, Tickets,
    Timestamp, Utilization,
};
use crate::review_bot::Profile;
use crate::runner::{
    CHECKS_SETTLE, OpenError, Pass, Runner, SETTLE_LEAST, StepReport, advance, answer, step,
};
use crate::settings::SettingsError;
use crate::state::StateError;
use crate::webhook::{KelpieSettings, Webhook};

mod claude;
mod fork;
mod held_git;
mod home;
mod reads;
mod registered_app;
mod rig_settings;
mod rig_totp;
mod script_cli;

// The stand-ins and helpers that name only `core` and the leaves, where `core`'s own tests use them
pub use claude::{FakeClaude, LEFT_BEHIND, Scripted, Seen};
// The stand-ins that name an adapter
pub use held_git::HeldGit;
pub use reads::{
    assert_granted, assert_reads, assert_refuses, installed_sources, sandbox_refuses,
    source_folders, tools_refuse,
};
pub use registered_app::RegisteredApp;
pub use script_cli::ScriptCli;
pub use shep_kelpie_adapters::test::{
    Asked, DogStandIn, FakeGithub, FakeLeases, FakeReviewer, HeldToken, REVIEW_ID, ScriptedRound,
    SeenRound, SentReview, Told,
};
pub use shep_kelpie_core::test::{
    Answer, EPOCH, FakeAlerts, FakeBridges, FakeClassifier, FakeClock, FakeDevServers, FakeDisk,
    FakeE2e, FakeForge, FakeGit, FakeMeter, FakePages, FakeShepherd, FakeTests, GB, HeldCall, Hold,
    Home, LOGIN_TOKEN, LeaseReads, Memory, OpenSandbox, RecordedSink, SeenAsk, StandInEndpoint,
    Synced, Take, VIEWER, a_work_item, clean_visit, project_table, ticket, unreachable_url,
};
pub use shep_kelpie_test_support::{
    CALL_PATIENCE, Elsewhere, Fake, cut_from_main, git, linked_worktree, main_commits, make_fifo,
    short_tempdir, waits_for, write_fake, write_in, write_script,
};

/// A read a test holds open: a forge call, a token mint or a git fetch
pub trait Held: Send {
    /// Whether a read reached the stand-in and waits there, within `within`
    fn waiting(&self, within: std::time::Duration) -> bool;
}

impl Held for HeldCall {
    fn waiting(&self, within: std::time::Duration) -> bool {
        Self::waiting(self, within)
    }
}

impl Held for HeldGit {
    fn waiting(&self, within: std::time::Duration) -> bool {
        Self::waiting(self, within)
    }
}

impl Held for HeldToken {
    fn waiting(&self, within: std::time::Duration) -> bool {
        Self::waiting(self, within)
    }
}

/// The workflow the rig's repo holds on `main`
const WORKFLOW: &str = ".github/workflows/ci.yml";

/// The `git.checkout` in `settings.example.toml`, which the rig points at its own
const EXAMPLE_REPO: &str = "~/GitHub/shep";

/// The reviewers `settings.example.toml` lists, commented out
const EXAMPLE_REVIEWERS: &str = "# reviewers = [\"iq3-xxs-review\", \"defect-hunter-lite\"]\n";

/// The rig's reviewers: its qwen round and then its own Claude session, `claude`
pub const RIG_REVIEWERS: &str = "reviewers = [\"qwen\", \"claude\"]\n";

/// What the rig adds to its settings to run no retro
pub const NO_RETRO: &str = "\n[app.dogs.kelpie.skills]\nretro = { kind = \"none\" }\n";

// What [`Rig::retro_on`] puts in its place, a retro being off when left out
const RETRO_ON: &str = "\n[app.dogs.kelpie.skills]\nretro = { kind = \"kelpie\" }\n";

/// The rig's own session reviewer, as the older Claude round ran: Sonnet 5
/// at medium, asked for findings in kelpie's format
pub const CLAUDE_REVIEWER: &str = "---\nrole: reviewer\nharness: claude-code\n\
model: claude-sonnet-5\neffort: medium\n---\nReview the change against {{BASE}} for \
defects. Output one finding per line as SEVERITY|file:line|what|why, or exactly CLEAN.\n";

/// The rig's reviewers with CodeRabbit read last
pub const RIG_REVIEWERS_AND_CODERABBIT: &str =
    "reviewers = [\"qwen\", \"claude\", \"coderabbit\"]\n";

/// One project's world: kelpie's home, the maintainer's home, and the
/// project's repo cloned from a bare origin
#[derive(Debug)]
pub struct Rig {
    /// The throwaway home the project's folders are under
    pub home: Home,
    /// The project's name
    pub project: ProjectName,
    /// The Claude Code stand-in, scripted a reply per call
    pub claude: FakeClaude,
    /// The forge and the project's tickets
    pub forge: FakeForge,
    /// The clones the runner asks git for
    pub git: FakeGit,
    /// The Claude usage meter
    pub meter: FakeMeter,
    /// The Codex usage meter
    pub codex_meter: FakeMeter,
    /// The local reviewer's rounds
    pub reviewer: FakeReviewer,
    /// The real local reviewer, which the rig's runner keeps its rounds in flight in
    pub local_leases: LocalReviewer,
    // The gateway's leases, answered with nothing and counted, in place of the
    // real read, for a test that says who asked
    counted_leases: Arc<Mutex<Option<Arc<LeaseReads>>>>,
    /// The alerts sent to the maintainer
    pub alerts: FakeAlerts,
    /// The disk the runner reads free space from
    pub disk: FakeDisk,
    /// The lease book the runner asks
    pub leases: FakeLeases,
    /// The project's test runs
    pub tests: FakeTests,
    /// The dev servers the runner starts
    pub dev_servers: FakeDevServers,
    /// The e2e runs on the e2e host
    pub e2e: FakeE2e,
    /// The pages the browser visits
    pub pages: FakePages,
    /// The bridges the runner opens for a worker
    pub bridges: FakeBridges,
    /// What the harnesses say of MCP servers: the real facts, until a test swaps in its own
    pub harness_mcp: Mutex<Arc<dyn HarnessMcp>>,
    /// The samples the runner records
    pub samples: RecordedSink,
    /// The gateway classifier triage's spam check asks
    pub classifier: FakeClassifier,
    /// The clock every stand-in and the runner read
    pub clock: FakeClock,
    /// The login's forge hold and pace, which no test's calls reach the cap of
    /// until it swaps in a pacer with one
    pub pace: Arc<dyn Pace>,
    /// GitHub as kelpie's App sees it: the rig has registered no App until
    /// [`Rig::with_app`]
    pub github: FakeGithub,
}

impl Rig {
    /// Where every rig's clock starts
    pub const EPOCH: u64 = EPOCH;

    /// One day, in seconds
    pub const DAY: u64 = shep_kelpie_core::test::DAY;

    /// Usage with `week` and `session` percent spent. The week began at
    /// [`Self::EPOCH`] and the session window resets five hours on.
    pub fn utilization(week: u32, session: u32) -> Utilization {
        shep_kelpie_core::test::utilization(week, session)
    }

    /// The repo the rig's project works on GitHub
    pub const REMOTE: &str = "shep-pm/shep";

    /// Where the rig says the kelpie binary is
    pub const KELPIE: &str = "/opt/kelpie/bin/kelpie";

    /// The rig's webhook URL: a credential, so finding it anywhere else is a leak
    pub const WEBHOOK_URL: &str = "https://alerts.example.invalid/hook/kelpie-s3cr3t";

    /// The part of [`Self::WEBHOOK_URL`] no output may carry
    pub const WEBHOOK_SECRET: &str = "s3cr3t";

    /// A project with the example settings, pointed at a fresh repo
    pub fn new(project: &str) -> Self {
        let rig = Self::stand_ins(project, Home::made());
        rig.make_repo();

        let example = include_str!("../../../../settings.example.toml");
        assert!(example.contains(EXAMPLE_REPO), "the example's repo moved");
        // Most tests are about what comes after the review, so the rig's
        // project runs a short one: a local round and then a Claude session. A
        // test of another lineup names it with [`Rig::reviewers`].
        assert!(
            example.contains(EXAMPLE_REVIEWERS),
            "the example's reviewers moved"
        );
        let settings = example
            .replace(EXAMPLE_REPO, &rig.repo().display().to_string())
            .replace(EXAMPLE_REVIEWERS, RIG_REVIEWERS);
        // A retro is one more call after every batch, so most tests
        // leave it off and a test of the retro turns it on with [`Rig::retro_on`].
        let settings = format!("{settings}{NO_RETRO}");
        // No reviewer reads a head before CI has passed on it, and a run has
        // two minutes to register, so most tests run the project without CI
        // and a test of the gate turns it on with [`Rig::ci_on`].
        let settings = settings.replace("block = true", "block = false");
        let paths = rig.paths();
        std::fs::create_dir_all(&paths.folder).unwrap();
        std::fs::write(rig.settings_file(), settings).unwrap();
        rig.write_agent("claude", CLAUDE_REVIEWER);
        // Kelpie's own agents bind nothing, so each runs as the file the maintainer binds.
        crate::agents::write_bound_stubs(
            &paths.agents,
            "harness: claude-code\nmodel: claude-opus-5-5\neffort: medium\n",
        );
        rig.write_agent(
            "defect-hunter",
            "---\nextends: kelpie:defect-hunter\nharness: claude-code\nmodel: claude-opus-5-5\n\
             effort: high\nsecond_look: true\n---\n",
        );
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
    pub fn webhook(&self) -> Webhook {
        self.kelpie_settings()
            .webhook
            .expect("the rig's kelpie settings name a webhook")
    }

    /// Kelpie's own settings as the rig's file holds them, empty when it is gone
    pub fn kelpie_settings(&self) -> KelpieSettings {
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
    pub fn set_kelpie_settings(&self, text: &str) {
        std::fs::write(self.kelpie_settings_file(), text).unwrap();
    }

    /// Gives kelpie a gateway named `name` at `url`, in its own settings, read
    /// when a runner next opens. Its key's variable is `CARGO_PKG_NAME`, which
    /// every test run has set.
    pub fn gateway(&self, name: &str, url: &str) {
        let text = std::fs::read_to_string(self.kelpie_settings_file()).unwrap();
        let table = format!("[gateways.{name}]\nurl = \"{url}\"\nkey_env = \"CARGO_PKG_NAME\"\n");
        std::fs::write(self.kelpie_settings_file(), format!("{text}{table}")).unwrap();
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
        // Checks appear on a pull request only where a workflow runs on one.
        let workflow = self.repo().join(WORKFLOW);
        std::fs::create_dir_all(workflow.parent().unwrap()).unwrap();
        std::fs::write(&workflow, "on: pull_request\njobs:\n  test:\n").unwrap();
        git(&self.repo(), &["add", "README.md", WORKFLOW]);
        git(&self.repo(), &["commit", "--quiet", "-m", "first"]);
        git(&self.repo(), &["push", "--quiet", "origin", "main"]);
    }

    fn origin(&self) -> PathBuf {
        self.home.path().join("origin.git")
    }

    /// Has origin's `main` hold a workflow that runs on a push to `main` alone, so
    /// no check ever appears on a pull request
    pub fn without_pull_request_ci(&self) {
        let on_main = "on:\n  push:\n    branches: [main]\njobs:\n  test:\n";
        self.land(WORKFLOW, on_main);
    }

    /// Lands a commit on origin's `main` from another clone, as a merge
    /// elsewhere would, and returns its hash
    pub fn land_on_origin(&self, file: &str) -> String {
        self.land(file, "landed elsewhere\n")
    }

    /// Lands `file` holding `text` on origin's `main`, and returns the commit
    pub fn land(&self, file: &str, text: &str) -> String {
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
    pub fn push_by_hand(&self, branch: &str, file: &str) -> String {
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
    pub fn assert_worker_reads(&self, seen: &Seen, path: &Path) {
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
    pub fn repo(&self) -> PathBuf {
        self.home.path().join("repos").join(self.project.as_str())
    }

    /// The file the rig keeps the project's table in, as the Flockfile entry's `[app.dogs.kelpie]`
    pub fn settings_file(&self) -> PathBuf {
        self.paths().folder.join("settings.toml")
    }

    /// The file the rig keeps kelpie's `[kelpie]` section in
    pub fn kelpie_settings_file(&self) -> PathBuf {
        self.paths().kelpie_home.join("settings.toml")
    }

    /// The project's paths under the rig's homes
    pub fn paths(&self) -> ProjectPaths {
        // Kelpie's home under the shepherd's, as the runner works it out.
        let shep = self.home.path().join("shep");
        ProjectPaths::under(&shep.join("kelpie"), &shep, &self.project)
    }

    /// Starts a runner, as a restarted sheep would, on the rig's stand-ins
    pub fn open(&self) -> Result<Mutex<Runner>, OpenError> {
        self.open_with(vec![Arc::new(CodeRabbit), Arc::new(Cubic), Arc::new(Codex)])
    }

    /// Has the runners this rig starts ask `facts` what a harness says of an MCP server
    pub fn harness_mcp_is(&self, facts: Arc<dyn HarnessMcp>) {
        *self.harness_mcp.lock().unwrap() = facts;
    }

    /// Starts a runner that reads each review bot by its profile in `review_bots`
    pub fn open_with(
        &self,
        review_bots: Vec<Arc<dyn Profile>>,
    ) -> Result<Mutex<Runner>, OpenError> {
        self.open_with_agents(review_bots, Arc::new(self.claude.clone()))
    }

    /// Starts a runner whose calls go to `agents` rather than the stand-in Claude
    pub fn open_agents(&self, agents: Arc<dyn Agents>) -> Result<Mutex<Runner>, OpenError> {
        let bots: Vec<Arc<dyn Profile>> =
            vec![Arc::new(CodeRabbit), Arc::new(Cubic), Arc::new(Codex)];
        self.open_with_agents(bots, agents)
    }

    fn open_with_agents(
        &self,
        review_bots: Vec<Arc<dyn Profile>>,
        agents: Arc<dyn Agents>,
    ) -> Result<Mutex<Runner>, OpenError> {
        let tokens = Arc::new(self.github.tokens(&self.paths().kelpie_home));
        let forge = Arc::new(self.forge.clone());
        let paths = self.paths();
        let ports = Ports {
            agents,
            forge: Arc::clone(&forge) as Arc<dyn Forge>,
            tickets: Arc::clone(&forge) as Arc<dyn Tickets>,
            app_reads: Some(Arc::new(RegisteredApp::under(&paths.kelpie_home))),
            revalidated: crate::ports::Revalidated::default(),
            voice: Arc::new(AppVoice::new(tokens, Box::new(self.github.clone()))),
            meter: Arc::new(self.meter.clone()),
            codex_meter: Arc::new(self.codex_meter.clone()),
            reviewer: Arc::new(self.reviewer.clone()),
            git: Arc::new(self.git.clone()),
            gpu: Arc::new(GpuCurl),
            disk: Arc::new(self.disk.clone()),
            local_leases: self.lease_port(),
            review_bots,
            alerts: Arc::new(self.alerts.clone()),
            leases: Arc::new(self.leases.clone()),
            test_runner: Arc::new(self.tests.clone()),
            dev_servers: Arc::new(self.dev_servers.clone()),
            e2e_runner: Arc::new(self.e2e.clone()),
            pages: Arc::new(self.pages.clone()),
            bridges: Arc::new(self.bridges.clone()),
            harness_mcp: Arc::clone(&self.harness_mcp.lock().unwrap()),
            metrics: Arc::new(self.samples.clone()),
            classifier: Arc::new(self.classifier.clone()),
            clock: Arc::new(self.clock.clone()),
            pace: Arc::clone(&self.pace),
            flock: Vec::new(),
        };
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
    pub fn ask(
        &self,
        runner: &Mutex<Runner>,
        action: &str,
        params: Option<&str>,
    ) -> serde_json::Value {
        let postings = runner.lock().unwrap().postings();
        let reply = answer(runner, &postings, action, params);
        runner.lock().unwrap().settle_notices();
        serde_json::from_str(&reply).expect("a JSON reply")
    }

    /// Runs `run` until its forge call or git fetch `read` is held open, does `meanwhile`, and lets the read go
    ///
    /// Returns what `meanwhile` returned, or `None` when it had not finished within
    /// [`CALL_PATIENCE`], as a trigger that waits for a lock the read holds has not, and what
    /// `run` returned.
    pub fn during_read<R: Send, T: Send>(
        &self,
        read: impl Held,
        run: impl FnOnce() -> R + Send,
        meanwhile: impl FnOnce() -> T + Send,
    ) -> (Option<T>, R) {
        std::thread::scope(|scope| {
            let running = scope.spawn(run);
            assert!(
                read.waiting(CALL_PATIENCE),
                "the run never reached the read"
            );
            let (sent, heard) = std::sync::mpsc::channel();
            let doing = scope.spawn(move || sent.send(meanwhile()));
            let done = heard.recv_timeout(CALL_PATIENCE).ok();
            drop(read);
            let _ = doing.join();
            (done, running.join().expect("the run's thread"))
        })
    }

    /// Runs a pass of `runner` until its forge call `read` is held open and asks `action` of it
    /// meanwhile, a trigger that waits for the lock
    ///
    /// Returns the reply, or `None` when the runner did not answer within [`CALL_PATIENCE`], and
    /// the pass's own result.
    pub fn asked_while_held(
        &self,
        runner: &Mutex<Runner>,
        read: impl Held,
        action: &str,
        params: Option<&str>,
    ) -> (Option<serde_json::Value>, Result<Pass, StateError>) {
        let ask = || self.ask(runner, action, params);
        self.during_read(read, || advance(runner), ask)
    }

    /// A running project whose worker's first turn pushed `work.txt` on
    /// `kelpie/7` and opened draft pull request 71, with CI not yet reported
    ///
    /// Its review ran each reviewer once, a clean qwen round and then a clean
    /// Claude round, so the gate tests all start where the gate itself
    /// begins: the worker's pull request open and its one pass over.
    ///
    /// Returns the pull request's head.
    pub fn with_pull_request(project: &str) -> (Self, Mutex<Runner>, String) {
        Self::with_pull_request_set(project, |_| {})
    }

    /// [`Rig::with_pull_request`], after `setup` changed the rig's settings
    pub fn with_pull_request_set(
        project: &str,
        setup: impl FnOnce(&Self),
    ) -> (Self, Mutex<Runner>, String) {
        Self::with_pull_request_worked(project, setup, Scripted::Push("work.txt", "work\n"))
    }

    /// [`Rig::with_pull_request_set`], the worker's first turn being `work`
    pub fn with_pull_request_worked(
        project: &str,
        setup: impl FnOnce(&Self),
        work: Scripted,
    ) -> (Self, Mutex<Runner>, String) {
        let rig = Self::new(project);
        setup(&rig);
        let runner = rig.open().unwrap();
        rig.ask(&runner, "add", Some("7"));
        rig.forge.open_pull_request(71, "kelpie/7", &[7]);
        rig.claude.script([work, Scripted::Text(CLEAN)]);
        step(&runner).unwrap(); // the worker's first turn: opens the pull request
        step(&runner).unwrap(); // review round 1, qwen: clean by default
        step(&runner).unwrap(); // review round 2, claude: scripted clean above
        let head = rig.forge.head_of("kelpie/7").expect("the worker pushed");
        // The gate begins here, so CI blocks from now on.
        rig.ci_on_now(&runner);
        (rig, runner, head)
    }

    /// [`Rig::with_pull_request`], with CI green and the worker parked on
    /// merge ruling 71 (the pull request's number) about the returned head
    pub fn parked(project: &str) -> (Self, Mutex<Runner>, String) {
        Self::parked_set(project, |_| {})
    }

    /// [`Rig::parked`], after `setup` changed the rig's settings
    pub fn parked_set(project: &str, setup: impl FnOnce(&Self)) -> (Self, Mutex<Runner>, String) {
        let (rig, runner, head) = Self::with_pull_request_set(project, setup);
        rig.forge.set_checks(&head, Checks::Passed);
        assert!(matches!(
            rig.verdict(&runner),
            Some(StepReport::Ruling { id, .. }) if id == ticket(71)
        ));
        (rig, runner, head)
    }

    /// `login` writes `body` on `thread` now, and the comment's id
    pub fn writes_on(&self, thread: u64, login: &str, body: &str) -> u64 {
        (self.forge.threads).write(thread, login, body, self.clock.now())
    }

    /// Moves the clock on to the board's next read, past the first three
    /// waits of a step that failed and the longest poll of an idle board
    pub fn next_look(&self) {
        self.clock.advance(crate::runner::IDLE_POLL_MAX.as_secs());
    }

    /// Moves the clock on to the next poll of a board that is not idle
    ///
    /// A `tell` to a project with no project manager is refused and changes
    /// nothing but ends the backoff of an idle board's poll, as any trigger
    /// that asks for work does.
    pub fn next_poll(&self, runner: &Mutex<Runner>) {
        self.ask(runner, "tell", Some("is anyone there?"));
        self.clock.advance(crate::runner::BOARD_POLL.as_secs());
    }

    /// Steps once and, if nothing happened, waits out CI's settling and
    /// steps again: how a CI verdict is reached
    pub fn verdict(&self, runner: &Mutex<Runner>) -> Option<StepReport> {
        let report = step(runner).unwrap();
        if report.is_some() {
            return report;
        }
        self.clock.advance(CHECKS_SETTLE);
        step(runner).unwrap()
    }

    /// Steps a bot's round from the review it covers the head with: the
    /// step that sees the review reads its threads, and the one
    /// `SETTLE_LEAST` on reads them again and lands the round
    pub fn threads_read(&self, runner: &Mutex<Runner>) -> Option<StepReport> {
        assert_eq!(step(runner).unwrap(), None, "the threads read once");
        self.clock.advance(SETTLE_LEAST);
        step(runner).unwrap()
    }

    /// The worktree kelpie makes for issue 7
    pub fn worktree_7(&self) -> PathBuf {
        self.paths().worktree(ticket(7))
    }

    /// The build folder kelpie makes for issue 7
    pub fn build_7(&self) -> PathBuf {
        self.paths().build(ticket(7))
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
