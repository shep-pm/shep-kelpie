// A worker's escapes, each tried under the real sandbox runtime with the
// policy the adapter builds, and each refused. A shell stands in for Claude
// Code: inside the sandbox, its file tools, hooks and commands are alike.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::sandbox::{policy, transcripts};
use crate::adapters::SandboxRuntime;
use crate::ports::{AgentCall, Role, Sandbox, Session, SessionId, Tools};
use crate::preview::Tools as KelpieTools;
use crate::profile::WorkerProfile;
use crate::settings::Effort;

const NEEDS: &str = "needs kelpie's tools: KELPIE_TOOLS=<dir> from `shep kelpie tools install`";

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    // Under `/tmp`, short enough for a socket, and canonical from the start.
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let root = dir.path().canonicalize().unwrap();
        for folder in ["home", "wt/src", "build", "worker", "outside", "repo/.git"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        Self { _dir: dir, root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    fn call(&self) -> AgentCall {
        let (wt, build, git) = (self.path("wt"), self.path("build"), self.path("repo/.git"));
        let profile = WorkerProfile {
            worktree: &wt,
            build: &build,
            git_common_dir: &git,
            git_dir: &git.join("worktrees/7"),
            branch: "kelpie/7",
            kelpie: Path::new("/k/kelpie"),
            kelpie_home: Path::new("/k"),
            repo: Path::new("/k/repo"),
            private_names: &[],
            guard_hooks: &[],
            allowed_domains: &[],
            build_env: &BTreeMap::new(),
            preview: None,
            shep_home: &self.path("shep"),
        };
        AgentCall {
            role: Role::Worker,
            issue: 7,
            model: "claude-sonnet-5".into(),
            effort: Effort::Medium,
            session: Session::New(SessionId("s".into())),
            cwd: wt.clone(),
            settings: self.path("worker/settings.json"),
            instructions: None,
            prompt: String::new(),
            timeout: None,
            mcp_config: None,
            plugin_dirs: Vec::new(),
            tools: Tools::Work,
            reach: profile.reach(),
        }
    }

    // Runs `script` in the worktree, inside the worker's sandbox.
    fn try_to(&self, script: &str) -> Output {
        let tools = KelpieTools::at(std::env::var_os("KELPIE_TOOLS").expect(NEEDS).into());
        let call = self.call();
        let policy = policy(&call, &self.path("home")).unwrap();
        let mut inner = Command::new("/bin/sh");
        inner.args(["-c", script]).current_dir(&call.cwd);
        SandboxRuntime::new(tools)
            .wrap(&policy, &self.path("worker/settings.sandbox.json"), &inner)
            .unwrap()
            .output()
            .unwrap()
    }

    // Asserts `script` wrote nothing at `written`, which the file system
    // opens in any case and Unicode spelling it folds.
    fn refused(&self, script: &str, written: &str) {
        let out = self.try_to(script);
        let path = self.path(written);
        assert!(
            !path.exists() && path.symlink_metadata().is_err(),
            "{script} wrote {written}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
#[ignore = "needs kelpie's tools: KELPIE_TOOLS=<dir> from `shep kelpie tools install`"]
fn the_worktree_the_build_folder_and_the_transcripts_are_written_and_nothing_outside() {
    let w = World::new();
    let transcripts = transcripts(&w.path("home"), &w.path("wt")).unwrap();
    std::fs::create_dir_all(&transcripts).unwrap();
    let out = w.try_to(&format!(
        "echo ok > src/lib.rs && echo ok > {build}/ok && echo ok > {t}/s.jsonl",
        build = w.path("build").display(),
        t = transcripts.display(),
    ));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(w.path("wt/src/lib.rs").exists() && w.path("build/ok").exists());
    assert!(transcripts.join("s.jsonl").exists());

    for (script, written) in [
        ("echo x > ../outside/f", "outside/f"),
        ("echo x > ../home/.zshrc", "home/.zshrc"),
        (
            "mkdir -p ../home/.claude && echo {} > ../home/.claude/settings.json",
            "home/.claude/settings.json",
        ),
        ("echo {} > ../home/.claude.json", "home/.claude.json"),
        ("echo x > ../repo/.git/config", "repo/.git/config"),
        ("ln -s ../outside escape && echo x > escape/f", "outside/f"),
    ] {
        w.refused(script, written);
    }
    let memory = transcripts.join("memory");
    let out = w.try_to(&format!(
        "mkdir -p {m} && echo x > {m}/MEMORY.md",
        m = memory.display()
    ));
    assert!(
        !memory.join("MEMORY.md").exists(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "needs kelpie's tools: KELPIE_TOOLS=<dir> from `shep kelpie tools install`"]
fn claude_codes_own_files_are_refused_in_any_case_and_spelling() {
    let w = World::new();
    for (script, written) in [
        (
            "mkdir other && echo {} > other/settings.json && mv other .claude",
            "wt/.claude",
        ),
        ("mkdir .claude", "wt/.claude"),
        ("mkdir .CLAUDE", "wt/.claude"),
        ("echo {} > .mcp.json", "wt/.mcp.json"),
        ("echo {} > .MCP.JSON", "wt/.mcp.json"),
        ("echo {} > .mcp.j\u{17f}on", "wt/.mcp.json"),
        ("ln -s /etc/hosts .mcp.json", "wt/.mcp.json"),
        (
            "mkdir -p src/.CLAUDE/skills && echo x > src/.CLAUDE/skills/s.md",
            "wt/src/.claude",
        ),
        ("ln -s ../outside .claude", "wt/.claude"),
    ] {
        w.refused(script, written);
    }

    let w = World::new();
    std::fs::create_dir_all(w.path("wt/.claude")).unwrap();
    for (script, written) in [
        (
            "echo {} > .claude/settings.local.json",
            "wt/.claude/settings.local.json",
        ),
        (
            "echo {} > .CLAUDE/Settings.Local.json",
            "wt/.claude/settings.local.json",
        ),
        (
            "mkdir -p .claude/agents && echo x > .claude/agents/a.md",
            "wt/.claude/agents",
        ),
        (
            "cp /etc/hosts .claude/settings.json",
            "wt/.claude/settings.json",
        ),
    ] {
        w.refused(script, written);
    }
}

#[test]
#[ignore = "needs kelpie's tools: KELPIE_TOOLS=<dir> from `shep kelpie tools install`"]
fn codexs_own_files_are_refused_in_any_case() {
    let w = World::new();
    for (script, written) in [
        (
            "mkdir other && echo x > other/config.toml && mv other .codex",
            "wt/.codex",
        ),
        (
            "mkdir -p .codex && echo x > .codex/config.toml",
            "wt/.codex",
        ),
        (
            "mkdir -p .CODEX && echo x > .CODEX/config.toml",
            "wt/.codex",
        ),
        (
            "mkdir -p src/.Codex && echo x > src/.Codex/config.toml",
            "wt/src/.codex",
        ),
        ("ln -s ../outside .codex", "wt/.codex"),
    ] {
        w.refused(script, written);
    }

    let w = World::new();
    std::fs::create_dir_all(w.path("wt/.codex")).unwrap();
    for (script, written) in [
        ("echo x > .codex/config.toml", "wt/.codex/config.toml"),
        ("echo x > .Codex/Config.toml", "wt/.codex/config.toml"),
    ] {
        w.refused(script, written);
    }
}

// The real adapter on Haiku: a worker turn with kelpie's hooks from this
// build and the shots tool bridged in, then the same session resumed.
#[test]
#[ignore = "runs real Claude calls on Haiku under KELPIE_TOOLS' sandbox, after `cargo build`; about 1 min"]
fn a_live_worker_turn_runs_inside_the_sandbox_and_resumes() {
    use std::sync::Arc;

    use crate::adapters::ClaudeCli;
    use crate::ports::Agents;
    use crate::test::git;

    let tools = KelpieTools::at(std::env::var_os("KELPIE_TOOLS").expect(NEEDS).into());
    let exe = std::env::current_exe().unwrap();
    let kelpie = exe.parent().unwrap().parent().unwrap().join("shep-kelpie");
    assert!(
        kelpie.is_file(),
        "run `cargo build` first: {}",
        kelpie.display()
    );
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "probe\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "probe"]);
    let wt = root.join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "kelpie/7",
            wt.to_str().unwrap(),
        ],
    );
    let (build, worker) = (root.join("build"), root.join("worker"));
    std::fs::create_dir_all(&build).unwrap();
    std::fs::create_dir_all(&worker).unwrap();
    let job = worker.join("shots-job.json");
    let shots = crate::shots::ShotsJob {
        worktree: wt.clone(),
        build: build.clone(),
        out: root.join("shots"),
        launch: Err("this probe has no launch file".into()),
        routes: Vec::new(),
        domains: Vec::new(),
        env: BTreeMap::new(),
        server_pid: root.join("shots/dev-server.pid"),
        shep_home: root.join("shep"),
    };
    std::fs::write(&job, serde_json::to_string(&shots).unwrap()).unwrap();
    let config = worker.join("mcp.json");
    let servers = serde_json::json!({ "mcpServers": { "kelpie": {
        "command": kelpie, "args": ["shots-mcp", tools.dir(), job],
    } } });
    std::fs::write(&config, servers.to_string()).unwrap();
    let git_dir = repo.join(".git");
    let preview: [crate::settings::NonBlank; 0] = [];
    let profile = WorkerProfile {
        worktree: &wt,
        build: &build,
        git_common_dir: &git_dir,
        git_dir: &git_dir.join("worktrees/wt"),
        branch: "kelpie/7",
        kelpie: &kelpie,
        kelpie_home: &root,
        repo: &repo,
        private_names: &[],
        guard_hooks: &[],
        allowed_domains: &[],
        build_env: &BTreeMap::new(),
        preview: Some(&preview),
        shep_home: &root.join("shep"),
    };
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    let cli = ClaudeCli::default().sandboxed(Arc::new(SandboxRuntime::new(tools)), home.clone());
    let id = crate::work_item::new_session_id().unwrap();
    let mut call = AgentCall {
        role: Role::Worker,
        issue: 7,
        model: "haiku".into(),
        effort: Effort::Low,
        session: Session::New(id.clone()),
        cwd: wt.clone(),
        settings: worker.join("settings.json"),
        instructions: None,
        prompt: format!(
            "This is a sandbox test. Run each step with the tool named, one at a time, and \
             report each one's exact output or error, numbered: \
             (1) Bash: echo inside > a.txt && cat a.txt \
             (2) Write the file .claude/settings.local.json with the text {{}} \
             (3) Bash: mkdir other && mv other .codex \
             (4) Bash: echo x > {outside} \
             (5) Bash: curl -sS -m 10 https://example.com -o /dev/null \
             (6) call the mcp__kelpie__shots tool with routes [\"/\"]",
            outside = root.join("outside.txt").display()
        ),
        timeout: Some(std::time::Duration::from_secs(300)),
        mcp_config: Some(config),
        plugin_dirs: Vec::new(),
        tools: Tools::Work,
        reach: profile.reach(),
    };
    cli.prepare(&call).unwrap();
    let reply = cli.run(&call).expect("the turn ran");
    println!("--- turn ---\n{}\n---", reply.text);
    assert_eq!(
        std::fs::read_to_string(wt.join("a.txt")).unwrap(),
        "inside\n"
    );
    for escaped in [
        wt.join(".claude/settings.local.json"),
        wt.join(".codex"),
        root.join("outside.txt"),
    ] {
        assert!(!escaped.exists(), "{} was written", escaped.display());
    }
    assert!(
        reply.text.contains("launch file"),
        "the shots tool was not reached"
    );

    call.session = Session::Resume(id);
    call.prompt = "What did step 1 print? Answer with that word alone.".into();
    let reply = cli.run(&call).expect("the resumed turn ran");
    println!("--- resumed ---\n{}\n---", reply.text);
    assert!(reply.text.contains("inside"), "{}", reply.text);
    assert_eq!(
        std::fs::read_dir(&worker)
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "sock"))
            .count(),
        0,
        "a bridge's socket outlived its call"
    );
}

#[test]
#[ignore = "needs kelpie's tools: KELPIE_TOOLS=<dir> from `shep kelpie tools install`"]
fn a_host_not_allowed_and_a_socket_not_bridged_are_refused() {
    let w = World::new();
    let out = w.try_to("curl -sS -m 20 -o /dev/null -w '%{http_code}' https://example.com/");
    assert!(
        !out.status.success(),
        "example.com answered {:?}",
        out.stdout
    );

    let socket = w.path("outside/s.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let out = w.try_to(&format!(
        "perl -MIO::Socket::UNIX -e 'IO::Socket::UNIX->new(Peer => $ARGV[0]) or exit 7' {}",
        socket.display()
    ));
    assert_eq!(out.status.code(), Some(7), "a socket outside was reached");
}
