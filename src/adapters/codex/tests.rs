use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::adapters::SandboxRuntime;
use crate::ports::{Reach, Role, Unreadable};
use crate::preview::Tools as KelpieTools;
use crate::profile::WorkerProfile;
use crate::settings::Effort;
use crate::test::OpenSandbox;

const WORKER_ID: &str = "5a2e9d40-1c7b-4b38-8e6f-2d4a9c1b7e53";

/// A worker's turn on gpt-6-luna that added one file through apply_patch
const FRESH: &str = include_str!("../../../fixtures/codex-exec-fresh.jsonl");
/// The same session resumed: a patch `kelpie confine` refused, a write the
/// sandbox refused, and a command `kelpie guard` refused
const RESUMED: &str = include_str!("../../../fixtures/codex-exec-resumed.jsonl");
/// A usage limit, written by hand from Codex's source, since the account
/// was never run to one
const LIMITED: &str = include_str!("../../../fixtures/codex-exec-usage-limit.jsonl");

/// The Codex session the recordings ran in
const THREAD: &str = "01a0f7fd-4ed5-7773-80ea-165edc292d8d";

fn output(code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

fn id(id: &str) -> SessionId {
    SessionId(id.into())
}

fn strings(call: &AgentCall, resumed: Option<&Thread>, instructions: Option<&str>) -> Vec<String> {
    argv(call, &Files::of(call).home, resumed, instructions)
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect()
}

// A stand-in for codex that keeps its arguments and `CODEX_HOME`, and
// prints `stdout`.
fn stand_in(world: &World, stdout: &str) -> CodexCli {
    std::fs::write(world.path("stdout.jsonl"), stdout).unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nprintf '%s' \"$CODEX_HOME\" > {}\ncat {}\n",
        world.path("argv").display(),
        world.path("codex_home").display(),
        world.path("stdout.jsonl").display()
    );
    if !world.path("codex").exists() {
        crate::test::write_script(&world.path("codex"), &script);
    }
    ClaudeCli::default()
        .sandboxed(Arc::new(OpenSandbox::default()), world.path("home"))
        .codex(world.path("login"))
        .with_program(world.path("codex"))
}

