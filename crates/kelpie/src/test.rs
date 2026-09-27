//! The main seam's rig: a runner on stand-ins for Claude, the forge and the
//! clock, over a real git repo in a throwaway home

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use crate::board::{OpenPullRequest, READY, ReadyIssue, WorkerModel};
use crate::ports::{
    Checks, Claude, ClaudeCall, ClaudeError, ClaudeReply, Clock, Cost, Forge, ForgeError, Issue,
    Ports, PullRequest, PullRequestState, Role, SessionId, Timestamp, Usage, Visibility,
};
use crate::runner::{OpenError, ProjectName, ProjectPaths, Runner, answer};
use crate::settings::{Effort, ForgeSlug};
use crate::work_item::{CallRecord, Turn, WorkItem};

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
    /// Fails with this error
    Fail(ClaudeError),
    /// Leaves [`LEFT_BEHIND`] in the worktree, then dies with the runner
    Kill,
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
}

impl FakeClaude {
    pub(crate) fn calls(&self) -> Vec<ClaudeCall> {
        self.seen().into_iter().map(|s| s.call).collect()
    }

    pub(crate) fn seen(&self) -> Vec<Seen> {
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
        match next {
            Some(Scripted::Reply(usage, session_cost)) => Ok(ClaudeReply {
                session_id: call.session.id().clone(),
                text: "done".into(),
                usage,
                session_cost,
            }),
            Some(Scripted::Fail(error)) => Err(error),
            Some(Scripted::Kill) => {
                std::fs::write(call.cwd.join(LEFT_BEHIND), "work in progress\n").unwrap();
                panic!("the runner is killed mid-turn");
            }
            None => Err(ClaudeError::Failed("the rig scripts no reply".into())),
        }
    }
}

/// A forge whose repo is public and whose every issue exists, unless a
/// test says otherwise. Its board is empty until a test lists issues on it.
#[derive(Debug, Clone)]
pub(crate) struct FakeForge {
    visibility: Arc<Mutex<Visibility>>,
    missing: Arc<Mutex<HashSet<u64>>>,
    labels: Arc<Mutex<HashMap<u64, Vec<String>>>>,
    ready: Arc<Mutex<Vec<ReadyIssue>>>,
    open: Arc<Mutex<Vec<OpenPullRequest>>>,
    board_down: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    origin: PathBuf,
    pull_requests: Arc<Mutex<HashMap<u64, FakePullRequest>>>,
    checks: Arc<Mutex<HashMap<String, Checks>>>,
    comments: Arc<Mutex<Vec<(u64, String)>>>,
    comments_down: Arc<AtomicBool>,
    readied: Arc<Mutex<Vec<u64>>>,
    merges: Arc<Mutex<Vec<(u64, String)>>>,
}

/// A pull request on the fake forge. Its head is its branch on the rig's
/// origin, so a push the runner makes moves it.
#[derive(Debug, Clone)]
struct FakePullRequest {
    branch: String,
    state: PullRequestState,
    draft: bool,
}

impl FakeForge {
    pub(crate) fn set_visibility(&self, visibility: Visibility) {
        *self.visibility.lock().unwrap() = visibility;
    }

    pub(crate) fn remove_issue(&self, number: u64) {
        self.missing.lock().unwrap().insert(number);
    }

    /// Labels issue `number` with `label`, on the board and when viewed
    pub(crate) fn label(&self, number: u64, label: &str) {
        let mut labels = self.labels.lock().unwrap();
        labels.entry(number).or_default().push(label.to_owned());
    }

    /// Lists issue `number` as ready, with whether anyone is assigned
    pub(crate) fn list_ready(&self, number: u64, assigned: bool) {
        self.label(number, READY);
        self.ready.lock().unwrap().push(ReadyIssue {
            number,
            assigned,
            labels: Vec::new(),
        });
    }

    /// Opens draft pull request `number` from `head`, closing `closes`
    pub(crate) fn open_pull_request(&self, number: u64, head: &str, closes: &[u64]) {
        self.open.lock().unwrap().push(OpenPullRequest {
            number,
            head: head.to_owned(),
            closes: closes.to_vec(),
        });
        let pr = FakePullRequest {
            branch: head.to_owned(),
            state: PullRequestState::Open,
            draft: true,
        };
        self.pull_requests.lock().unwrap().insert(number, pr);
    }

    /// Reports `checks` for commit `head`. A head with none reported is pending.
    pub(crate) fn set_checks(&self, head: &str, checks: Checks) {
        self.checks.lock().unwrap().insert(head.to_owned(), checks);
    }

    /// Merges or closes pull request `number` as someone other than kelpie would
    pub(crate) fn set_state(&self, number: u64, state: PullRequestState) {
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("an opened pull request").state = state;
    }

    /// Makes posting comments fail, or work again
    pub(crate) fn set_comments_down(&self, down: bool) {
        self.comments_down.store(down, Ordering::SeqCst);
    }

    /// Every comment posted, oldest first, with its pull request
    pub(crate) fn comments(&self) -> Vec<(u64, String)> {
        self.comments.lock().unwrap().clone()
    }

    /// Every pull request marked ready, in order
    pub(crate) fn readied(&self) -> Vec<u64> {
        self.readied.lock().unwrap().clone()
    }

