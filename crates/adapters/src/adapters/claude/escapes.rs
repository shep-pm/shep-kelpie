// A worker's escapes, each tried under the real sandbox runtime with the
// policy the adapter builds, and each refused. A shell stands in for Claude
// Code: inside the sandbox, its file tools, hooks and commands are alike.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::sandbox::{config_folder, policy, transcripts};
use crate::adapters::{Base, SandboxRuntime};
use crate::ports::{AgentCall, Ending, Policy, Role, Sandbox, Session, SessionId, Tools};
use crate::profile::WorkerProfile;
use crate::settings::Effort;
use crate::test::NO_PROGRAM;

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    // Short enough for a socket, and canonical from the start.
    fn new() -> Self {
        let dir = crate::test::short_tempdir();
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
        let home = self.path("home");
        let profile = WorkerProfile {
            worktree: &wt,
            build: &build,
            home: &home,
            git_common_dir: &git,
            git_dir: &git.join("worktrees/7"),
            branch: "kelpie/7",
            base: "main",
            kelpie_pushes: false,
            kelpie: Path::new("/k/kelpie"),
            skills: Path::new("/k/skills"),
            kelpie_home: Path::new("/k"),
            repo: Path::new("/k/repo"),
            guard_hooks: &[],
            allowed_domains: &[],
            registries: &[],
            build_env: &BTreeMap::new(),
            shep_home: &self.path("shep"),
            door: &self.path("dog/lease.sock"),
            token: None,
            login_token: None,
        };
        AgentCall {
            issue: crate::ticket::TicketId::from_issue(7),
            settings: self.path("worker/settings.json"),
            reach: profile.reach(),
            ..AgentCall::new(
                Role::Worker,
                Tools::Work,
                crate::settings::AgentHarness::ClaudeCode,
                "claude-sonnet-5",
                Effort::Medium,
                Session::New(SessionId("s".into())),
                wt.clone(),
                String::new(),
            )
        }
    }

    // The policy the wrapped shell runs under, with a stand-in for the harness
    // program, which the sandbox must read to exec it.
    fn sandbox_policy(&self) -> Policy {
        let call = self.call();
        policy(&call, &self.path("home"), &NO_PROGRAM, &[]).unwrap()
    }

    // Runs `script` in the worktree, inside the worker's sandbox.
    fn try_to(&self, script: &str) -> Output {
        let call = self.call();
        let policy = self.sandbox_policy();
        let mut inner = Command::new("/bin/sh");
        inner.args(["-c", script]).current_dir(&call.cwd);
        SandboxRuntime::under_test()
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
#[ignore = "runs the real sandbox, after `cargo build`"]
fn the_worktree_the_build_folder_and_the_transcripts_are_written_and_nothing_outside() {
    let w = World::new();
    let transcripts = transcripts(&config_folder(&w.call()), &w.path("wt")).unwrap();
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
#[ignore = "runs the real sandbox, after `cargo build`"]
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

// On Linux srt skips the globs of `denyWrite`, so the fence lists the folders
// below the worktree when the call starts. `World::try_to` builds its policy
// from the worktree as it stands, which is the call's start.
#[test]
#[ignore = "runs the real sandbox, after `cargo build`"]
fn a_nested_claude_folder_that_existed_at_call_start_is_refused() {
    let w = World::new();
    std::fs::create_dir_all(w.path("wt/src/.claude")).unwrap();
    std::fs::create_dir_all(w.path("wt/lib/.Codex")).unwrap();
    for (script, written) in [
        (
            "echo {} > src/.claude/settings.local.json",
            "wt/src/.claude/settings.local.json",
        ),
        (
            "mkdir -p src/.claude/agents && echo x > src/.claude/agents/a.md",
            "wt/src/.claude/agents",
        ),
        (
            "echo x > lib/.Codex/config.toml",
            "wt/lib/.Codex/config.toml",
        ),
    ] {
        w.refused(script, written);
    }
}

#[test]
#[ignore = "runs the real sandbox, after `cargo build`"]
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

// A worker's shell reads its worktree, its build folder and an installed
// dependency source, and nothing of another repo of the same home folder,
// private ones and their `.env` files included.
#[test]
#[ignore = "needs kelpie's tools: KELPIE_TOOLS=<dir> from `shep kelpie tools install`"]
fn a_worker_reads_its_own_folders_and_no_other_repo_of_the_home() {
    let w = World::new();
    let dep = w.path("home/.cargo/registry/src/serde-1.0.2");
    let files: Vec<(PathBuf, &str)> = vec![
        (w.path("wt/src/lib.rs"), "the worktree's own\n"),
        (w.path("build/built.txt"), "the build folder's own\n"),
        (dep.join("src/lib.rs"), "an installed dependency\n"),
        (w.path("home/other-repo/.env"), "SECRET=1\n"),
        (w.path("home/other-repo/src/lib.rs"), "private work\n"),
        (w.path("home/other-repo/.git/config"), "[remote]\n"),
    ];
    for (path, text) in &files {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    for path in files[..3].iter() {
        let out = w.try_to(&format!("cat {}", path.0.display()));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            path.1,
            "{}: {}",
            path.0.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    for path in files[3..].iter() {
        let out = w.try_to(&format!("cat {}", path.0.display()));
        let why = String::from_utf8_lossy(&out.stderr).to_string();
        // macOS seatbelt refuses the open. Linux crabpot covers a denied
        // folder with an empty tmpfs, so the name is not there at all.
        let expected = if cfg!(target_os = "linux") {
            "No such file or directory"
        } else {
            "Operation not permitted"
        };
        assert!(
            !out.status.success() && why.contains(expected),
            "{} was read: {why}",
            path.0.display()
        );
    }
}

// The real adapter on Haiku: a worker turn with kelpie's hooks from this
// build, then the same session resumed.
#[test]
#[ignore = "runs real Claude calls on Haiku under the sandbox, after `cargo build`; about 1 min"]
fn a_live_worker_turn_runs_inside_the_sandbox_and_resumes() {
    use std::sync::Arc;

    use crate::adapters::ClaudeCli;
    use crate::ports::Agents;
    use crate::test::git;

    let exe = std::env::current_exe().unwrap();
    let kelpie = exe.parent().unwrap().parent().unwrap().join("shep-kelpie");
    assert!(
        kelpie.is_file(),
        "run `cargo build` first: {}",
        kelpie.display()
    );
    let dir = crate::test::short_tempdir();
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
    let git_dir = repo.join(".git");
    let profile = WorkerProfile {
        worktree: &wt,
        build: &build,
        home: &root,
        git_common_dir: &git_dir,
        git_dir: &git_dir.join("worktrees/wt"),
        branch: "kelpie/7",
        base: "main",
        kelpie_pushes: false,
        kelpie: &kelpie,
        skills: &root.join("skills"),
        kelpie_home: &root,
        repo: &repo,
        guard_hooks: &[],
        allowed_domains: &[],
        registries: &[],
        build_env: &BTreeMap::new(),
        shep_home: &root.join("shep"),
        door: &root.join("dog/lease.sock"),
        token: None,
        login_token: None,
    };
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    let login = crate::adapters::LoginToken::take().expect("set CLAUDE_CODE_OAUTH_TOKEN");
    let cli = ClaudeCli::on(
        &Base::default().sandboxed(Arc::new(SandboxRuntime::under_test()), home.clone()),
    )
    .with_login(Some(login));
    let id = crate::work_item::new_session_id().unwrap();
    let mut call = AgentCall {
        issue: crate::ticket::TicketId::from_issue(7),
        settings: worker.join("settings.json"),
        reach: profile.reach(),
        ..AgentCall::new(
            Role::Worker,
            Tools::Work,
            crate::settings::AgentHarness::ClaudeCode,
            "haiku",
            Effort::Low,
            Session::New(id.clone()),
            wt.clone(),
            format!(
                "This is a sandbox test. Run each step with the tool named, one at a time, and \
             report each one's exact output or error, numbered: \
             (1) Bash: echo inside > a.txt && cat a.txt \
             (2) Write the file .claude/settings.local.json with the text {{}} \
             (3) Bash: mkdir other && mv other .codex \
             (4) Bash: echo x > {outside} \
             (5) Bash: curl -sS -m 10 https://example.com -o /dev/null",
                outside = root.join("outside.txt").display()
            ),
        )
    };
    cli.prepare(&call).unwrap();
    // A live call has five minutes, as a worker's turn has its ceiling.
    let ending = Ending::default();
    let ceiling = ending.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(300));
        ceiling.end();
    });
    let reply = cli.run(&call, &ending).expect("the turn ran");
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

    call.session = Session::Resume(id);
    call.prompt = "What did step 1 print? Answer with that word alone.".into();
    let reply = cli.run(&call, &ending).expect("the resumed turn ran");
    println!("--- resumed ---\n{}\n---", reply.text);
    assert!(reply.text.contains("inside"), "{}", reply.text);
}

// Whether `curl`'s `%{http_connect}` shows the proxy refusing the CONNECT. srt
// answers one to a host the policy does not list with a 403. A `curl` that
// could not reach a proxy at all prints `000`, and fails the same way with no
// network, so only the 403 shows the fence refused it.
fn refused_by_the_proxy(connect_code: &str) -> bool {
    connect_code.trim() == "403"
}

#[test]
fn a_403_to_the_connect_is_the_proxys_refusal_and_a_failed_connect_is_not() {
    // `curl -sS -w '%{http_connect}'` against a proxy that refuses, and with none to reach.
    assert!(refused_by_the_proxy("403"));
    assert!(!refused_by_the_proxy("000"));
    assert!(!refused_by_the_proxy(""));
}

#[cfg(unix)]
#[test]
#[ignore = "runs the real sandbox, after `cargo build`"]
fn a_host_not_allowed_and_a_socket_not_listed_are_refused() {
    let w = World::new();
    let out = w.try_to("curl -sS -m 20 -o /dev/null -w '%{http_connect}' https://example.com/");
    let code = String::from_utf8_lossy(&out.stdout);
    assert!(
        refused_by_the_proxy(&code),
        "example.com was not refused by the proxy: {code:?}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let socket = w.path("outside/s.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let out = w.try_to(&format!(
        "perl -MIO::Socket::UNIX -e 'IO::Socket::UNIX->new(Peer => $ARGV[0]) or exit 7' {}",
        socket.display()
    ));
    assert_eq!(out.status.code(), Some(7), "a socket outside was reached");
}

// Measured on Linux: srt 0.0.77 turns every Unix socket off whatever
// `allowUnixSockets` lists, so the dog's door in the policy is refused too.
// On macOS the same policy reaches it.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "runs the real sandbox, after `cargo build`"]
fn a_socket_the_policy_lists_is_still_refused_on_linux() {
    let w = World::new();
    let door = w.path("dog/lease.sock");
    std::fs::create_dir_all(door.parent().unwrap()).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(&door).unwrap();
    let policy = w.sandbox_policy();
    assert_eq!(
        policy.sockets,
        std::slice::from_ref(&door),
        "the door is listed"
    );
    let out = w.try_to(&format!(
        "perl -MIO::Socket::UNIX -e 'IO::Socket::UNIX->new(Peer => $ARGV[0]) or exit 7' {}",
        door.display()
    ));
    assert_eq!(out.status.code(), Some(7), "the listed door was reached");
}