#[test]
fn a_new_worker_session_loads_nothing_of_the_maintainers_and_runs_kelpies_checks() {
    let w = World::new();
    let mut call = w.fenced(Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.prompt = "implement #7".into();
    let argv = strings(&call, None, Some("Be brief."));
    let at = |flag: &str| argv.iter().position(|a| a == flag).unwrap();
    assert_eq!(argv[0], "exec");
    for flag in [
        "--json",
        "--ignore-user-config",
        "--ignore-rules",
        "--dangerously-bypass-approvals-and-sandbox",
        "--dangerously-bypass-hook-trust",
    ] {
        assert!(argv.iter().any(|a| a == flag), "{flag}");
    }
    assert_eq!(argv[at("--model") + 1], "gpt-6-sol");
    for config in [
        "model_reasoning_effort=\"medium\"",
        "web_search=\"disabled\"",
        "developer_instructions=\"Be brief.\"",
    ] {
        assert!(argv.iter().any(|a| a == config), "{config} in {argv:?}");
    }
    for feature in ["multi_agent", "plugins", "memories", "apps"] {
        assert!(
            argv.windows(2).any(|w| w == ["--disable", feature]),
            "{feature}"
        );
    }
    assert!(!argv.windows(2).any(|w| w == ["--disable", "shell_tool"]));
    let hooks = argv
        .iter()
        .find_map(|a| a.strip_prefix("hooks.PreToolUse="))
        .unwrap();
    let hooks: toml::Value = toml::from_str(&format!("h = {hooks}")).unwrap();
    let hooks = hooks["h"].as_array().unwrap();
    let (wt, build) = (w.path("wt"), w.path("build"));
    assert_eq!(hooks[0]["matcher"].as_str(), Some("^apply_patch$"));
    assert_eq!(
        hooks[0]["hooks"][0]["command"].as_str().unwrap(),
        format!(
            "'/k/kelpie' 'confine' '{}' '{}'",
            wt.display(),
            build.display()
        )
    );
    assert_eq!(hooks[1]["matcher"].as_str(), Some("^Bash$"));
    let guard = hooks[1]["hooks"][0]["command"].as_str().unwrap();
    assert!(guard.starts_with("'/k/kelpie' 'guard' "), "{guard}");
    assert_eq!(&argv[argv.len() - 2..], ["--", "implement #7"]);
}

#[test]
fn a_resumed_session_names_codexs_own_id_after_the_options() {
    let w = World::new();
    let mut call = w.fenced(Path::new("/k/kelpie"), Session::Resume(id(WORKER_ID)));
    call.prompt = "-carry on".into();
    let thread = Thread {
        id: "019a0000-0000-7000-8000-000000000001".into(),
        spent: Spent::default(),
    };
    let argv = strings(&call, Some(&thread), None);
    assert_eq!(argv[..2], ["exec", "resume"]);
    assert_eq!(
        &argv[argv.len() - 3..],
        ["--", "019a0000-0000-7000-8000-000000000001", "-carry on"]
    );
    assert!(!argv.iter().any(|a| a.starts_with("developer_instructions")));
}

#[test]
fn a_review_runs_no_checks_and_the_judge_no_shell() {
    let w = World::new();
    let mut review = w.call(Role::Reviewer, Session::New(id(WORKER_ID)));
    review.tools = Tools::Review;
    let argv = strings(&review, None, None);
    assert!(!argv.iter().any(|a| a == "--dangerously-bypass-hook-trust"));
    assert!(!argv.iter().any(|a| a.starts_with("hooks.")));
    assert!(!argv.windows(2).any(|w| w == ["--disable", "shell_tool"]));

    let judge = w.call(Role::Judge, Session::New(id(WORKER_ID)));
    let argv = strings(&judge, None, None);
    assert!(argv.windows(2).any(|w| w == ["--disable", "shell_tool"]));
}

#[test]
fn a_steps_skill_is_a_file_codex_is_told_to_follow() {
    let w = World::new();
    let plugin = w.path("skills/mattpocock");
    std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(plugin.join("skills/implement")).unwrap();
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name":"mattpocock"}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("skills/implement/SKILL.md"), "---\n").unwrap();
    let mut call = w.call(Role::Worker, Session::New(id(WORKER_ID)));
    call.plugin_dirs = vec![plugin.clone()];
    call.prompt = "/mattpocock:implement Implement issue #7".into();
    let prompt = strings(&call, None, None).pop().unwrap();
    let skill = plugin.join("skills/implement/SKILL.md");
    assert!(prompt.contains(skill.to_str().unwrap()), "{prompt}");
    assert!(prompt.ends_with("\n\nImplement issue #7"), "{prompt}");

    call.prompt = "/mattpocock:tdd Implement issue #7".into();
    let prompt = strings(&call, None, None).pop().unwrap();
    assert_eq!(prompt, "/mattpocock:tdd Implement issue #7");
}

#[test]
fn a_call_reads_the_login_and_writes_only_its_own_codex_home() {
    let w = World::new();
    let login = w.path("login");
    let call = w.fenced(Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    let files = Files::of(&call);
    let policy = policy(&call, &login, &files);
    for unread in ["~/.codex/**", "~/.agents/**", "~/.claude.json", "~/.ssh/**"] {
        assert!(policy.no_read.iter().any(|p| p == unread), "{unread}");
    }
    assert_eq!(
        policy
            .read
            .iter()
            .filter(|p| p.starts_with(&login))
            .collect::<Vec<_>>(),
        [&login.join("auth.json")]
    );
    assert!(!policy.write.iter().any(|p| p.starts_with(&login)));
    // The call writes only the folders Codex keeps its sessions, databases,
    // logs, helpers and locks in, so nothing Codex loads from its home.
    let home = w.path("worker/settings.codex");
    let written: Vec<_> = policy
        .write
        .iter()
        .filter_map(|p| p.strip_prefix(&home).ok())
        .collect();
    assert_eq!(
        written,
        [
            "sessions",
            "archived_sessions",
            "db",
            "log",
            "tmp",
            "thread-writer-locks",
            "installation_id"
        ]
        .map(Path::new)
    );
    for loaded in [
        "auth.json",
        "config.toml",
        "hooks.json",
        ".env",
        "AGENTS.md",
        "AGENTS.override.md",
        "rules",
        "skills",
        "models_cache.json",
    ] {
        let path = home.join(loaded);
        assert!(
            !policy.write.iter().any(|w| path.starts_with(w)),
            "{loaded}"
        );
    }
    assert!(!policy.write.contains(&w.path("worker/settings.threads")));
    assert!(policy.no_write.contains(&w.path("wt/**/.codex")));
    assert_eq!(policy.hosts, ["github.com", "api.github.com", MODEL_HOST]);

    let judge = w.call(Role::Judge, Session::New(id(WORKER_ID)));
    let unfenced = super::policy(&judge, &login, &Files::of(&judge));
    assert_eq!(unfenced.hosts, [MODEL_HOST]);
    assert!(!unfenced.write.contains(&w.path("wt")));
}

#[test]
fn every_call_runs_on_a_codex_home_of_its_own_with_the_login_linked_in() {
    let w = World::new();
    let cli = stand_in(&w, "");
    let call = w.call(Role::Judge, Session::New(id(WORKER_ID)));
    cli.prepare(&call).unwrap();
    let home = w.path("worker/settings.codex");
    let link = home.join("auth.json");
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        w.path("login/auth.json")
    );
    let _ = cli.run(&call);
    assert_eq!(
        std::fs::read_to_string(w.path("codex_home")).unwrap(),
        home.to_str().unwrap()
    );

    // A file left where the link was is linked again, never read.
    std::fs::remove_file(&link).unwrap();
    std::fs::write(&link, "{}").unwrap();
    cli.prepare(&call).unwrap();
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        w.path("login/auth.json")
    );

    // A link to a login `codex_home` no longer names is pointed at this one.
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(w.path("old-login/auth.json"), &link).unwrap();
    cli.prepare(&call).unwrap();
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        w.path("login/auth.json")
    );

    // The folders the call writes are made before it runs, since it cannot
    // make them in a home it may not write.
    for folder in ["sessions", "db", "log", "tmp"] {
        assert!(home.join(folder).is_dir(), "{folder}");
    }
}