    /// Every merge made, with the head it was held to
    pub(crate) fn merges(&self) -> Vec<(u64, String)> {
        self.merges.lock().unwrap().clone()
    }

    /// The commit `branch` points at on origin, if it exists there
    pub(crate) fn head_of(&self, branch: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(&self.origin)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/heads/{branch}"))
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn opened(&self, number: u64) -> Result<FakePullRequest, ForgeError> {
        let prs = self.pull_requests.lock().unwrap();
        let pr = prs.get(&number).cloned();
        pr.ok_or_else(|| ForgeError::Failed(format!("no pull request #{number}")))
    }

    /// Makes listing the board fail, or work again
    pub(crate) fn set_board_down(&self, down: bool) {
        self.board_down.store(down, Ordering::SeqCst);
    }

    /// How many times the repo's visibility was asked
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn labels_of(&self, number: u64) -> Vec<String> {
        let labels = self.labels.lock().unwrap();
        labels.get(&number).cloned().unwrap_or_default()
    }

    fn board(&self) -> Result<(), ForgeError> {
        if self.board_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("the board is down".into()));
        }
        Ok(())
    }
}

impl Forge for FakeForge {
    fn visibility(&self, _repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(*self.visibility.lock().unwrap())
    }

    fn issue(&self, _repo: &ForgeSlug, number: u64) -> Result<Issue, ForgeError> {
        if self.missing.lock().unwrap().contains(&number) {
            return Err(ForgeError::Failed(format!("no issue #{number}")));
        }
        Ok(Issue {
            title: format!("Title of #{number}"),
            body: format!("Body of #{number}.\n"),
            labels: self.labels_of(number),
        })
    }

    fn ready_issues(&self, _repo: &ForgeSlug) -> Result<Vec<ReadyIssue>, ForgeError> {
        self.board()?;
        let ready = self.ready.lock().unwrap().clone();
        Ok(ready
            .into_iter()
            .map(|i| ReadyIssue {
                labels: self.labels_of(i.number),
                ..i
            })
            .collect())
    }

    fn open_pull_requests(&self, _repo: &ForgeSlug) -> Result<Vec<OpenPullRequest>, ForgeError> {
        self.board()?;
        let prs = self.pull_requests.lock().unwrap();
        let open = self.open.lock().unwrap().clone();
        Ok(open
            .into_iter()
            .filter(|pr| prs[&pr.number].state == PullRequestState::Open)
            .collect())
    }

    fn pull_request(&self, _repo: &ForgeSlug, number: u64) -> Result<PullRequest, ForgeError> {
        let pr = self.opened(number)?;
        let head = self
            .head_of(&pr.branch)
            .ok_or_else(|| ForgeError::Failed(format!("no branch {} on origin", pr.branch)))?;
        let checks = self.checks.lock().unwrap().get(&head).cloned();
        Ok(PullRequest {
            state: pr.state,
            draft: pr.draft,
            checks: checks.unwrap_or(Checks::Pending),
            head,
        })
    }

    fn comment(&self, _repo: &ForgeSlug, number: u64, body: &str) -> Result<(), ForgeError> {
        if self.comments_down.load(Ordering::SeqCst) {
            return Err(ForgeError::Failed("comments are down".into()));
        }
        self.opened(number)?;
        self.comments
            .lock()
            .unwrap()
            .push((number, body.to_owned()));
        Ok(())
    }

    fn mark_ready(&self, _repo: &ForgeSlug, number: u64) -> Result<(), ForgeError> {
        self.opened(number)?;
        let mut prs = self.pull_requests.lock().unwrap();
        prs.get_mut(&number).expect("checked above").draft = false;
        self.readied.lock().unwrap().push(number);
        Ok(())
    }

    // Refuses what GitHub refuses: a draft, a pull request that is not open,
    // and a head that moved since the caller looked.
    fn merge(&self, repo: &ForgeSlug, number: u64, head: &str) -> Result<(), ForgeError> {
        let now = self.pull_request(repo, number)?;
        if now.draft || now.state != PullRequestState::Open || now.head != head {
            return Err(ForgeError::Failed(format!(
                "cannot merge #{number}: {now:?}"
            )));
        }
        self.merges.lock().unwrap().push((number, head.to_owned()));
        self.set_state(number, PullRequestState::Merged);
        Ok(())
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
    pub(crate) clock: FakeClock,
}

impl Rig {
    /// Where every rig's clock starts
    pub(crate) const EPOCH: u64 = 1_790_000_000;

    /// Where the rig says the kelpie binary is
    pub(crate) const KELPIE: &str = "/opt/kelpie/bin/kelpie";

    /// A project with the example settings, pointed at a fresh repo
    pub(crate) fn new(project: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let rig = Self {
            project: ProjectName::try_from(project).unwrap(),
            claude: FakeClaude::default(),
            forge: FakeForge {
                visibility: Arc::new(Mutex::new(Visibility::Public)),
                missing: Arc::default(),
                labels: Arc::default(),
                ready: Arc::default(),
                open: Arc::default(),
                board_down: Arc::default(),
                calls: Arc::default(),
                origin: home.path().join("origin.git"),
                pull_requests: Arc::default(),
                checks: Arc::default(),
                comments: Arc::default(),
                comments_down: Arc::default(),
                readied: Arc::default(),
                merges: Arc::default(),
            },
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
