//! A fixture project of each ecosystem, built, linted and pushed from a worker's
//! fence under the real sandbox runtime
//!
//! The fixtures are `fixtures/ecosystems/<name>`. A toolchain lives under a fake
//! home the fence denies whole, as the maintainer's does, so a read-back the fence
//! lacks fails the run. Dependencies are fetched into the caches outside the
//! fence first: nothing inside it needs the network. Each harness runs the same
//! steps through its own policy, so pi's tests call what is here.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use super::sandbox::policy;
use crate::adapters::SandboxRuntime;
use crate::ports::{AgentCall, Policy, Reach, Role, Sandbox, Session, SessionId, Tools};
use crate::profile::WorkerProfile;
use crate::settings::{AgentHarness, BuildDir, Effort, EnvName, GuardHook, HookEvent, NonBlank};
use crate::test::{NO_PROGRAM, git, linked_worktree, short_tempdir};

/// A harness's fence: the call a worker makes and the policy its commands run under
#[derive(Clone, Copy)]
pub(crate) struct Through {
    /// The call from `cwd`, with `reach` and its settings file `settings`
    pub(crate) call: fn(Reach, PathBuf, PathBuf) -> AgentCall,
    /// The policy the call's commands run under, the machine's home being `home`
    pub(crate) policy: fn(&AgentCall, &Path) -> Policy,
}

/// The fence Claude Code's worker runs under
pub(super) const CLAUDE_CODE: Through = Through {
    call: claude_call,
    policy: claude_policy,
};

fn claude_call(reach: Reach, settings: PathBuf, cwd: PathBuf) -> AgentCall {
    AgentCall {
        issue: crate::ticket::TicketId::from_issue(7),
        settings,
        reach,
        ..AgentCall::new(
            Role::Worker,
            Tools::Work,
            AgentHarness::ClaudeCode,
            "claude-sonnet-5",
            Effort::Medium,
            Session::New(SessionId("s".into())),
            cwd,
            String::new(),
        )
    }
}

fn claude_policy(call: &AgentCall, home: &Path) -> Policy {
    policy(call, home, &NO_PROGRAM, &[]).unwrap()
}

/// A project of one ecosystem, whose fixture is a folder of `fixtures/ecosystems`
#[derive(Clone, Copy)]
pub(crate) enum Ecosystem {
    Rust,
    Go,
    Bun,
    Npm,
}