#[test]
fn codexs_databases_and_logs_go_in_folders_the_call_may_write() {
    let w = World::new();
    let call = w.call(Role::Judge, Session::New(id(WORKER_ID)));
    let argv = strings(&call, None, None);
    let home = w.path("worker/settings.codex");
    for (key, folder) in [("sqlite_home", "db"), ("log_dir", "log")] {
        let set = format!("{key}={:?}", home.join(folder));
        assert!(argv.contains(&set), "{set} in {argv:?}");
    }
}

#[test]
fn a_turn_answers_with_its_last_message_and_its_tokens() {
    let w = World::new();
    let cli = stand_in(&w, FRESH);
    let call = w.call(Role::Worker, Session::New(id(WORKER_ID)));
    cli.prepare(&call).unwrap();
    let reply = cli.run(&call).unwrap();
    assert_eq!(reply.text, "done");
    assert_eq!(reply.session_id, id(WORKER_ID));
    assert_eq!(reply.session_cost, None);
    assert_eq!(
        reply.usage,
        Usage {
            input: 6959,
            cache_write: 0,
            cache_read: 10752,
            output: 44,
        }
    );
}

#[test]
fn a_resumed_turn_counts_only_its_own_tokens_and_reads_past_refusals() {
    let w = World::new();
    let cli = stand_in(&w, FRESH);
    let call = w.call(Role::Worker, Session::New(id(WORKER_ID)));
    cli.prepare(&call).unwrap();
    cli.run(&call).unwrap();

    let cli = stand_in(&w, RESUMED);
    let again = w.call(Role::Worker, Session::Resume(id(WORKER_ID)));
    let reply = cli.run(&again).unwrap();
    let argv = std::fs::read_to_string(w.path("argv")).unwrap();
    assert!(argv.starts_with("exec\nresume\n"), "{argv}");
    assert!(argv.contains(&format!("--\n{THREAD}\n")), "{argv}");
    // Codex reports the session's tokens so far; the turn's own are the rest.
    assert_eq!(
        reply.usage,
        Usage {
            input: 3686,
            cache_write: 0,
            cache_read: 42752,
            output: 427,
        }
    );
    // A refused patch, write and command leave the turn's answer standing.
    for refused in [
        "blocked by a hook",
        "operation not permitted",
        "gh pr merge 1",
    ] {
        assert!(reply.text.contains(refused), "{refused}: {}", reply.text);
    }
    assert!(reply.text.contains("hello.txt"), "{}", reply.text);
}

