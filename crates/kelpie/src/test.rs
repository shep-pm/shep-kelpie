//! The main seam's rig: a runner on stand-ins for Claude, the forge and the
//! clock, over a real git repo in a throwaway home

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use crate::ports::{
    Claude, ClaudeCall, ClaudeError, ClaudeReply, Clock, Forge, ForgeError, Ports, Timestamp,
    Visibility,
};
use crate::runner::{OpenError, ProjectName, ProjectPaths, Runner, answer};
use crate::settings::ForgeSlug;

/// The `repo` in `settings.example.toml`, which the rig points at its own
const EXAMPLE_REPO: &str = "~/.kelpie/repos/shep";

/// Records every call and answers each with a failure
#[derive(Debug, Clone, Default)]
pub(crate) struct FakeClaude {
    calls: Arc<Mutex<Vec<ClaudeCall>>>,
}

impl FakeClaude {
    pub(crate) fn calls(&self) -> Vec<ClaudeCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Claude for FakeClaude {
    fn run(&self, call: &ClaudeCall) -> Result<ClaudeReply, ClaudeError> {
        self.calls.lock().unwrap().push(call.clone());
        Err(ClaudeError::Failed(
            "the rig scripts no Claude reply".into(),
        ))
    }
}

/// A forge whose repo is public unless a test says otherwise
#[derive(Debug, Clone)]
pub(crate) struct FakeForge {
    visibility: Arc<Mutex<Visibility>>,
    calls: Arc<AtomicUsize>,
}

impl FakeForge {
    pub(crate) fn set_visibility(&self, visibility: Visibility) {
        *self.visibility.lock().unwrap() = visibility;
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Forge for FakeForge {
    fn visibility(&self, _repo: &ForgeSlug) -> Result<Visibility, ForgeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(*self.visibility.lock().unwrap())
    }
}

/// A clock that moves only when a test moves it
#[derive(Debug, Clone)]
pub(crate) struct FakeClock(Arc<AtomicU64>);

impl FakeClock {
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

    /// A project with the example settings, pointed at a fresh repo
    pub(crate) fn new(project: &str) -> Self {
        let rig = Self {
            home: tempfile::tempdir().unwrap(),
            project: ProjectName::try_from(project).unwrap(),
            claude: FakeClaude::default(),
            forge: FakeForge {
                visibility: Arc::new(Mutex::new(Visibility::Public)),
                calls: Arc::default(),
            },
            clock: FakeClock(Arc::new(AtomicU64::new(Self::EPOCH))),
        };
        rig.make_repo(&rig.home.path().join("origin.git"));

        let example = include_str!("../settings.example.toml");
        assert!(example.contains(EXAMPLE_REPO), "the example's repo moved");
        let settings = example.replace(EXAMPLE_REPO, &rig.repo().display().to_string());
        let paths = rig.paths();
        std::fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
        std::fs::write(&paths.settings, settings).unwrap();
        rig
    }

    fn make_repo(&self, origin: &Path) {
        let root = self.home.path();
        git(
            root,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                path(origin),
            ],
        );
        git(
            root,
            &["clone", "--quiet", path(origin), path(&self.repo())],
        );
        std::fs::write(self.repo().join("README.md"), "a project\n").unwrap();
        git(&self.repo(), &["add", "README.md"]);
        git(&self.repo(), &["commit", "--quiet", "-m", "first"]);
        git(&self.repo(), &["push", "--quiet", "origin", "main"]);
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
            claude: Box::new(self.claude.clone()),
            forge: Box::new(self.forge.clone()),
            clock: Box::new(self.clock.clone()),
        };
        Runner::open(self.project.clone(), &self.paths(), self.home.path(), ports).map(Mutex::new)
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

// The maintainer's own git config never reaches the rig: no signing, no hooks.
fn git(cwd: &Path, args: &[&str]) {
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
}