impl Ecosystem {
    fn fixture(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Bun => "bun",
            Self::Npm => "npm",
        }
    }

    // What a worker runs in the worktree, in order, before it pushes.
    fn steps(self) -> &'static [&'static str] {
        match self {
            Self::Rust | Self::Go => &["make build", "make test", "make lint"],
            Self::Bun => &[
                "bun install --frozen-lockfile",
                "bun run build",
                "bun run test",
                "bun run lint",
            ],
            Self::Npm => &[
                "npm ci --offline --no-audit --no-fund",
                "npm run build",
                "npm run test",
                "npm run lint",
            ],
        }
    }

    // Text the fixture's pre-push hook prints, which the push shows only if the hook ran.
    fn hook_prints(self) -> &'static str {
        match self {
            Self::Rust | Self::Go => "pre-push: make lint",
            Self::Bun => "pre-push: bun run lint",
            Self::Npm => "pre-push: npm run lint",
        }
    }

    // The variables the project's settings point into the build folder. npm's
    // cache is none of the fence's own, so a project that installs names it.
    fn build_env(self) -> BTreeMap<EnvName, BuildDir> {
        let mut env = BTreeMap::new();
        if matches!(self, Self::Npm) {
            env.insert(
                EnvName::try_from("NPM_CONFIG_CACHE".to_owned()).unwrap(),
                BuildDir::try_from("npm-cache".to_owned()).unwrap(),
            );
        }
        env
    }

    // Puts the toolchain's own home under the fake home, as the machine has it.
    // The compilers and runtimes stay where the machine keeps them.
    fn install(self, w: &World) {
        let home = w.path("home");
        match self {
            Self::Rust => fs::create_dir_all(home.join(".cargo")).unwrap(),
            Self::Go => {
                fs::create_dir_all(home.join("go/pkg/mod")).unwrap();
                copy_tool("golangci-lint", &home.join("go/bin"));
            }
            Self::Bun => copy_tool("bun", &home.join(".bun/bin")),
            Self::Npm => {}
        }
    }

    // The variables its tools read, and the folders of its toolchain on `PATH`.
    fn env(self, w: &World) -> Vec<(String, String)> {
        let home = w.path("home");
        let var = |name: &str, value: &Path| (name.to_owned(), value.display().to_string());
        match self {
            Self::Rust => {
                // rustup finds its toolchains under the real home, which the fence leaves.
                let rustup = std::env::var_os("RUSTUP_HOME")
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".rustup")))
                    .filter(|dir| dir.is_dir());
                [var("CARGO_HOME", &home.join(".cargo"))]
                    .into_iter()
                    .chain(rustup.map(|dir| var("RUSTUP_HOME", &dir)))
                    .collect()
            }
            Self::Go => vec![
                var("GOPATH", &home.join("go")),
                ("GOTOOLCHAIN".to_owned(), "local".to_owned()),
                on_path(&[home.join("go/bin")]),
            ],
            Self::Bun => vec![
                var("BUN_INSTALL", &home.join(".bun")),
                on_path(&[home.join(".bun/bin")]),
            ],
            Self::Npm => vec![("NPM_CONFIG_UPDATE_NOTIFIER".to_owned(), "false".to_owned())],
        }
    }

    // Fetches the dependencies into the caches, outside the fence, with `env`.
    fn warm(self, w: &World, env: &[(String, String)]) {
        let wt = w.path("wt");
        match self {
            // Cargo unpacks a crate's source on the first build, which a fence
            // that cannot write the cargo home cannot do.
            Self::Rust => {
                let target = w.path("warm");
                let env = [
                    env,
                    &[("CARGO_TARGET_DIR".to_owned(), target.display().to_string())],
                ];
                w.outside(&wt, "cargo check --locked --all-targets", &env.concat());
            }
            Self::Go => w.outside(&wt, "go mod download", env),
            Self::Bun => {
                w.outside(&wt, "bun install --frozen-lockfile", env);
                fs::remove_dir_all(wt.join("node_modules")).unwrap();
            }
            Self::Npm => {
                w.outside(&wt, "npm ci --no-audit --no-fund", env);
                fs::remove_dir_all(wt.join("node_modules")).unwrap();
            }
        }
    }
}

// `PATH` with `dirs` in front of the machine's.
fn on_path(dirs: &[PathBuf]) -> (String, String) {
    let machine = std::env::var_os("PATH").unwrap_or_default();
    let joined = std::env::join_paths(dirs.iter().cloned().chain(std::env::split_paths(&machine)));
    (
        "PATH".to_owned(),
        joined.unwrap().to_string_lossy().into_owned(),
    )
}

// Copies the program `name`, as `PATH` finds it, into `folder`.
fn copy_tool(name: &str, folder: &Path) {
    let machine = std::env::var_os("PATH").unwrap_or_default();
    let found = std::env::split_paths(&machine)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("{name} is not on PATH: .github/workflows/test.yml installs it"));
    fs::create_dir_all(folder).unwrap();
    fs::copy(found, folder.join(name)).unwrap();
}