#[test]
fn a_session_resumed_as_another_is_unreadable() {
    let w = World::new();
    let cli = stand_in(&w, FRESH);
    let call = w.call(Role::Worker, Session::New(id(WORKER_ID)));
    cli.prepare(&call).unwrap();
    cli.run(&call).unwrap();
    let other = RESUMED.replace(THREAD, "01a0f7fd-0000-7000-8000-000000000000");
    let cli = stand_in(&w, &other);
    let again = w.call(Role::Worker, Session::Resume(id(WORKER_ID)));
    assert!(matches!(
        cli.run(&again),
        Err(AgentError::Unreadable(CODEX, _))
    ));
}

#[test]
fn a_usage_limit_fails_the_turn_naming_it() {
    let w = World::new();
    let cli = stand_in(&w, LIMITED);
    let call = w.call(Role::Worker, Session::New(id(WORKER_ID)));
    cli.prepare(&call).unwrap();
    let Err(AgentError::Failed(CODEX, why)) = cli.run(&call) else {
        panic!("a usage limit was not a failure");
    };
    assert!(why.contains("hit your usage limit"), "{why}");
    // Nothing started, so there is no session to resume.
    let again = w.call(Role::Worker, Session::Resume(id(WORKER_ID)));
    assert_eq!(
        cli.run(&again),
        Err(AgentError::NoSession(CODEX, id(WORKER_ID)))
    );
}

#[test]
fn what_codex_cannot_give_a_worker_fails_before_the_call() {
    let w = World::new();
    let mut call = w.fenced(Path::new("/k/kelpie"), Session::New(id(WORKER_ID)));
    call.mcp_config = Some(w.path("worker/mcp.json"));
    let cli = stand_in(&w, "");
    assert!(matches!(cli.prepare(&call), Err(AgentError::Setup(_))));
    let mut call = w.call(Role::Judge, Session::New(id(WORKER_ID)));
    call.harness = AgentHarness::ClaudeCode;
    assert!(matches!(cli.prepare(&call), Err(AgentError::Setup(_))));
}

#[test]
fn a_resume_with_no_codex_session_of_kelpies_is_no_session() {
    let w = World::new();
    let cli = stand_in(&w, "");
    let call = w.call(Role::Worker, Session::Resume(id(WORKER_ID)));
    cli.prepare(&call).unwrap();
    assert_eq!(
        cli.run(&call),
        Err(AgentError::NoSession(CODEX, id(WORKER_ID)))
    );
    let odd = w.call(Role::Worker, Session::Resume(id("../../x")));
    assert!(matches!(cli.run(&odd), Err(AgentError::Setup(_))));
}

#[test]
fn an_error_carries_only_the_end_of_a_long_output() {
    let long = format!("{}the end", "x".repeat(10_000));
    let Err(AgentError::Failed(_, why)) = parse_result(&output(1, "", &long), None) else {
        panic!("a failed exit was a success");
    };
    assert!(why.len() <= ERROR_TAIL && why.ends_with("the end"), "{why}");
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    // Under `/tmp`, and canonical, so a recorded path reads the same each time.
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let root = dir.path().canonicalize().unwrap();
        for folder in ["wt", "build", "worker", "outside", "home", "login"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        Self { _dir: dir, root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    fn call(&self, role: Role, session: Session) -> AgentCall {
        AgentCall {
            harness: AgentHarness::Codex,
            role,
            issue: 7,
            model: "gpt-6-sol".into(),
            effort: Effort::Medium,
            session,
            cwd: self.path("wt"),
            settings: self.path("worker/settings.json"),
            instructions: None,
            prompt: String::new(),
            timeout: None,
            mcp_config: None,
            plugin_dirs: Vec::new(),
            tools: Tools::Answer,
            reach: Reach::default(),
            lease: None,
        }
    }

    fn fenced(&self, kelpie: &Path, session: Session) -> AgentCall {
        worker_in(
            &self.path("wt"),
            &self.path("build"),
            kelpie,
            &self.root,
            self.call(Role::Worker, session),
        )
    }
}

// `call` as a worker's, fenced to `wt` and `build`, whose repo is `wt`'s own.
fn worker_in(wt: &Path, build: &Path, kelpie: &Path, root: &Path, call: AgentCall) -> AgentCall {
    let git = wt.join(".git");
    let profile = WorkerProfile {
        worktree: wt,
        build,
        git_common_dir: &git,
        git_dir: &git,
        branch: "kelpie/34",
        kelpie,
        kelpie_home: &root.join("kelpie"),
        repo: &root.join("repo"),
        private_names: &[],
        guard_hooks: &[],
        allowed_domains: &[],
        build_env: &BTreeMap::new(),
        preview: None,
        shep_home: &root.join("shep"),
        door: Path::new("/k/dog/lease.sock"),
    };
    AgentCall {
        role: Role::Worker,
        tools: Tools::Work,
        reach: profile.reach(),
        ..call
    }
}

const NEEDS: &str = "needs KELPIE_TOOLS=<dir> from `shep kelpie tools install`, \
                     KELPIE_BIN=<this branch's kelpie>, KELPIE_CODEX_MODEL=<model>, \
                     and KELPIE_CODEX_RECORD=<dir> for what it prints; kelpie's \
                     own Codex login is read from ~/.kelpie/codex";

fn env(name: &str) -> String {
    std::env::var(name).expect(NEEDS)
}

// The real Codex on kelpie's own login, under the real sandbox, the way
// the runner wraps it.
fn live() -> CodexCli {
    let tools = KelpieTools::at(env("KELPIE_TOOLS").into());
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let login = home.join(".kelpie/codex");
    let sandbox = Unreadable::new(
        Arc::new(SandboxRuntime::new(tools)),
        vec![format!("{}/**", login.display())],
    );
    ClaudeCli::default()
        .sandboxed(Arc::new(sandbox), home)
        .codex(login)
}

fn run_raw(codex: &CodexCli, call: &AgentCall) -> Output {
    let files = Files::of(call);
    let mut command = codex.sandboxed_command(call).unwrap();
    let outputs = [files.stdout.as_path(), files.stderr.as_path()];
    codex
        .processes
        .output_to_files(&mut command, None, &|_| {}, outputs)
        .unwrap()
}

// A worker's turn that writes one file through apply_patch, then the same
// session resumed and asked for three things kelpie refuses and what it
// wrote before. What Codex prints is recorded with the folders renamed;
// the fixtures are these recordings.
#[test]
#[ignore = "runs Codex on the ChatGPT account"]
fn record_a_turn_a_resumed_turn_and_three_refusals() {
    let kelpie = PathBuf::from(env("KELPIE_BIN"));
    let record = PathBuf::from(env("KELPIE_CODEX_RECORD"));
    let codex = live();
    let world = World::new();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(world.path("wt"))
        .status()
        .unwrap();
    let home = std::env::var("HOME").unwrap();
    let save = |name: &str, out: &Output, took: std::time::Duration| {
        let text = String::from_utf8_lossy(&out.stdout)
            .replace(world.root.to_str().unwrap(), "/tmp/kelpie-codex")
            .replace(&home, "/Users/me");
        std::fs::write(record.join(name), text).unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).replace(&home, "/Users/me");
        std::fs::write(record.join(format!("{name}.stderr")), stderr.as_bytes()).unwrap();
        eprintln!("{name}: {} ms", took.as_millis());
    };
    let worker_id = id(WORKER_ID);
    let mut first = world.fenced(&kelpie, Session::New(worker_id.clone()));
    first.model = env("KELPIE_CODEX_MODEL").as_str().into();
    first.effort = Effort::Low;
    first.prompt = "Use the apply_patch tool to add the file hello.txt holding the \
                    line hi. Then reply with the single word done."
        .into();
    codex.prepare(&first).unwrap();
    let started = Instant::now();
    let out = run_raw(&codex, &first);
    save("codex-exec-fresh.jsonl", &out, started.elapsed());
    let turn = parse_result(&out, None).unwrap();
    eprintln!("{turn:?}");
    let path = Files::of(&first).thread(&worker_id);
    std::fs::write(
        &path,
        serde_json::to_string(&Thread {
            id: turn.thread,
            spent: turn.spent,
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(world.path("wt/hello.txt")).unwrap(),
        "hi\n"
    );

    first.session = Session::Resume(worker_id.clone());
    first.prompt = format!(
        "Three checks, then stop. First, use the apply_patch tool to add the \
         file .codex/config.toml holding one empty line. Second, run \
         `echo hi > {}/x` in the shell. Third, run `gh pr merge 1` in the \
         shell. Then say in one line per step what happened, and name the \
         file you added in your first turn.",
        world.path("outside").display()
    );
    let started = Instant::now();
    let out = run_raw(&codex, &first);
    save("codex-exec-resumed.jsonl", &out, started.elapsed());
    let thread = thread(&Files::of(&first), &worker_id).unwrap();
    let again = parse_result(&out, thread.as_ref()).unwrap();
    eprintln!("{again:?}");
    assert!(again.text.contains("hello.txt"), "{again:?}");
    assert!(!world.path("wt/.codex/config.toml").exists());
    assert!(!world.path("outside/x").exists());
    eprintln!("kept: {}", world.root.display());
    std::mem::forget(world);
}

// One run of a worker's turn on a real issue, for the measurement against
// Claude Code. It records Codex's output outside the repo, since the
// issue's code is private, and prints the wall time and tokens.
#[test]
#[ignore = "runs Codex on the ChatGPT account"]
fn measure_a_codex_worker_on_a_real_issue() {
    let (kelpie, repo) = (
        PathBuf::from(env("KELPIE_BIN")),
        PathBuf::from(env("KELPIE_ISSUE_REPO")),
    );
    let record = PathBuf::from(env("KELPIE_CODEX_RECORD"));
    let codex = live();
    let world = World::new();
    let mut call = worker_in(
        &repo,
        &world.path("build"),
        &kelpie,
        &world.root,
        world.call(Role::Worker, Session::New(id(WORKER_ID))),
    );
    call.model = env("KELPIE_CODEX_MODEL").as_str().into();
    call.cwd.clone_from(&repo);
    call.prompt = std::fs::read_to_string(env("KELPIE_ISSUE")).unwrap();
    codex.prepare(&call).unwrap();
    let started = Instant::now();
    let out = run_raw(&codex, &call);
    let took = started.elapsed();
    std::fs::write(record.join("issue.jsonl"), &out.stdout).unwrap();
    std::fs::write(record.join("issue.stderr"), &out.stderr).unwrap();
    eprintln!("secs: {}", took.as_secs());
    eprintln!("{:#?}", parse_result(&out, None));
}

// The same issue for Claude Code, as a worker under the same sandbox, for
// the measurement's other side.
#[test]
#[ignore = "runs Claude Code on the Claude account"]
fn measure_a_claude_code_worker_on_the_same_issue() {
    let (kelpie, repo) = (
        PathBuf::from(env("KELPIE_BIN")),
        PathBuf::from(env("KELPIE_ISSUE_REPO")),
    );
    let tools = KelpieTools::at(env("KELPIE_TOOLS").into());
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let claude = ClaudeCli::default().sandboxed(Arc::new(SandboxRuntime::new(tools)), home);
    let world = World::new();
    let mut call = worker_in(
        &repo,
        &world.path("build"),
        &kelpie,
        &world.root,
        AgentCall {
            harness: AgentHarness::ClaudeCode,
            model: env("KELPIE_CLAUDE_MODEL").as_str().into(),
            ..world.call(Role::Worker, Session::New(id(WORKER_ID)))
        },
    );
    call.cwd.clone_from(&repo);
    call.prompt = std::fs::read_to_string(env("KELPIE_ISSUE")).unwrap();
    claude.prepare(&call).unwrap();
    let started = Instant::now();
    let reply = claude.run(&call);
    eprintln!("secs: {}", started.elapsed().as_secs());
    eprintln!("{reply:#?}");
    eprintln!("kept: {}", world.root.display());
    std::mem::forget(world);
}