// The fixture folder `name`, copied whole into `to`, modes kept.
fn copy_fixture(name: &str, to: &Path) {
    let from = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/ecosystems")
        .join(name);
    copy_tree(&from, to);
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        match entry.file_type().unwrap().is_dir() {
            true => copy_tree(&entry.path(), &target),
            false => {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
}

// A fake machine: a home the fence denies whole, the toolchains under it, and
// the worker's folders beside it.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    // Short enough for a socket, and canonical from the start.
    fn new() -> Self {
        let dir = short_tempdir();
        let root = dir.path().canonicalize().unwrap();
        for folder in ["home", "build/tmp", "worker", "outside"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        Self { _dir: dir, root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    // The worker's call: the worktree `wt` of the repo `repo`, its branch `wt`.
    fn call(
        &self,
        through: &Through,
        build_env: &BTreeMap<EnvName, BuildDir>,
        hooks: &[GuardHook],
    ) -> AgentCall {
        let (wt, build, git) = (self.path("wt"), self.path("build"), self.path("repo/.git"));
        let profile = WorkerProfile {
            worktree: &wt,
            build: &build,
            home: &self.path("home"),
            git_common_dir: &git,
            git_dir: &git.join("worktrees/wt"),
            branch: "wt",
            base: "main",
            kelpie_pushes: false,
            kelpie: Path::new("/k/kelpie"),
            skills: Path::new("/k/skills"),
            kelpie_home: Path::new("/k"),
            repo: Path::new("/k/repo"),
            guard_hooks: hooks,
            allowed_domains: &[],
            registries: &[],
            build_env,
            shep_home: &self.path("shep"),
            door: &self.path("dog/lease.sock"),
            token: None,
        };
        (through.call)(profile.reach(), self.path("worker/settings.json"), wt)
    }

    // Runs `script` in `cwd` with `env` on top of the machine's, outside any fence.
    fn outside(&self, cwd: &Path, script: &str, env: &[(String, String)]) {
        let out = Command::new("/bin/sh")
            .args(["-c", script])
            .current_dir(cwd)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .output()
            .unwrap();
        assert!(out.status.success(), "{script}: {}", said(&out));
    }

    // Runs `script` in the call's folder, inside the fence of `through`.
    fn inside(
        &self,
        through: &Through,
        call: &AgentCall,
        script: &str,
        env: &[(String, String)],
    ) -> Output {
        let policy = (through.policy)(call, &self.path("home"));
        // The sandbox runtime sets `TMPDIR` to `/tmp/claude` for the command, which a
        // runner does not have, so the script puts it back first.
        let tmp = self.path("build/tmp");
        let script = format!("export TMPDIR={}; {script}", tmp.display());
        let mut inner = Command::new("/bin/sh");
        inner
            .args(["-c", &script])
            .current_dir(&call.cwd)
            .envs(env.iter().map(|(k, v)| (k, v)));
        SandboxRuntime::under_test()
            .wrap(&policy, &self.path("worker/settings.sandbox.json"), &inner)
            .unwrap()
            .output()
            .unwrap()
    }

    // Every path of the machine the fence does not let the call write, with what
    // would show a write to it. Kelpie's own `worker` folder is not the fence's.
    fn outside_the_grants(&self, call: &AgentCall, through: &Through) -> BTreeMap<PathBuf, Stamp> {
        let mut granted = (through.policy)(call, &self.path("home")).write;
        granted.push(self.path("worker"));
        let mut seen = BTreeMap::new();
        let mut todo = vec![self.root.clone()];
        while let Some(dir) = todo.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if granted.iter().any(|g| path.starts_with(g)) {
                    continue;
                }
                let meta = path.symlink_metadata().unwrap();
                if meta.is_dir() {
                    todo.push(path.clone());
                    seen.insert(path, (0, None));
                } else {
                    seen.insert(path, (meta.len(), meta.modified().ok()));
                }
            }
        }
        seen
    }
}

// A file's size and time of last write; a folder's are none, as adding to it moves its time.
type Stamp = (u64, Option<SystemTime>);

fn said(out: &Output) -> String {
    format!(
        "{}\n--- stdout\n{}\n--- stderr\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// Asserts that a call of `through` wrote nothing between `before` and now.
fn assert_wrote_nothing_outside(
    w: &World,
    through: &Through,
    call: &AgentCall,
    before: &BTreeMap<PathBuf, Stamp>,
) {
    let after = w.outside_the_grants(call, through);
    let changed: Vec<&PathBuf> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .collect();
    assert!(
        changed.is_empty(),
        "written outside the fence's grants: {changed:#?}"
    );
}

/// The project of `eco` builds, tests, lints and pushes from inside the fence of `through`
pub(crate) fn project(through: &Through, eco: Ecosystem) {
    let w = World::new();
    let (repo, wt) = linked_worktree(&w.root);
    copy_fixture(eco.fixture(), &wt);
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "--quiet", "-m", "fixture"]);
    // A local bare remote the build folder holds, so the push needs no network and
    // writes only what the fence grants. The tracking ref is there as it is after
    // the branch's first push, since its folder is not the fence's to make.
    let remote = w.path("build/remote.git");
    git(
        &w.root,
        &["init", "--quiet", "--bare", remote.to_str().unwrap()],
    );
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["config", "core.hooksPath", ".githooks"]);
    git(&repo, &["update-ref", "refs/remotes/origin/wt", "HEAD"]);

    eco.install(&w);
    let call = w.call(through, &eco.build_env(), &[]);
    let fence = call.reach.fence.as_deref().unwrap();
    let env: Vec<(String, String)> = fence
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .chain([
            ("HOME".to_owned(), w.path("home").display().to_string()),
            (
                "TMPDIR".to_owned(),
                w.path("build/tmp").display().to_string(),
            ),
            ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
            ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
            ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
        ])
        .chain(eco.env(&w))
        .collect();
    eco.warm(&w, &env);

    let before = w.outside_the_grants(&call, through);
    let push = "git push origin wt";
    let mut pushed = None;
    for step in eco.steps().iter().copied().chain([push]) {
        let out = w.inside(through, &call, step, &env);
        assert!(out.status.success(), "{step}: {}", said(&out));
        pushed = Some(out);
    }
    let stderr = String::from_utf8_lossy(&pushed.unwrap().stderr).into_owned();
    assert!(
        stderr.contains(eco.hook_prints()),
        "the pre-push hook did not run: {stderr}"
    );
    assert_eq!(
        git(&remote, &["rev-parse", "wt"]),
        git(&wt, &["rev-parse", "HEAD"]),
        "the push did not reach the remote"
    );
    assert_wrote_nothing_outside(&w, through, &call, &before);
}

/// A hook the home keeps loads its neighbour, its config and its `package.json` inside the fence of `through`
pub(crate) fn hook_script(through: &Through) {
    let w = World::new();
    fs::create_dir_all(w.path("wt")).unwrap();
    copy_fixture("hook", &w.path("home/hooks"));
    let command = format!("node {}", w.path("home/hooks/gate.js").display());
    let hooks = [GuardHook {
        event: HookEvent::PreToolUse,
        matcher: None,
        command: NonBlank::try_from(command.clone()).unwrap(),
    }];
    let call = w.call(through, &BTreeMap::new(), &hooks);
    let before = w.outside_the_grants(&call, through);
    let out = w.inside(through, &call, &command, &[]);
    assert!(out.status.success(), "{command}: {}", said(&out));
    assert_wrote_nothing_outside(&w, through, &call, &before);
}

#[test]
#[ignore = "runs the real sandbox with Rust, after `cargo build`"]
fn a_rust_project_builds_tests_lints_and_pushes_inside_the_fence() {
    project(&CLAUDE_CODE, Ecosystem::Rust);
}

#[test]
#[ignore = "runs the real sandbox with Go and golangci-lint, after `cargo build`"]
fn a_go_project_builds_tests_lints_and_pushes_inside_the_fence() {
    project(&CLAUDE_CODE, Ecosystem::Go);
}

#[test]
#[ignore = "runs the real sandbox with bun, after `cargo build`"]
fn a_bun_project_installs_tests_and_pushes_inside_the_fence() {
    project(&CLAUDE_CODE, Ecosystem::Bun);
}

#[test]
#[ignore = "runs the real sandbox with Node, after `cargo build`"]
fn an_npm_project_installs_tests_and_pushes_inside_the_fence() {
    project(&CLAUDE_CODE, Ecosystem::Npm);
}

#[test]
#[ignore = "runs the real sandbox with Node, after `cargo build`"]
fn a_hook_script_loads_its_neighbours_inside_the_fence() {
    hook_script(&CLAUDE_CODE);
}
